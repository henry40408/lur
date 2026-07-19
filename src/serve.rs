//! Server mode (spec §3): load `app.lua` into a pool of pre-warmed VMs, then
//! dispatch each request to the matching Lua handler on an exclusively-borrowed
//! VM.
//!
//! The host owns the route table (`(method, path) → handler id`); each pooled VM
//! holds its own handler closures, keyed by that id. The network layer (hyper)
//! is a thin adapter on top of [`Server::dispatch`].

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use cron::Schedule;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response as HyperResponse};
use hyper_util::rt::TokioIo;
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Value};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::{error, info, warn};

use crate::capabilities::serve::Registry;
use crate::runtime::{Deadline, RunError, RuntimeConfig, build_lua};

/// A loaded server application: a VM pool plus the host-side route table and the
/// multi-threaded runtime that drives request handling.
pub struct Server {
    pool: Pool,
    router: Router,
    cron_jobs: Vec<CronJob>,
    rt: tokio::runtime::Runtime,
    /// Per-request wall-clock budget; `None` leaves handlers unbounded.
    per_event_timeout: Option<Duration>,
    /// Request-body cap; a larger body gets a 413 before the handler runs.
    max_body: Option<usize>,
    /// Grace period for draining in-flight work on graceful shutdown.
    shutdown_grace: Duration,
    /// App source text, retained for rendering handler/cron errors (diagnostics).
    source: Arc<str>,
    /// Bare chunk name (no `@` prefix) used by the diagnostics renderer.
    chunk_name: String,
}

/// One pre-warmed VM in the pool: its sandboxed Lua state and the handler
/// closures it collected at warm-up, indexed by the host-assigned handler id
/// (HTTP and cron handlers each in their own list).
struct Vm {
    lua: Lua,
    deadline: Deadline,
    handlers: Vec<Function>,
    cron_handlers: Vec<Function>,
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

/// A scheduled job: its parsed schedule plus the handler id (an index into every
/// VM's cron-handler list).
#[derive(Clone)]
struct CronJob {
    schedule: Schedule,
    name: String,
    overlap: bool,
    timeout: Option<Duration>,
    id: usize,
}

/// Cron metadata collected at warm-up (handler lives in each VM by id). Compared
/// across VMs to keep handler ids aligned.
#[derive(Clone, PartialEq, Eq)]
struct CronMeta {
    spec: String,
    name: String,
    overlap: bool,
    timeout_ms: Option<u64>,
}

/// Parse each collected cron spec into a [`CronJob`], assigning registration-
/// order ids. A bad spec (e.g. 5-field crontab) fails the load.
fn build_cron_jobs(meta: &[CronMeta]) -> Result<Vec<CronJob>, RunError> {
    meta.iter()
        .enumerate()
        .map(|(id, m)| {
            let schedule = Schedule::from_str(&m.spec).map_err(|e| {
                RunError::Script(mlua::Error::RuntimeError(format!(
                    "lur.serve: invalid cron spec {:?}: {e}",
                    m.spec
                )))
            })?;
            Ok(CronJob {
                schedule,
                name: m.name.clone(),
                overlap: m.overlap,
                timeout: m.timeout_ms.map(Duration::from_millis),
                id,
            })
        })
        .collect()
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
        let bare_chunk_name = config.chunk_name.as_deref().unwrap_or("script").to_owned();
        let chunk_name = format!("@{bare_chunk_name}");
        let (vms, routes, crons) = rt.block_on(async {
            let mut vms = Vec::with_capacity(pool_size);
            let mut routes: Option<Vec<(String, String)>> = None;
            let mut crons: Option<Vec<CronMeta>> = None;
            for _ in 0..pool_size {
                let registry = Registry::default();
                let (lua, deadline) = build_lua(&config, Some(&registry))?;
                lua.load(source)
                    .set_name(&chunk_name)
                    .exec_async()
                    .await
                    .map_err(RunError::Script)?;

                let regs = registry.take();
                let handlers = regs.iter().map(|r| r.handler.clone()).collect();
                let route_sig: Vec<(String, String)> =
                    regs.into_iter().map(|r| (r.method, r.path)).collect();

                let cron_regs = registry.take_crons();
                let cron_handlers = cron_regs.iter().map(|r| r.handler.clone()).collect();
                let cron_meta: Vec<CronMeta> = cron_regs
                    .into_iter()
                    .map(|r| CronMeta {
                        spec: r.spec,
                        name: r.name,
                        overlap: r.overlap,
                        timeout_ms: r.timeout_ms,
                    })
                    .collect();

                // Every VM must register the same routes and jobs in the same
                // order, or the shared handler ids would not line up.
                if routes.get_or_insert_with(|| route_sig.clone()) != &route_sig
                    || crons.get_or_insert_with(|| cron_meta.clone()) != &cron_meta
                {
                    return Err(RunError::Script(mlua::Error::RuntimeError(
                        "lur.serve: app.lua registered differently across VMs".into(),
                    )));
                }
                vms.push(Vm {
                    lua,
                    deadline,
                    handlers,
                    cron_handlers,
                });
            }
            Ok((vms, routes.unwrap_or_default(), crons.unwrap_or_default()))
        })?;

        let router = Router::build(&routes)?;
        let cron_jobs = build_cron_jobs(&crons)?;
        let pool = Pool {
            permits: Semaphore::new(vms.len()),
            available: Mutex::new(vms),
        };
        Ok(Self {
            pool,
            router,
            cron_jobs,
            rt,
            per_event_timeout: config.per_event_timeout,
            max_body: config.max_body,
            shutdown_grace: config.shutdown_grace,
            source: Arc::from(source),
            chunk_name: bare_chunk_name,
        })
    }

