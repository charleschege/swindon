use std::sync::Arc;

use futures::Async;
use tk_http::server::{Error, Codec, RecvMode};
use tk_http::server as http;

use crate::config::Config;
use crate::incoming::{Request, Reply, Encoder, IntoContext, Debug};


pub struct QuickReply<F> {
    inner: Option<(F, Arc<Config>, Debug)>,
}


pub fn reply<F, C, S: 'static>(ctx: C, f: F)
    -> Request<S>
    where F: FnOnce(Encoder<S>) -> Reply<S> + 'static,
          C: IntoContext,
{
    let (cfg, debug) = ctx.into_context();
    Box::new(QuickReply {
        inner: Some((f, cfg, debug)),
    })
}

impl<F, S> Codec<S> for QuickReply<F>
    where F: FnOnce(Encoder<S>) -> Reply<S>,
{
    type ResponseFuture = Reply<S>;
    fn recv_mode(&mut self) -> RecvMode {
        RecvMode::buffered_upfront(0)
    }
    fn data_received(&mut self, data: &[u8], end: bool)
        -> Result<Async<usize>, Error>
    {
        assert!(end);
        assert!(data.len() == 0);
        Ok(Async::Ready(0))
    }
    fn start_response(&mut self, e: http::Encoder<S>) -> Reply<S> {
        let (func, config, debug) = self.inner.take()
            .expect("start response called once");
        func(Encoder::new(e, (config, debug)))
    }
}
