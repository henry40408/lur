//! Server mode (spec §3): load `app.lua` into a pool of pre-warmed VMs, then
//! dispatch each request to the matching Lua handler on an exclusively-borrowed
//! VM.
//!
//! The host owns the route table (`(method, path) → handler id`); each pooled VM
//! holds its own handler closures, keyed by that id. The network layer (hyper)
//! is a thin adapter on top of [`Server::dispatch`].

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response as HyperResponse};
use hyper_util::rt::TokioIo;
use mlua::{Function, Lua, MultiValue, Value};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::capabilities::serve::Registry;
use crate::runtime::{Deadline, RunError, RuntimeConfig, build_lua};

/// A loaded server application: a VM pool plus the host-side route table and the
/// multi-threaded runtime that drives request handling.
pub struct Server {
    pool: Pool,
    router: Router,
    rt: tokio::runtime::Runtime,
}

/// One pre-warmed VM in the pool: its sandboxed Lua state and the handler
/// closures it collected at warm-up, indexed by the host-assigned handler id.
struct Vm {
    lua: Lua,
    #[allow(dead_code)] // wired to the per-event timeout in a later slice.
    deadline: Deadline,
    handlers: Vec<Function>,
}

/// A fixed-size pool of VMs, each checked out exclusively per request. The
/// semaphore caps in-flight handlers at the pool size; a waiter parks until a
/// VM is returned.
struct Pool {
    available: Mutex<Vec<Vm>>,
    permits: Semaphore,
}

/// An exclusively-borrowed VM; returns itself to the pool on drop (before the
/// permit is released, so a waiter always finds a VM waiting).
struct CheckedOut<'a> {
    pool: &'a Pool,
    vm: Option<Vm>,
    _permit: SemaphorePermit<'a>,
}

impl Pool {
    async fn checkout(&self) -> CheckedOut<'_> {
        let permit = self
            .permits
            .acquire()
            .await
            .expect("pool semaphore never closed");
        let vm = self
            .available
            .lock()
            .expect("pool mutex poisoned")
            .pop()
            .expect("a permit guarantees an available VM");
        CheckedOut {
            pool: self,
            vm: Some(vm),
            _permit: permit,
        }
    }
}

impl CheckedOut<'_> {
    fn vm(&self) -> &Vm {
        self.vm.as_ref().expect("VM present until drop")
    }
}

impl Drop for CheckedOut<'_> {
    fn drop(&mut self) {
        if let Some(vm) = self.vm.take() {
            self.pool
                .available
                .lock()
                .expect("pool mutex poisoned")
                .push(vm);
        }
        // `_permit` releases after this body, so the VM is back in the pool
        // before any waiter is woken.
    }
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

/// A compiled route: method, parsed pattern, and the handler id (an index into
/// every VM's handler list — the same registration order across all VMs).
struct Route {
    method: String,
    pattern: Vec<Seg>,
    id: usize,
}