    /// Run the named cron job's handler once, returning whether a job matched.
    /// Lets embedders (and tests) trigger a job deterministically; errors are
    /// logged like a scheduled fire, never propagated.
    pub fn fire_cron(&self, name: &str) -> Result<bool, RunError> {
        let Some(job) = self.cron_jobs.iter().find(|j| j.name == name) else {
            return Ok(false);
        };
        self.rt.block_on(self.run_cron(job));
        Ok(true)
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

    /// Bind `addr` and serve requests until a shutdown signal (SIGTERM/SIGINT),
    /// then drain (spec §3/§5). Each connection is a spawned task; handlers run
    /// on whichever VM the pool hands out, across worker threads.
    pub fn run(self, addr: SocketAddr) -> std::io::Result<()> {
        self.run_with_shutdown(addr, wait_for_signal())
    }

    /// As [`Self::run`], but driven by an arbitrary `shutdown` future instead of
    /// OS signals. On shutdown: stop accepting, stop cron scheduling, then wait
    /// for in-flight requests and jobs to finish within the grace period before
    /// returning (anything still running is aborted when the runtime drops).
    pub fn run_with_shutdown(
        self,
        addr: SocketAddr,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let grace = self.shutdown_grace;
        let server = Arc::new(self);
        let driver = server.clone();
        driver.rt.block_on(async move {
            let listener = TcpListener::bind(addr).await?;
            info!("listening on http://{addr}");

            // Fan the single shutdown future out to the accept loop and every
            // cron loop via a watch channel.
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            tokio::spawn(async move {
                shutdown.await;
                let _ = shutdown_tx.send(true);
            });

            // A liveness token cloned into every in-flight connection and cron
            // run; draining waits until only this original handle remains.
            let active = Arc::new(());

            // One scheduler task per cron job; they draw VMs from the same pool.
            for job in &server.cron_jobs {
                tokio::spawn(cron_loop(
                    server.clone(),
                    job.clone(),
                    shutdown_rx.clone(),
                    active.clone(),
                ));
            }

            let mut accept_shutdown = shutdown_rx.clone();
            loop {
                tokio::select! {
                    _ = accept_shutdown.changed() => break,
                    res = listener.accept() => {
                        let (stream, _) = res?;
                        let io = TokioIo::new(stream);
                        let server = server.clone();
                        let guard = active.clone();
                        tokio::spawn(async move {
                            let _guard = guard;
                            let service = service_fn(move |req| {
                                let server = server.clone();
                                async move { server.handle(req).await }
                            });
                            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                                warn!("connection error: {e}");
                            }
                        });
                    }
                }
            }

            // Drain: wait for in-flight connections and jobs to finish, bounded
            // by the grace period.
            info!("shutting down, draining for up to {}ms", grace.as_millis());
            let deadline = Instant::now() + grace;
            while Arc::strong_count(&active) > 1 && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(())
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
                error!(
                    "handler error:\n{}",
                    crate::diagnostics::render(
                        &self.source,
                        &self.chunk_name,
                        &e.to_string(),
                        crate::color::stderr_color(),
                    )
                );
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
        // Reject an oversize body at the host edge, before routing or building
        // the Lua `req` — the VM never allocates it (spec §3).
        if matches!(self.max_body, Some(max) if req.body.len() > max) {
            return Ok(oversize_response());
        }

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

        match call_handler(vm, handler, req_table, self.per_event_timeout).await {
            Ok(values) => response_from(values),
            Err(CallError::TimedOut) => Ok(timeout_response()),
            Err(CallError::Lua(e)) => Err(RunError::Script(e)),
        }
    }

    /// Run a cron job's handler once on a pooled VM. Errors and timeouts are
    /// logged (tagged with the job name) and never propagated — a job must not
    /// bring the server down (§8).
    async fn run_cron(&self, job: &CronJob) {
        let checked = self.pool.checkout().await;
        let vm = checked.vm();
        let handler = &vm.cron_handlers[job.id];
        let timeout = job.timeout.or(self.per_event_timeout);
        match call_handler(vm, handler, (), timeout).await {
            Ok(_) => {}
            Err(CallError::TimedOut) => {
                warn!("cron[{}]: timed out", job.name);
            }
            Err(CallError::Lua(e)) => {
                error!(
                    "cron[{}]:\n{}",
                    job.name,
                    crate::diagnostics::render(
                        &self.source,
                        &self.chunk_name,
                        &e.to_string(),
                        crate::color::stderr_color(),
                    )
                );
            }
        }
    }
}

/// Why a handler call did not return a value.
enum CallError {
    /// Exceeded its time budget (interrupt or wall-clock layer).
    TimedOut,
    /// Raised a Lua error.
    Lua(mlua::Error),
}

/// Run `handler` on `vm` with a fresh per-call environment under the two-layer
/// timeout (spec §5): the deadline interrupt aborts CPU-bound code, while
/// `tokio::time::timeout` drops a handler parked on async I/O.
async fn call_handler(
    vm: &Vm,
    handler: &Function,
    args: impl IntoLuaMulti,
    timeout: Option<Duration>,
) -> Result<MultiValue, CallError> {
    handler
        .set_environment(fresh_env(&vm.lua).map_err(CallError::Lua)?)
        .map_err(CallError::Lua)?;

    let at = timeout.map(|d| Instant::now() + d);
    *vm.deadline.lock().expect("deadline mutex poisoned") = at;

    let call = handler.call_async::<MultiValue>(args);
    let outcome = match timeout {
        Some(d) => tokio::time::timeout(d, call).await.map_err(|_elapsed| ()),
        None => Ok(call.await),
    };

    *vm.deadline.lock().expect("deadline mutex poisoned") = None;

    match outcome {
        Err(()) => Err(CallError::TimedOut),
        Ok(Ok(values)) => Ok(values),
        Ok(Err(e)) => {
            if matches!(at, Some(at) if Instant::now() >= at) {
                Err(CallError::TimedOut)
            } else {
                Err(CallError::Lua(e))
            }
        }
    }
}

/// The scheduler loop for one cron job: compute the next future fire, sleep to
/// it, then run (single-flight by default — a tick is skipped, not queued, while
/// the previous run is still in flight). Fire-forward: missed ticks are never
/// replayed (spec §3).
async fn cron_loop(
    server: Arc<Server>,
    job: CronJob,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    active: Arc<()>,
) {
    let in_flight = Arc::new(AtomicBool::new(false));
    loop {
        let Some(next) = job.schedule.upcoming(Utc).next() else {
            return; // no further fires (e.g. a one-shot past spec)
        };
        let wait = (next - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        // Wake on the next fire or on shutdown, whichever comes first.
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => return,
        }
        if *shutdown.borrow() {
            return; // shutdown raced the timer → stop scheduling new runs
        }

        if !job.overlap && in_flight.swap(true, Ordering::SeqCst) {
            continue; // previous run still in flight → skip this tick
        }

        let server = server.clone();
        let job = job.clone();
        let in_flight = in_flight.clone();
        let guard = active.clone();
        tokio::spawn(async move {
            let _guard = guard;
            server.run_cron(&job).await;
            in_flight.store(false, Ordering::SeqCst);
        });
    }
}

/// Resolve once an OS shutdown signal (SIGTERM or SIGINT) arrives.
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut intr = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = intr.recv() => {}
    }
}

