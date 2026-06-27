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
    router: Router,
}

/// Decoded path parameters: `(name, raw-byte value)` pairs.
type Params = Vec<(String, Vec<u8>)>;

/// One parsed path segment of a route pattern.
enum Seg {
    /// A literal segment that must match exactly.
    Static(String),
    /// A `:name` segment that matches any one segment and binds it.
    Param(String),
}

/// A compiled route: method, parsed pattern, and the handler closure.
struct Route {
    method: String,
    pattern: Vec<Seg>,
    handler: Function,
}

/// The host-side route table: resolves `(method, path)` to a handler, applying
/// static-beats-dynamic precedence independent of registration order.
struct Router {
    routes: Vec<Route>,
}

/// A request as seen by the host before it crosses into Lua. Path is still
/// percent-encoded; query is the raw string without the leading `?`.
#[derive(Debug, Default, Clone)]
pub struct RawRequest {
    /// HTTP method (case-insensitive; normalized to upper case for the handler).
    pub method: String,
    /// Request path, percent-encoded (`/users/42`).
    pub path: String,
    /// Raw query string without the `?` (`a=1&b=2`); empty if none.
    pub query: String,
    /// Header name/value pairs in arrival order (names case-insensitive).
    pub headers: Vec<(String, String)>,
    /// Full request body as raw bytes.
    pub body: Vec<u8>,
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
    /// its `lur.serve.http` registrations, and compile them into the router
    /// (which rejects a duplicate `(method, path)` at load time).
    pub fn load(source: &str, config: RuntimeConfig) -> Result<Self, RunError> {
        let registry = Registry::default();
        let runtime = Runtime::with_serve(config, registry.clone())?;
        runtime.run(source)?;

        let router = Router::build(registry.take())?;
        Ok(Self { runtime, router })
    }

    /// Dispatch a bare `(method, path, body)` request — query and headers empty.
    /// Convenience wrapper over [`Self::dispatch_raw`].
    pub fn dispatch(&self, method: &str, path: &str, body: &[u8]) -> Result<Response, RunError> {
        self.dispatch_raw(&RawRequest {
            method: method.to_owned(),
            path: path.to_owned(),
            body: body.to_vec(),
            ..Default::default()
        })
    }

    /// Dispatch a fully-described request, driving the async handler on the
    /// runtime's executor (synchronous wrapper).
    pub fn dispatch_raw(&self, req: &RawRequest) -> Result<Response, RunError> {
        self.runtime.block_on(self.dispatch_async(req))
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
        let query = req.uri().query().unwrap_or("").to_owned();
        let headers = req
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        let body = req
            .into_body()
            .collect()
            .await
            .map(|c| c.to_bytes().to_vec())
            .unwrap_or_default();

        let raw = RawRequest {
            method,
            path,
            query,
            headers,
            body,
        };
        let response = match self.dispatch_async(&raw).await {
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

    /// The async core of [`Self::dispatch_raw`]; the network loop awaits this
    /// directly rather than blocking.
    async fn dispatch_async(&self, req: &RawRequest) -> Result<Response, RunError> {
        let Some((handler, params)) = self.router.resolve(&req.method, &req.path) else {
            return Ok(Response {
                status: 404,
                body: b"Not Found".to_vec(),
            });
        };

        let lua = self.runtime.lua();
        let req_table = build_req(lua, req, &params).map_err(RunError::Script)?;

        let values = handler
            .call_async::<MultiValue>(req_table)
            .await
            .map_err(RunError::Script)?;
        response_from(values)
    }
}

impl Router {
    /// Compile collected registrations, rejecting a duplicate `(method,
    /// pattern)` — two routes that would match the same requests.
    fn build(
        registrations: Vec<crate::capabilities::serve::Registration>,
    ) -> Result<Self, RunError> {
        let mut routes: Vec<Route> = Vec::new();
        for reg in registrations {
            let pattern = parse_pattern(&reg.path);
            let clash = routes
                .iter()
                .any(|r| r.method == reg.method && same_signature(&r.pattern, &pattern));
            if clash {
                return Err(RunError::Script(mlua::Error::RuntimeError(format!(
                    "lur.serve: duplicate route {} {}",
                    reg.method, reg.path
                ))));
            }
            routes.push(Route {
                method: reg.method,
                pattern,
                handler: reg.handler,
            });
        }
        Ok(Self { routes })
    }

    /// Resolve `(method, path)` to a handler and its decoded path parameters.
    /// Among matches, the most specific (most static segments, then a concrete
    /// method over `ANY`) wins, independent of registration order.
    fn resolve(&self, method: &str, path: &str) -> Option<(&Function, Params)> {
        let method = method.to_uppercase();
        let req_segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        let mut best: Option<&Route> = None;
        let mut best_params: Params = Vec::new();
        for route in &self.routes {
            if route.method != "ANY" && route.method != method {
                continue;
            }
            let Some(params) = match_pattern(&route.pattern, &req_segs) else {
                continue;
            };
            if best.is_none_or(|b| more_specific(route, b)) {
                best = Some(route);
                best_params = params;
            }
        }
        best.map(|r| (&r.handler, best_params))
    }
}

/// Parse a route path into segments, dropping empty ones (so `/a/` ≡ `/a`).
fn parse_pattern(path: &str) -> Vec<Seg> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_prefix(':') {
            Some(name) => Seg::Param(name.to_owned()),
            None => Seg::Static(s.to_owned()),
        })
        .collect()
}

