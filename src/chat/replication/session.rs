use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Instant, Duration};

use async_slot as slot;
use tokio_core::reactor::Handle;
use tokio_core::reactor::Interval;
use futures::{self, Future, Stream, Async, AsyncSink};
use futures::sync::mpsc::{unbounded, UnboundedSender};
use futures::sync::oneshot::{channel as oneshot, Sender};
use tk_http::websocket::Packet;
use serde_json::to_string as json_encode;
use ns_router::{Router};
use void::Void;

use crate::intern::SessionPoolName;
use crate::runtime::{Runtime, ServerId};
use crate::config::listen::Listen;
use crate::config::{Replication};
use crate::chat::processor::Processor;

use super::{ReplAction, RemoteAction, IncomingChannel, OutgoingChannel};
use super::action::Message;
use super::spawn::{listen, connect};


pub struct ReplicationSession {
    pub remote_sender: RemoteSender,
    tx: IncomingChannel,
    listener_channel: slot::Sender<Listen>,
    reconnect_shutter: Option<Sender<()>>,
}

struct Watcher {
    peers: HashMap<String, State>,
    links: HashMap<ServerId, OutgoingChannel>,
    tx: IncomingChannel,
    processor: Processor,
    server_id: ServerId,
    resolver: Router,
    handle: Handle,
}

#[derive(Debug)]
enum State {
    /// Connect started for outbound connection.
    /// ServerId is still unknown.
    Connecting(Instant),
    /// Either outbound or inbound live connection.
    /// ServerId is known.
    /// Also for outbound connection peer name is known.
    Connected(ServerId),
}

#[derive(Clone)]
pub struct RemoteSender {
    queue: UnboundedSender<ReplAction>,
}

#[derive(Clone)]
pub struct RemotePool {
    pool: SessionPoolName,
    queue: UnboundedSender<ReplAction>,
}

impl ReplicationSession {
    pub fn new(processor: Processor, resolver: &Router, handle: &Handle,
        server_id: &ServerId, cfg: &Arc<Replication>)
        -> ReplicationSession
    {
        let (tx, rx) = unbounded();
        let watcher = Watcher {
            processor: processor,
            peers: HashMap::new(),
            links: HashMap::new(),
            tx: tx.clone(),
            server_id: server_id.clone(),
            handle: handle.clone(),
            resolver: resolver.clone(),
        };
        handle.spawn(rx.forward(watcher)
            .map(|_| debug!("rx stopped"))
            .map_err(|_| debug!("watcher error")));

        let (listen_tx, listen_rx) = slot::channel();
        listen(
            resolver.subscribe_stream(
                listen_rx.map_err(|()| -> Void { unreachable!() }), 80),
            tx.clone(), server_id, cfg, handle);

        ReplicationSession {
            tx: tx.clone(),
            remote_sender: RemoteSender { queue: tx },
            listener_channel: listen_tx,
            reconnect_shutter: None,
        }
    }

    pub fn update(&mut self, cfg: &Arc<Replication>,
        handle: &Handle, _runtime: &Arc<Runtime>)
    {
        self.listener_channel.swap(cfg.listen.clone())
            .map_err(|_| error!("Can't update replication listener")).ok();
        // stop reconnecting
        if let Some(tx) = self.reconnect_shutter.take() {
            tx.send(()).ok();
        }
        let (tx, shutter) = oneshot();
        self.reconnect_shutter = Some(tx);
        let s = cfg.clone();
        let tx = self.tx.clone();

        use futures::Sink; // conflicting with tx.send in RemotePool
        handle.spawn(Interval::new(Duration::new(1, 0), &handle)
            .expect("interval created")
            .map(move |_| ReplAction::Reconnect(s.clone()))
            .map_err(|e| error!("Interval error: {}", e))
            .forward(tx.sink_map_err(|_| error!("sink error"))).map(|_| ())
            .select(shutter.map_err(|_| unreachable!()))
            .map(|_| info!("Reconnector stopped"))
            .map_err(|_| info!("Reconnector stopped"))
        );
    }

}