/// The 5xx returned when a handler exceeds its per-event budget (spec §3/§8).
fn timeout_response() -> Response {
    Response {
        status: 503,
        body: b"Service Unavailable".to_vec(),
    }
}

/// The 413 returned when a request body exceeds `max_body` (spec §3).
fn oversize_response() -> Response {
    Response {
        status: 413,
        body: b"Payload Too Large".to_vec(),
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
/// `query` / `query_all`, `headers`, `cookies`, `body`, and a `json()` shorthand.
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
        let list: mlua::Table = if let Some(t) = query_all.get::<Option<mlua::Table>>(k.clone())? {
            t
        } else {
            let t = lua.create_table()?;
            query_all.set(k, t.clone())?;
            t
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

    // Cookies: parse every `Cookie` header into one table (later value wins),
    // sharing the lenient parser with `lur.cookie.parse`. Always a table — an
    // absent or empty header yields an empty `req.cookies`, never `nil`. Cookie
    // names are case-sensitive, so (unlike header names) they are not altered.
    let cookies = lua.create_table()?;
    for (name, value) in &req.headers {
        if name.eq_ignore_ascii_case("cookie") {
            for (cname, cvalue) in crate::capabilities::cookie::cookie_pairs(value.as_bytes()) {
                cookies.set(lua.create_string(cname)?, lua.create_string(cvalue)?)?;
            }
        }
    }
    table.set("cookies", cookies)?;

    // Body as a one-shot stream. `req.read([n])` mirrors `lur.stdin.read`;
    // `req.body` (a property, served via `__index`) and `req.json()` materialize
    // the whole body but become unavailable once a chunked `req.read(n)` has
    // consumed part of it (spec §3). Shared cursor/flag behind a mutex because the
    // `send` feature requires the closures to be `Send`.
    let state = Arc::new(Mutex::new(BodyStream {
        body: req.body.clone(),
        cursor: 0,
        streamed: false,
    }));

    let read_state = Arc::clone(&state);
    let read = lua.create_function(move |lua, n: Option<usize>| {
        let mut st = read_state.lock().expect("body stream mutex poisoned");
        match n {
            // No arg: drain the remaining body (sugar, not a chunked consume).
            None => {
                let chunk = lua.create_string(&st.body[st.cursor..])?;
                st.cursor = st.body.len();
                Ok(Value::String(chunk))
            }
            // Chunked read: advance the cursor; nil once exhausted. Marks the
            // body as streamed, disabling `req.body` / `req.json()`.
            Some(n) => {
                st.streamed = true;
                if st.cursor >= st.body.len() && n > 0 {
                    return Ok(Value::Nil);
                }
                let end = (st.cursor + n).min(st.body.len());
                let chunk = lua.create_string(&st.body[st.cursor..end])?;
                st.cursor = end;
                Ok(Value::String(chunk))
            }
        }
    })?;
    table.set("read", read)?;

    let decode: mlua::Function = lua
        .globals()
        .get::<mlua::Table>("lur")?
        .get::<mlua::Table>("json")?
        .get("decode")?;
    let json_state = Arc::clone(&state);
    let json = lua.create_function(move |lua, ()| {
        let st = json_state.lock().expect("body stream mutex poisoned");
        if st.streamed {
            return Err(mlua::Error::runtime(
                "req.json() is unavailable after req.read(n) consumed part of the body",
            ));
        }
        let raw = lua.create_string(&st.body)?;
        decode.call::<Value>(raw)
    })?;
    table.set("json", json)?;

    // `req.body` is a computed property: present only while the body has not been
    // chunk-consumed. Served via `__index` so the streamed guard can fire.
    let index_state = Arc::clone(&state);
    let index = lua.create_function(move |lua, (_t, key): (mlua::Table, Value)| {
        if matches!(&key, Value::String(s) if s.as_bytes() == b"body") {
            let st = index_state.lock().expect("body stream mutex poisoned");
            if st.streamed {
                return Err(mlua::Error::runtime(
                    "req.body is unavailable after req.read(n) consumed part of the body",
                ));
            }
            return Ok(Value::String(lua.create_string(&st.body)?));
        }
        Ok(Value::Nil)
    })?;
    let meta = lua.create_table()?;
    meta.set("__index", index)?;
    table.set_metatable(Some(meta))?;

    Ok(table)
}

/// The one-shot request body behind `req.read` / `req.body` / `req.json`: a
/// cursor into the bytes plus a flag set once a chunked `read(n)` runs.
struct BodyStream {
    body: Vec<u8>,
    cursor: usize,
    streamed: bool,
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
            b'%' if i + 2 < input.len() => {
                if let (Some(h), Some(l)) = (hex(input[i + 1]), hex(input[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
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
/// table; `status` defaults to 200 (and must be a valid HTTP status in
/// `100..=599`) and `body` to empty.
fn response_from(values: MultiValue) -> Result<Response, RunError> {
    let table = match values.into_iter().next() {
        Some(Value::Table(t)) => t,
        _ => {
            return Err(RunError::Script(mlua::Error::RuntimeError(
                "handler must return a table { status, body, headers }".into(),
            )));
        }
    };

    // Validate rather than `as u16`-truncate: an out-of-range `status` (negative
    // or > u16, or outside the HTTP range) would otherwise silently wrap to a
    // bogus code on the wire (e.g. -1 → 65535, 65736 → 200).
    let status_raw = table
        .get::<Option<i64>>("status")
        .map_err(RunError::Script)?
        .unwrap_or(200);
    let status = u16::try_from(status_raw)
        .ok()
        .filter(|s| (100..=599).contains(s))
        .ok_or_else(|| {
            RunError::Script(mlua::Error::RuntimeError(format!(
                "handler returned invalid HTTP status {status_raw}; must be in 100..=599"
            )))
        })?;
    let body = table
        .get::<Option<mlua::LuaString>>("body")
        .map_err(RunError::Script)?
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_default();

    Ok(Response { status, body })
}
