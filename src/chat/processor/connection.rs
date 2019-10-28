use std::sync::Arc;
use std::collections::{HashSet, HashMap};

use serde_json::Value as Json;

use crate::chat::{Cid, CloseReason, ConnectionSender};
use crate::intern::{Topic, SessionId, Lattice as Namespace, LatticeKey};
use super::{ConnectionMessage};
use super::lattice;


pub struct NewConnection {
    pub cid: Cid,
    pub topics: HashSet<Topic>,
    pub lattices: HashSet<Namespace>,
    pub users_lattice: HashSet<SessionId>,
    pub message_buffer: Vec<(Topic, Arc<Json>)>,
    pub channel: ConnectionSender,
}


pub struct Connection {
    pub cid: Cid,
    pub session_id: SessionId,
    pub topics: HashSet<Topic>,
    pub lattices: HashSet<Namespace>,
    pub users_lattice: bool,
    pub channel: ConnectionSender,
}

impl NewConnection {
    pub fn new(conn_id: Cid, channel: ConnectionSender)
        -> NewConnection
    {
        NewConnection {
            cid: conn_id,
            topics: HashSet::new(),
            lattices: HashSet::new(),
            users_lattice: HashSet::new(),
            message_buffer: Vec::new(),
            channel: channel,
        }
    }
    pub fn associate(self, session_id: SessionId)
        -> (Connection, HashSet<SessionId>)
    {
        let mut conn = Connection {
            cid: self.cid,
            session_id: session_id,
            topics: self.topics,
            lattices: self.lattices,
            users_lattice: self.users_lattice.len() > 0,
            channel: self.channel,
        };
        for (t, m) in self.message_buffer {
            conn.message(t, m);
        }
        return (conn, self.users_lattice);
    }
    pub fn message(&mut self, topic: Topic, data: Arc<Json>) {
        self.message_buffer.push((topic, data));
    }
    pub fn stop(&mut self, reason: CloseReason) {
        self.channel.send(ConnectionMessage::StopSock(reason));
    }
}

impl Connection {

    pub fn message(&mut self, topic: Topic, data: Arc<Json>) {
        self.channel.send(ConnectionMessage::Publish(topic, data));
    }

    pub fn lattice(&mut self, namespace: &Namespace,
        update: &Arc<HashMap<LatticeKey, lattice::Values>>)
    {
        let msg = ConnectionMessage::Lattice(
            namespace.clone(), update.clone());
        self.channel.send(msg);
    }
    pub fn stop(&mut self, reason: CloseReason) {
        self.channel.send(ConnectionMessage::StopSock(reason));
    }
}
