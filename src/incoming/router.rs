use std::net::SocketAddr;
use std::sync::Arc;

use tokio_core::reactor::Handle;
use tk_http::Status;
use tk_http::server::{Dispatcher, Error as ServerError, Head};

use crate::runtime::Runtime;
use crate::incoming::{Request, Debug, Input, Transport};
use crate::routing::{parse_host, route};
use crate::default_error_page::serve_error_page;
use crate::request_id;

use crate::metrics::{Counter};
use crate::logging;
use crate::request_id::RequestId;


lazy_static! {
    pub static ref REQUESTS: Counter = Counter::new();
}


pub struct Router {
    addr: SocketAddr,
    runtime: Arc<Runtime>,
    handle: Handle,
}

pub enum Error {
    Page(Status, Debug),
    Fallback(ServerError),
}

impl Router {
    pub fn new(addr: SocketAddr, runtime: Arc<Runtime>, handle: Handle)
        -> Router
    {
        Router {
            addr: addr,
            runtime: runtime,
            handle: handle,
        }
    }
}

impl Router {

    fn start_request<S: Transport>(&mut self, headers: &Head,
        request_id: RequestId)
        -> Result<Request<S>, Error>
    {
        use self::Error::*;

        REQUESTS.incr(1);
        // Keep config same while processing a single request
        let cfg = self.runtime.config.get();
        let mut debug = Debug::new(headers, request_id, &cfg);

        // No path means either CONNECT host, or OPTIONS *
        // in both cases we use root route for the domain to make decision
        //
        // TODO(tailhook) strip ?, #, ; from path
        let path = headers.path().unwrap_or("/");

        let parsed_host = headers.host().map(parse_host);

        /*
        if let Some((auth, pref, suf)) = authorization_route {
            debug.set_authorizer(auth);
            let mut inp = AuthInput {
                addr: self.addr,
                runtime: &self.runtime,
                config: &cfg,
                debug: debug,
                headers: headers,
                prefix: pref,
                suffix: suf,
                handle: &self.handle,
                request_id: request_id,
            };
        };
        */

        let matched_route = parsed_host
            .and_then(|host| route(host, &path, &cfg.routing));

        let (route, pref, suf) = if let Some((route, p, s)) = matched_route {
            (route, p, s)
        } else {
            return Err(Page(Status::NotFound, debug));
        };
        debug.set_route(route);

        let mut inp = Input {
            addr: self.addr,
            runtime: &self.runtime,
            config: &cfg,
            debug: debug,
            headers: headers,
            prefix: pref,
            suffix: suf,
            handle: &self.handle,
            request_id: request_id,
        };

        match route.authorizer.check(&mut inp) {
            Ok(true) => {}
            Ok(false) => {
                return Err(Page(Status::Forbidden, inp.debug));
            }
            Err(e) => return Err(Fallback(e)),
        }

        route.handler.serve(inp).map_err(Fallback)
    }
}

impl<S: Transport> Dispatcher<S> for Router {
    type Codec = Request<S>;
    fn headers_received(&mut self, headers: &Head)
        -> Result<Self::Codec, ServerError>
    {
        let request_id = request_id::new();
        match self.start_request(headers, request_id) {
            Ok(x) => {
                // TODO(tailhook) request is not done yet, just a fake
                logging::log(&self.runtime,
                    logging::http::FakePage {
                        request: logging::http::EarlyRequest {
                            addr: self.addr,
                            head: headers,
                            request_id: request_id,
                        },
                        response: logging::http::FakeResponse {
                        },
                    });
                Ok(x)
            }
            Err(Error::Page(status, debug)) => {
                logging::log(&self.runtime,
                    logging::http::EarlyError {
                        request: logging::http::EarlyRequest {
                            addr: self.addr,
                            head: headers,
                            request_id: request_id,
                        },
                        response: logging::http::EarlyResponse {
                            status: status.into(),
                        }
                    });
                Ok(serve_error_page(status,
                    (self.runtime.config.get(), debug)))
            }
            // Maybe return bad request?
            Err(Error::Fallback(e)) => Err(e),
        }
    }
}
