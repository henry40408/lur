//! Server mode (spec §3): load `app.lua` to collect route registrations, then
//! dispatch incoming requests to the matching Lua handler.
//!
//! This module owns the host-side route table and the request/response bridge.
//! The network layer (hyper) is a thin adapter on top of [`Server::dispatch`].

use std::convert::Infallible;
use std::net::SocketAddr;
use std::rc::Rc;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response as HyperResponse};
use hyper_util::rt::TokioIo;
use mlua::{Function, MultiValue, Value};
use tokio::net::TcpListener;
use tokio::task::LocalSet;

use crate::capabilities::serve::Registry;
use crate::runtime::{RunError, Runtime, RuntimeConfig};

/// A loaded server application: a warmed-up VM plus its host-side route table.
pub struct Server {
    runtime: Runtime,
    routes: Vec<Route>,
}

/// One resolved route: a method/path pair bound to its handler closure.
struct Route {
    method: String,
    path: String,
    handler: Function,
}

/// The host-side view of a handler's reply (spec §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status code (defaults to 200 when the handler omits it).
    pub status: u16,
    /// Response body as raw bytes (defaults to empty).
    pub body: Vec<u8>,
}

impl Server {
    /// Load `app.lua`: build a server-mode VM, run the script once to collect
    /// its `lur.serve.http` registrations, and capture them as the route table.
    pub fn load(source: &str, config: RuntimeConfig) -> Result<Self, RunError> {
        let registry = Registry::default();
        let runtime = Runtime::with_serve(config, registry.clone())?;
        runtime.run(source)?;

        let routes = registry
            .take()
            .into_iter()
            .map(|r| Route {
                method: r.method,
                path: r.path,
                handler: r.handler,
            })
            .collect();

        Ok(Self { runtime, routes })
    }

    /// Dispatch one request to its handler, returning the handler's response or
    /// an automatic 404 when no route matches. Synchronous wrapper that drives
    /// the async handler on the runtime's executor.
    pub fn dispatch(&self, method: &str, path: &str, body: &[u8]) -> Result<Response, RunError> {
        self.runtime
            .block_on(self.dispatch_async(method, path, body))
    }

    /// Bind `addr` and serve requests forever. Single VM, single thread: each
    /// connection is driven on a `LocalSet` so the `!Send` VM never crosses
    /// threads. (The VM pool for concurrency lands in a later slice.)
    pub fn run(self, addr: SocketAddr) -> std::io::Result<()> {
        let server = Rc::new(self);
        let driver = server.clone();
        driver.runtime.block_on(async move {
            let listener = TcpListener::bind(addr).await?;
            eprintln!("lur: listening on http://{addr}");
            let local = LocalSet::new();
            let outcome: std::io::Result<()> = local
                .run_until(async move {
                    loop {
                        let (stream, _) = listener.accept().await?;
                        let io = TokioIo::new(stream);
                        let server = server.clone();
                        tokio::task::spawn_local(async move {
                            let service = service_fn(move |req| {
                                let server = server.clone();
                                async move { server.handle(req).await }
                            });
                            if let Err(e) =
                                http1::Builder::new().serve_connection(io, service).await
                            {
                                eprintln!("lur: connection error: {e}");
                            }
                        });
                    }
                })
                .await;
            outcome
        })
    }

    /// Adapt one hyper request through [`Self::dispatch_async`]. A handler error
    /// becomes a 500 and is logged — it must never bring the server down (§8).
    async fn handle(
        &self,
        req: Request<Incoming>,
    ) -> Result<HyperResponse<Full<Bytes>>, Infallible> {
        let method = req.method().as_str().to_owned();
        let path = req.uri().path().to_owned();
        let body = req
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes())
            .unwrap_or_default();

        let response = match self.dispatch_async(&method, &path, &body).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("lur: handler error: {e}");
                Response {
                    status: 500,
                    body: b"Internal Server Error".to_vec(),
                }
            }
        };

        let built = HyperResponse::builder()
            .status(response.status)
            .body(Full::new(Bytes::from(response.body)));
        Ok(built.unwrap_or_else(|_| HyperResponse::new(Full::new(Bytes::new()))))
    }

    /// The async core of [`Self::dispatch`]; the network loop awaits this
    /// directly rather than blocking.
    async fn dispatch_async(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<Response, RunError> {
        let Some(handler) = self.resolve(method, path) else {
            return Ok(Response {
                status: 404,
                body: b"Not Found".to_vec(),
            });
        };

        let lua = self.runtime.lua();
        let req = lua.create_table().map_err(RunError::Script)?;
        req.set("method", method.to_uppercase())
            .map_err(RunError::Script)?;
        req.set("path", path).map_err(RunError::Script)?;
        req.set("body", lua.create_string(body).map_err(RunError::Script)?)
            .map_err(RunError::Script)?;

        let values = handler
            .call_async::<MultiValue>(req)
            .await
            .map_err(RunError::Script)?;
        response_from(values)
    }

    /// Find the handler for `(method, path)`: exact path match, method matched
    /// case-insensitively with `ANY` as the wildcard.
    fn resolve(&self, method: &str, path: &str) -> Option<&Function> {
        let method = method.to_uppercase();
        self.routes
            .iter()
            .find(|r| r.path == path && (r.method == "ANY" || r.method == method))
            .map(|r| &r.handler)
    }
}

/// Turn a handler's return values into a [`Response`]: the first value must be a
/// table; `status` defaults to 200 and `body` to empty.
fn response_from(values: MultiValue) -> Result<Response, RunError> {
    let table = match values.into_iter().next() {
        Some(Value::Table(t)) => t,
        _ => {
            return Err(RunError::Script(mlua::Error::RuntimeError(
                "handler must return a table { status, body, headers }".into(),
            )));
        }
    };

    let status = table
        .get::<Option<i64>>("status")
        .map_err(RunError::Script)?
        .unwrap_or(200) as u16;
    let body = table
        .get::<Option<mlua::String>>("body")
        .map_err(RunError::Script)?
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_default();

    Ok(Response { status, body })
}