impl Watcher {

    fn attach(&mut self, tx: OutgoingChannel,
        server_id: ServerId, peer: Option<String>)
    {
        if let Some(peer) = peer {
            self.peers.insert(peer, State::Connected(server_id));
        }
        self.links.insert(server_id, tx);
    }

    fn local_send(&self, msg: Message) {
        use super::RemoteAction::*;
        let Message(pool, action) = msg;
        match action {
            Subscribe { server_id, .. } |
            Unsubscribe { server_id, .. } |
            Attach { server_id, .. } |
            Detach { server_id, .. } if self.server_id != server_id =>
            {
                debug!("Skipping remote action with non-local cid");
                return;
            }
            _ => {}
        }
        self.processor.send(&pool, action.into());
    }

    fn remote_send(&mut self, msg: Message) {
        if let Ok(data) = json_encode(&msg) {
            // TODO: use HashMap::retain() when in stable
            let to_delete = self.links.iter().filter_map(|(remote, tx)| {
                tx.unbounded_send(Packet::Text(data.clone())).err()
                .map(|_| remote.clone())    // XXX
            }).collect::<Vec<_>>();         // XXX
            for remote in to_delete {
                self.links.remove(&remote);
            }
        } else {
            debug!("error encoding message: {:?}", msg);
        }
    }

    fn reconnect(&mut self, settings: &Arc<Replication>)
    {
        use self::State::*;

        let now = Instant::now();
        let timeout = now + settings.reconnect_timeout;

        // TODO: use HashMap::retain() when in stable
        let to_delete = self.peers.keys()
            .filter(|p| !settings.peers.contains(p))
            .map(|p| p.clone()).collect::<Vec<_>>();  // XXX
        for peer in to_delete {
            match self.peers.remove(&peer) {
                Some(Connected(server_id)) => {
                    self.links.remove(&server_id);
                }
                _ => continue,
            }
        };

        for peer in &settings.peers {
            match self.peers.get(peer) {
                Some(&Connected(ref server_id)) => {
                    if let Some(_) = self.links.get(server_id) {
                        continue
                    }
                }
                Some(&Connecting(ref timeout)) => {
                    if timeout >= &now {
                        continue
                    }
                }
                _ => {}
            };
            self.peers.insert(peer.clone(), Connecting(timeout));
            connect(peer, self.tx.clone(), &self.server_id,
                timeout, &self.handle, &self.resolver);
        }
    }
}

impl RemoteSender {
    pub fn pool(&self, name: &SessionPoolName) -> RemotePool {
        RemotePool {
            pool: name.clone(),
            queue: self.queue.clone(),
        }
    }
}

impl RemotePool {

    pub fn send(&self, action: RemoteAction) {
        let msg = Message(self.pool.clone(), action);
        self.queue.unbounded_send(ReplAction::Outgoing(msg))
            .map_err(|e| error!("Error sending event: {}", e)).ok();
    }
}

impl futures::Sink for Watcher {
    type SinkItem = ReplAction;
    type SinkError = ();

    fn start_send(&mut self, item: Self::SinkItem)
        -> futures::StartSend<Self::SinkItem, Self::SinkError>
    {
        match item {
            ReplAction::Attach { tx, server_id, peer } => {
                if let Some(ref peer) = peer {
                    debug!("Got connected to {}: {}", peer, server_id);
                } else {
                    debug!("Got connection from: {}", server_id);
                }
                self.attach(tx, server_id, peer);
            }
            ReplAction::Incoming(msg) => {
                debug!("Received incoming message: {:?}", msg);
                self.local_send(msg);
            }
            ReplAction::Outgoing(msg) => {
                debug!("Sending outgoing message: {:?}", msg);
                self.remote_send(msg);
            }
            ReplAction::Reconnect(ref cfg) => {
                self.reconnect(cfg);
            }
        }
        Ok(AsyncSink::Ready)
    }
    fn poll_complete(&mut self) -> futures::Poll<(), Self::SinkError>
    {
        Ok(Async::Ready(()))
    }
}