/// Whether two patterns match the same request shape (param names ignored).
fn same_signature(a: &[Seg], b: &[Seg]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Seg::Static(p), Seg::Static(q)) => p == q,
            (Seg::Param(_), Seg::Param(_)) => true,
            _ => false,
        })
}

/// Match `pattern` against request segments, returning decoded params or `None`.
fn match_pattern(pattern: &[Seg], req_segs: &[&str]) -> Option<Params> {
    if pattern.len() != req_segs.len() {
        return None;
    }
    let mut params = Vec::new();
    for (seg, raw) in pattern.iter().zip(req_segs) {
        match seg {
            Seg::Static(s) if s == raw => {}
            Seg::Static(_) => return None,
            Seg::Param(name) => params.push((name.clone(), percent_decode(raw.as_bytes(), false))),
        }
    }
    Some(params)
}

/// Whether route `a` is strictly more specific than `b` (a static segment beats
/// a param at the same position; a concrete method beats `ANY` as a tiebreak).
fn more_specific(a: &Route, b: &Route) -> bool {
    for (x, y) in a.pattern.iter().zip(&b.pattern) {
        match (x, y) {
            (Seg::Static(_), Seg::Param(_)) => return true,
            (Seg::Param(_), Seg::Static(_)) => return false,
            _ => {}
        }
    }
    a.method != "ANY" && b.method == "ANY"
}

/// Build the Lua `req` table handed to a handler: `method`, `path`, `params`,
/// `query` / `query_all`, `headers`, `body`, and a `json()` shorthand.
fn build_req(
    lua: &mlua::Lua,
    req: &RawRequest,
    params: &[(String, Vec<u8>)],
) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    table.set("method", req.method.to_uppercase())?;
    table.set("path", req.path.as_str())?;

    // Path parameters (percent-decoded to raw bytes, always strings).
    let params_table = lua.create_table()?;
    for (name, value) in params {
        params_table.set(name.as_str(), lua.create_string(value)?)?;
    }
    table.set("params", params_table)?;

    // Query: `query` keeps the last value per key, `query_all` the full list.
    let query = lua.create_table()?;
    let query_all = lua.create_table()?;
    for (key, value) in parse_query(&req.query) {
        let k = lua.create_string(&key)?;
        query.set(k.clone(), lua.create_string(&value)?)?;
        let list: mlua::Table = match query_all.get::<Option<mlua::Table>>(k.clone())? {
            Some(t) => t,
            None => {
                let t = lua.create_table()?;
                query_all.set(k, t.clone())?;
                t
            }
        };
        list.push(lua.create_string(&value)?)?;
    }
    table.set("query", query)?;
    table.set("query_all", query_all)?;

    // Headers: names lower-cased, last value wins (case-insensitive lookup).
    let headers = lua.create_table()?;
    for (name, value) in &req.headers {
        headers.set(name.to_lowercase(), value.as_str())?;
    }
    table.set("headers", headers)?;

    // Body as raw bytes, plus a `json()` shorthand over `lur.json.decode`.
    table.set("body", lua.create_string(&req.body)?)?;
    let decode: mlua::Function = lua
        .globals()
        .get::<mlua::Table>("lur")?
        .get::<mlua::Table>("json")?
        .get("decode")?;
    let body = req.body.clone();
    let json = lua.create_function(move |lua, ()| {
        let raw = lua.create_string(&body)?;
        decode.call::<Value>(raw)
    })?;
    table.set("json", json)?;

    Ok(table)
}

/// Parse a raw query string into ordered `(key, value)` byte pairs. Keys and
/// values are percent-decoded and `+` is treated as a space (form convention).
fn parse_query(query: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (
                percent_decode(k.as_bytes(), true),
                percent_decode(v.as_bytes(), true),
            )
        })
        .collect()
}

/// Percent-decode `input` to raw bytes. With `plus_as_space`, `+` decodes to a
/// space (query-string convention); an invalid `%` escape is left verbatim.
fn percent_decode(input: &[u8], plus_as_space: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'%' if i + 2 < input.len() => match (hex(input[i + 1]), hex(input[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Hex digit to its nibble value, or `None` if not a hex digit.
fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
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
