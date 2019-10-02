use std::sync::Arc;

use futures::{Async, Future};
use futures::stream::{Stream};
use futures::sink::{Sink};
use futures::future::{Either};
use tk_http::Status;
use tk_http::server::{Error, Codec, RecvMode};
use tk_http::server as http;
use tk_http::websocket::{self, ServerCodec as WebsocketCodec, Packet, Accept};
use tk_bufstream::{ReadBuf, WriteBuf};
use futures::future::{ok};
use futures::sync::mpsc::{UnboundedReceiver as Receiver};
use tokio_core::reactor::Handle;
use tokio_io::{AsyncRead, AsyncWrite};
use serde_json::{to_string as json_encode, Value as Json};

use crate::chat::ConnectionMessage::{Hello, FatalError};
use crate::chat::MessageError::HttpError;
use crate::chat::{self, Cid, ConnectionMessage, ConnectionSender};
use crate::chat::{json_err, good_status};
use crate::chat::tangle_auth::{SwindonAuth, TangleAuth};
use crate::config::chat::{Chat};
use crate::default_error_page::serve_error_page;
use crate::incoming::{Context, IntoContext};
use crate::incoming::{Request, Input, Reply, Encoder, Transport};
use crate::runtime::Runtime;

struct WebsockReply {
    cid: Cid,
    handle: Handle,
    runtime: Arc<Runtime>,
    settings: Arc<Chat>,
    reply_data: Option<ReplyData>,
    channel: Option<(ConnectionSender, Receiver<ConnectionMessage>)>,
}

struct ReplyData {
    context: Context,
    accept: Accept,
    proto: Option<&'static str>,
}


impl<S: AsyncRead + AsyncWrite + 'static> Codec<S> for WebsockReply {
    type ResponseFuture = Reply<S>;
    fn recv_mode(&mut self) -> RecvMode {
        RecvMode::hijack()
    }
    fn data_received(&mut self, _data: &[u8], _end: bool)
        -> Result<Async<usize>, Error>
    {
        unreachable!();
    }
    fn start_response(&mut self, e: http::Encoder<S>) -> Reply<S> {
        let ReplyData { context, accept, proto } = self.reply_data.take()
            .expect("start response called only once");
        let mut e = Encoder::new(e, context);
        // We always allow websocket, and send error as shutdown message
        // in case there is one.
        e.status(Status::SwitchingProtocol);
        e.add_header("Connection", "upgrade");
        e.add_header("Upgrade", "websocket");
        e.format_header("Sec-Websocket-Accept", &accept);
        if let Some(proto) = proto {
            e.add_header("Sec-Websocket-Protocol", proto);
        }
        e.done_headers();
        Box::new(ok(e.done()))
    }
    fn hijack(&mut self, write_buf: WriteBuf<S>, read_buf: ReadBuf<S>) {
        let inp = read_buf.framed(WebsocketCodec);
        let out = write_buf.framed(WebsocketCodec);

        // TODO(tailhook) don't create config on every websocket
        let cfg = websocket::Config::new()
            // TODO(tailhook) change defaults
            .done();
        let pool_settings = self.runtime.config
            .get().session_pools.get(&self.settings.session_pool)
            // TODO(tailhook) may this unwrap crash?
            //                return error code in this case
            .unwrap().clone();
        let processor = self.runtime.session_pools.processor
            // TODO(tailhook) this doesn't check that pool is created
            .pool(&self.settings.session_pool);
        let remote = self.runtime.session_pools.remote_sender
            .pool(&self.settings.session_pool);
        let h1 = self.handle.clone();
        let h2 = self.handle.clone();
        let r1 = self.runtime.clone();
        let s1 = self.settings.clone();
        let cid = self.cid;

        let (tx, rx) = self.channel.take()
            .expect("hijack called only once");
        let log_err_io = |e| debug!("closing websocket closed: {}", e);
        let log_err_sock = |e| debug!("closing websocket closed: {}", e);

        self.handle.spawn(rx.into_future()
            .then(move |result| match result {
                Ok((Some(Hello(session_id, data)), rx)) => {
                    // Cache formatted auth
                    let auth =
                        if s1.use_tangle_auth() {
                            Arc::new(format!("{}", TangleAuth(&session_id)))
                        } else {
                            Arc::new(format!("{}", SwindonAuth(&session_id)))
                        };
                    Either::A(
                        out.send(Packet::Text(
                            json_encode(&Hello(session_id.clone(), data))
                            .expect("every message can be encoded")))
                        .map_err(|e| info!("error sending userinfo: {:?}", e))
                        .and_then(move |out| {
                            let rx = rx.map(|x| {
                                chat::FRAMES_SENT.incr(1);
                                Packet::Text(json_encode(&x)
                                    .expect("any data can be serialized"))
                            }).map_err(|_| -> &str {
                                // There shouldn't be a real-life case for
                                // this.  But in case session-pool has been
                                // removed from the config and connection
                                // closes, it might probably happen, we don't
                                // care too much of that.
                                error!("outbound channel unexpectedly closed");
                                "outbound channel unexpectedly closed"
                            });
                            chat::CONNECTS.incr(1);
                            chat::CONNECTIONS.incr(1);
                            websocket::Loop::server(out, inp, rx,
                                chat::Dispatcher {
                                    cid: cid,
                                    session_id: session_id,
                                    auth: auth,
                                    handle: h1,
                                    pool_settings: pool_settings.clone(),
                                    processor: processor,
                                    remote: remote,
                                    runtime: r1,
                                    settings: s1,
                                    channel: tx,
                                }, &cfg, &h2)
                            .map_err(|e| debug!("websocket closed: {}", e))
                        }))
                }
                Ok((Some(FatalError(ref err)), _)) => {
                    let (code, data) = match *err {
                        HttpError(s, ref data) if good_status(s) => {
                            (s.code() + 4000,
                             data.clone().unwrap_or(Json::Null))
                        }
                        _ => (4500, Json::Null),
                    };
                    Either::B(Either::A(
                        // TODO(tailhook) optimize json
                        out.send(Packet::Text(json_encode(&Json::Array(vec![
                            "fatal_error".into(),
                            json_err(err),
                            data,
                        ])).expect("can always serialize error")))
                        .map_err(log_err_io)
                        .and_then(move |out| {
                            websocket::Loop::<_, _, _>::closing(out, inp,
                                code,
                                "backend_error",
                                &cfg, &h1)
                            .map_err(log_err_sock)
                        })))
                }
                Ok((msg, _)) => {
                    panic!("Received {:?} instead of Hello", msg);
                }
                Err(_) => {
                    error!("Aborted handshake because pool closed");
                    Either::B(Either::B(
                        // TODO(tailhook) optimize json
                        out.send(Packet::Text(json_encode(&Json::Array(vec![
                            "fatal_error".into(),
                            json!({
                                "error_kind": "pool_closed",
                            }),
                            Json::Null,
                        ])).expect("can always serialize")))
                        .map_err(log_err_io)
                        .and_then(move |out| {
                            websocket::Loop::<_, _, _>::closing(out, inp,
                                    1011, "", //
                                    &cfg, &h2)
                            .map_err(log_err_sock)
                        })))
                }
            }));
    }
}