/// The host-side route table: resolves `(method, path)` to a handler id,
/// applying static-beats-dynamic precedence independent of registration order.
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
    /// Load `app.lua`: build a multi-threaded runtime and a pool of `pool_size`
    /// pre-warmed VMs. Each VM runs the script once to collect its own handler
    /// closures (same registration order across VMs); the route table is built
    /// from that order and rejects a duplicate `(method, path)` at load time.
    pub fn load(source: &str, config: RuntimeConfig) -> Result<Self, RunError> {
        let pool_size = config.pool_size.max(1);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(RunError::AsyncRuntime)?;

        // Warm up each VM inside the runtime so an app.lua that awaits at the top
        // level (e.g. fetching config) still works.
        let (vms, routes) = rt.block_on(async {
            let mut vms = Vec::with_capacity(pool_size);
            let mut routes: Option<Vec<(String, String)>> = None;
            for _ in 0..pool_size {
                let registry = Registry::default();
                let (lua, deadline) = build_lua(&config, Some(&registry))?;
                lua.load(source)
                    .exec_async()
                    .await
                    .map_err(RunError::Script)?;

                let regs = registry.take();
                let handlers = regs.iter().map(|r| r.handler.clone()).collect();
                let signature: Vec<(String, String)> =
                    regs.into_iter().map(|r| (r.method, r.path)).collect();
                // Every VM must register the same routes in the same order, or
                // the shared handler ids would not line up.
                if routes.get_or_insert_with(|| signature.clone()) != &signature {
                    return Err(RunError::Script(mlua::Error::RuntimeError(
                        "lur.serve: app.lua registered different routes across VMs".into(),
                    )));
                }
                vms.push(Vm {
                    lua,
                    deadline,
                    handlers,
                });
            }
            Ok((vms, routes.unwrap_or_default()))
        })?;

        let router = Router::build(&routes)?;
        let pool = Pool {
            permits: Semaphore::new(vms.len()),
            available: Mutex::new(vms),
        };
        Ok(Self { pool, router, rt })
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
        self.rt.block_on(self.dispatch_async(req))
    }

    /// Bind `addr` and serve requests forever on the multi-threaded runtime.
    /// Each connection is a spawned task; handlers run on whichever VM the pool
    /// hands out, across worker threads.
    pub fn run(self, addr: SocketAddr) -> std::io::Result<()> {
        let server = Arc::new(self);
        let driver = server.clone();
        driver.rt.block_on(async move {
            let listener = TcpListener::bind(addr).await?;
            eprintln!("lur: listening on http://{addr}");
            loop {
                let (stream, _) = listener.accept().await?;
                let io = TokioIo::new(stream);
                let server = server.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let server = server.clone();
                        async move { server.handle(req).await }
                    });
                    if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                        eprintln!("lur: connection error: {e}");
                    }
                });
            }
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
        let Some((id, params)) = self.router.resolve(&req.method, &req.path) else {
            return Ok(Response {
                status: 404,
                body: b"Not Found".to_vec(),
            });
        };

        // Borrow a VM exclusively for the whole call. Exclusive ownership is what
        // makes the per-call environment swap safe (no other request runs on this
        // VM until it is returned) — it replaces the single-VM serialize lock.
        let checked = self.pool.checkout().await;
        let vm = checked.vm();
        let handler = &vm.handlers[id];

        let req_table = build_req(&vm.lua, req, &params).map_err(RunError::Script)?;
        handler
            .set_environment(fresh_env(&vm.lua).map_err(RunError::Script)?)
            .map_err(RunError::Script)?;
        let values = handler
            .call_async::<MultiValue>(req_table)
            .await
            .map_err(RunError::Script)?;
        response_from(values)
    }
}

/// A throwaway per-call environment: writes land here (and are discarded after
/// the call) while reads fall through `__index` to the readonly globals. This
/// closes the global-bleed vector between requests sharing a VM (spec §3).
fn fresh_env(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    let env = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set("__index", lua.globals())?;
    env.set_metatable(Some(meta))?;
    Ok(env)
}

impl Router {
    /// Compile `(method, path)` registrations into routes, assigning each its
    /// registration-order id and rejecting a duplicate `(method, pattern)` —
    /// two routes that would match the same requests.
    fn build(registrations: &[(String, String)]) -> Result<Self, RunError> {
        let mut routes: Vec<Route> = Vec::new();
        for (id, (method, path)) in registrations.iter().enumerate() {
            let pattern = parse_pattern(path);
            let clash = routes
                .iter()
                .any(|r| &r.method == method && same_signature(&r.pattern, &pattern));
            if clash {
                return Err(RunError::Script(mlua::Error::RuntimeError(format!(
                    "lur.serve: duplicate route {method} {path}"
                ))));
            }
            routes.push(Route {
                method: method.clone(),
                pattern,
                id,
            });
        }
        Ok(Self { routes })
    }

    /// Resolve `(method, path)` to a handler id and its decoded path parameters.
    /// Among matches, the most specific (most static segments, then a concrete
    /// method over `ANY`) wins, independent of registration order.
    fn resolve(&self, method: &str, path: &str) -> Option<(usize, Params)> {
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
        best.map(|r| (r.id, best_params))
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