fn choose_proto(h: &http::WebsocketHandshake, settings: &Arc<Chat>)
    -> Result<Option<&'static str>, ()>
{
    if h.protocols.len() == 0 {
        if settings.allow_empty_subprotocol() {
            Ok(None)
        } else {
            Err(())
        }
    } else if h.protocols.iter().any(|x| &x[..] == "v1.swindon-lattice+json") {
        return Ok(Some("v1.swindon-lattice+json"));
    } else {
        return Ok(None);
    }
}

pub fn serve<S: Transport>(settings: &Arc<Chat>, inp: Input)
    -> Result<Request<S>, Error>
{
    match inp.headers.get_websocket_upgrade() {
        Ok(Some(ws)) => {
            if let Ok(proto) = choose_proto(&ws, settings) {
                let (tx, rx) = ConnectionSender::new();
                let cid = Cid::new();
                chat::start_authorize(&inp, cid, settings, tx.clone());
                Ok(Box::new(WebsockReply {
                    cid: cid,
                    handle: inp.handle.clone(),
                    settings: settings.clone(),
                    runtime: inp.runtime.clone(),
                    reply_data: Some(ReplyData {
                        context: inp.into_context(),
                        accept: ws.accept,
                        proto: proto,
                    }),
                    channel: Some((tx, rx)),
                }))
            } else {
                Ok(serve_error_page(Status::BadRequest, inp))
            }
        }
        Ok(None) => {
            if let Some(ref hname) = settings.http_route {
                if let Some(handler) = inp.config.handlers.get(hname) {
                    handler.serve(inp)
                } else {
                    warn!("No such handler for `http-route`: {:?}", hname);
                    Ok(serve_error_page(Status::NotFound, inp))
                }
            } else {
                Ok(serve_error_page(Status::NotFound, inp))
            }
        }
        Err(()) => {
            Ok(serve_error_page(Status::BadRequest, inp))
        }
    }
}

