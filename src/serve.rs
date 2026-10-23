//! Server mode: a pool of pre-warmed VMs, each request run on an
//! exclusively-borrowed VM. The host owns the route table (`(method, path) →
//! handler id`); each VM holds its own handler closures under the same ids.
//!
//! ## Design notes
//!
//! - **Responses**: no `Content-Type` inference ("explicit over magic"); an invalid header
//!   fails the whole response (500) instead of being dropped.
//! - **Optional route params (`:name?`)** are trailing-only, so one request maps to exactly
//!   one prefix and `req.params` is a plain table (absent = `nil`). Overlap with another
//!   route of the same method (`/x/:p?` vs `/x`) is a load error, not a silent specificity
//!   tiebreak. Mid-path optionals and regex params were rejected as ambiguity for no real
//!   use.
//! - **`req.origin` / `req.url`** come from `Host` and `X-Forwarded-Proto` (`http` unless
//!   the latter is `https`; its first comma-separated value is used). They are `nil` when
//!   `Host` is missing or not a bare `host[:port]` / `[ipv6][:port]` (the first `Host`
//!   header wins). Both are client-controlled, so they are for building links only. A
//!   `--public-url` flag was deferred: add it if feeds behind a proxy need a pinned origin.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use cron::Schedule;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue, TRANSFER_ENCODING};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response as HyperResponse};
use hyper_util::rt::TokioIo;
use hyper_util::server::graceful::GracefulShutdown;
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Value};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};

use crate::capabilities::raise;
use crate::capabilities::serve::{
    Abort, AbortSlot, PendingAbort, Registry, ServeCtx, abort_table, status_arg,
};
use crate::runtime::{Deadline, RunError, RuntimeConfig, build_lua};

/// A loaded app: VM pool, route table, cron jobs, and the tokio runtime.
pub struct Server {
    pool: Pool,
    router: Router,
    cron_jobs: Vec<CronJob>,
    rt: tokio::runtime::Runtime,
    /// `None` leaves handlers unbounded.
    per_event_timeout: Option<Duration>,
    /// Request-body cap; a larger body gets a 413 before the handler runs.
    max_body: Option<usize>,
    /// Grace period for draining in-flight work on graceful shutdown.
    shutdown_grace: Duration,
    /// Kept for rendering handler/cron errors.
    source: Arc<str>,
    /// Chunk name without the `@` prefix.
    chunk_name: String,
}

/// A pre-warmed VM with its handler closures, indexed by handler id.
struct Vm {
    lua: Lua,
    deadline: Deadline,
    handlers: Vec<Function>,
    cron_handlers: Vec<Function>,
    /// Shared with `lur.serve.abort`; reset by `call_handler` on every run.
    abort: AbortSlot,
}

/// Fixed-size VM pool; the semaphore caps in-flight handlers at the pool size.
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

/// A route pattern segment: literal, `:name` binding one segment, or `:name?`
/// binding one segment that may be absent (trailing segments only).
enum Seg {
    Static(String),
    Param { name: String, optional: bool },
}

/// `id` indexes every VM's handler list (same registration order in all VMs).
struct Route {
    method: String,
    pattern: Vec<Seg>,
    id: usize,
}

struct Router {
    routes: Vec<Route>,
}

/// `id` indexes every VM's cron-handler list.
#[derive(Clone)]
struct CronJob {
    schedule: Schedule,
    name: String,
    overlap: bool,
    timeout: Option<Duration>,
    id: usize,
}

/// Cron registration minus the handler; compared across VMs to keep ids aligned.
#[derive(Clone, PartialEq, Eq)]
struct CronMeta {
    spec: String,
    name: String,
    overlap: bool,
    timeout_ms: Option<u64>,
}

/// A bad spec (e.g. 5-field crontab) fails the load.
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

/// A request before it crosses into Lua.
#[derive(Debug, Default, Clone)]
pub struct RawRequest {
    /// Case-insensitive; upper-cased for the handler.
    pub method: String,
    /// Still percent-encoded.
    pub path: String,
    /// Without the leading `?`; empty if none.
    pub query: String,
    /// In arrival order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A handler's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    /// Validated; a name may repeat (e.g. `Set-Cookie`).
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub body: Vec<u8>,
}

impl Server {
    /// Run `app.lua` once per pooled VM to collect handlers. Fails if VMs
    /// register differently or a route is duplicated.
    pub fn load(source: &str, config: RuntimeConfig) -> Result<Self, RunError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(RunError::AsyncRuntime)?;

        // Warm up inside the runtime so top-level awaits in app.lua work.
        let bare_chunk_name = config.chunk_name.as_deref().unwrap_or("script").to_owned();
        let chunk_name = format!("@{bare_chunk_name}");
        let (vms, routes, crons) = rt.block_on(warm_up(&config, source, &chunk_name))?;

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

    /// Run the named cron job once; returns whether it exists. Errors are
    /// logged, not propagated.
    pub fn fire_cron(&self, name: &str) -> Result<bool, RunError> {
        let Some(job) = self.cron_jobs.iter().find(|j| j.name == name) else {
            return Ok(false);
        };
        self.rt.block_on(self.run_cron(job));
        Ok(true)
    }

    /// [`Self::dispatch_raw`] with empty query and headers.
    pub fn dispatch(&self, method: &str, path: &str, body: &[u8]) -> Result<Response, RunError> {
        self.dispatch_raw(&RawRequest {
            method: method.to_owned(),
            path: path.to_owned(),
            body: body.to_vec(),
            ..Default::default()
        })
    }

    /// Blocking wrapper over the async dispatch.
    pub fn dispatch_raw(&self, req: &RawRequest) -> Result<Response, RunError> {
        self.rt.block_on(self.dispatch_async(req))
    }

    /// Serve on `addr` until SIGTERM/SIGINT, then drain.
    pub fn run(self, addr: SocketAddr) -> std::io::Result<()> {
        self.run_with_shutdown(addr, wait_for_signal())
    }

    /// [`Self::run`] with a custom `shutdown` future. On shutdown, stops
    /// accepting and scheduling, then waits up to the grace period for in-flight
    /// work; anything left is aborted when the runtime drops.
    pub fn run_with_shutdown(
        self,
        addr: SocketAddr,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let server = Arc::new(self);
        let driver = server.clone();
        driver.rt.block_on(server.serve(addr, shutdown))
    }

    async fn serve(
        self: Arc<Self>,
        addr: SocketAddr,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let grace = self.shutdown_grace;
        let listener = TcpListener::bind(addr).await?;
        info!("listening on http://{addr}");

        let shutdown_rx = fan_out_shutdown(shutdown);

        // Connections are watched by `graceful`, which tells idle keep-alive
        // ones to close on shutdown; cron runs are tracked by `crons`.
        let graceful = GracefulShutdown::new();
        let crons = TaskTracker::new();

        for job in &self.cron_jobs {
            tokio::spawn(cron_loop(
                self.clone(),
                job.clone(),
                shutdown_rx.clone(),
                crons.clone(),
            ));
        }

        self.accept_until(&listener, &graceful, shutdown_rx).await?;

        info!("shutting down, draining for up to {}ms", grace.as_millis());
        crons.close();
        // Anything still running after the grace period is aborted when the
        // runtime drops.
        let _ = tokio::time::timeout(grace, async {
            tokio::join!(graceful.shutdown(), crons.wait());
        })
        .await;
        Ok(())
    }

    /// Accept connections until `shutdown` flips.
    async fn accept_until(
        self: &Arc<Self>,
        listener: &TcpListener,
        graceful: &GracefulShutdown,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> std::io::Result<()> {
        loop {
            tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                res = listener.accept() => {
                    let (stream, _) = res?;
                    self.spawn_connection(stream, graceful);
                }
            }
        }
    }

    fn spawn_connection(
        self: &Arc<Self>,
        stream: tokio::net::TcpStream,
        graceful: &GracefulShutdown,
    ) {
        let io = TokioIo::new(stream);
        let server = self.clone();
        let service = service_fn(move |req| {
            let server = server.clone();
            async move { server.handle(req).await }
        });
        let conn = graceful.watch(http1::Builder::new().serve_connection(io, service));
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                warn!("connection error: {e}");
            }
        });
    }

    /// A handler error becomes a logged 500, never a crash.
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
        // `Limited` stops reading at the cap, so an oversized body is never buffered.
        let limit = self.max_body.unwrap_or(usize::MAX);
        let body = match Limited::new(req.into_body(), limit).collect().await {
            Ok(c) => c.to_bytes().to_vec(),
            Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => {
                return Ok(to_hyper(oversize_response()));
            }
            Err(_) => Vec::new(),
        };

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
                    headers: Vec::new(),
                    body: b"Internal Server Error".to_vec(),
                }
            }
        };

        Ok(to_hyper(response))
    }

    async fn dispatch_async(&self, req: &RawRequest) -> Result<Response, RunError> {
        // Reject before routing so the VM never allocates the body.
        if matches!(self.max_body, Some(max) if req.body.len() > max) {
            return Ok(oversize_response());
        }

        let Some((id, params)) = self.router.resolve(&req.method, &req.path) else {
            return Ok(Response {
                status: 404,
                headers: Vec::new(),
                body: b"Not Found".to_vec(),
            });
        };

        // Exclusive checkout is what makes the per-call environment swap safe.
        let checked = self.pool.checkout().await;
        let vm = checked.vm();
        let handler = &vm.handlers[id];
        let req_table = build_req(&vm.lua, req, &params).map_err(RunError::Script)?;

        match call_handler(
            vm,
            handler,
            req_table,
            Run {
                timeout: self.per_event_timeout,
                in_request: true,
            },
        )
        .await
        {
            Ok(values) => response_from(values),
            Err(CallError::TimedOut) => Ok(timeout_response()),
            Err(CallError::Aborted(a)) => Ok(abort_response(a)),
            Err(CallError::Lua(e)) => Err(RunError::Script(e)),
        }
    }

    /// Errors and timeouts are logged, never propagated.
    async fn run_cron(&self, job: &CronJob) {
        let checked = self.pool.checkout().await;
        let vm = checked.vm();
        let handler = &vm.cron_handlers[job.id];
        let timeout = job.timeout.or(self.per_event_timeout);
        match call_handler(
            vm,
            handler,
            (),
            Run {
                timeout,
                in_request: false,
            },
        )
        .await
        {
            Ok(_) => {}
            Err(CallError::TimedOut) => {
                warn!("cron[{}]: timed out", job.name);
            }
            // `abort` refuses to run outside a request, so this cannot happen.
            Err(CallError::Aborted(a)) => {
                warn!("cron[{}]: aborted: {}", job.name, a.message);
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

fn to_hyper(response: Response) -> HyperResponse<Full<Bytes>> {
    let mut built = HyperResponse::builder().status(response.status);
    for (name, value) in response.headers {
        built = built.header(name, value);
    }
    built
        .body(Full::new(Bytes::from(response.body)))
        .unwrap_or_else(|_| HyperResponse::new(Full::new(Bytes::new())))
}

enum CallError {
    /// Hit either timeout layer.
    TimedOut,
    /// The handler ended in an uncaught `lur.serve.abort`.
    Aborted(PendingAbort),
    Lua(mlua::Error),
}

/// The JSON response for an uncaught abort. A 4xx is the client's doing, so
/// it is logged at `info`; a 5xx chosen by the script at `warn`.
fn abort_response(a: PendingAbort) -> Response {
    // The message can carry client input; keep the log line bounded.
    let shown: String = a.message.chars().take(200).collect();
    let ellipsis = if shown.len() < a.message.len() {
        "…"
    } else {
        ""
    };
    if a.status >= 500 {
        warn!("handler aborted with {}: {shown}{ellipsis}", a.status);
    } else {
        info!("handler aborted with {}: {shown}{ellipsis}", a.status);
    }
    Response {
        status: a.status,
        headers: vec![(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        body: a.body,
    }
}

/// The routes and cron jobs `app.lua` registered, in order.
#[derive(Clone, Default, PartialEq)]
struct Registrations {
    routes: Vec<(String, String)>,
    crons: Vec<CronMeta>,
}

/// A VM that has run `app.lua`, with what it registered.
struct Warmed {
    vm: Vm,
    registered: Registrations,
}

/// Run `app.lua` in a fresh VM and collect its handlers.
async fn warm_up_vm(
    config: &RuntimeConfig,
    source: &str,
    chunk_name: &str,
) -> Result<Warmed, RunError> {
    let registry = Registry::default();
    let (lua, deadline) = build_lua(config, Some(&registry))?;
    let top = lua.load(source).set_name(chunk_name).exec_async();
    // No timeout here, but the same end-of-run cleanup.
    crate::runtime::with_timeout(&lua, &deadline, None, top)
        .await
        .unwrap_or_else(|()| unreachable!("no timeout set"))
        .map_err(RunError::Script)?;

    let regs = registry.take();
    let handlers = regs.iter().map(|r| r.handler.clone()).collect();
    let routes = regs.into_iter().map(|r| (r.method, r.path)).collect();

    let cron_regs = registry.take_crons();
    let cron_handlers = cron_regs.iter().map(|r| r.handler.clone()).collect();
    let crons = cron_regs
        .into_iter()
        .map(|r| CronMeta {
            spec: r.spec,
            name: r.name,
            overlap: r.overlap,
            timeout_ms: r.timeout_ms,
        })
        .collect();

    Ok(Warmed {
        vm: Vm {
            lua,
            deadline,
            handlers,
            cron_handlers,
            abort: registry.abort_slot(),
        },
        registered: Registrations { routes, crons },
    })
}

/// Warm every pooled VM. Warm up inside the runtime so top-level awaits in
/// app.lua work. Every VM must register the same routes and jobs in the same
/// order, or the shared handler ids would not line up.
async fn warm_up(
    config: &RuntimeConfig,
    source: &str,
    chunk_name: &str,
) -> Result<(Vec<Vm>, Vec<(String, String)>, Vec<CronMeta>), RunError> {
    let pool_size = config.pool_size.max(1);
    let mut vms = Vec::with_capacity(pool_size);
    let mut expected: Option<Registrations> = None;
    for _ in 0..pool_size {
        let Warmed { vm, registered } = warm_up_vm(config, source, chunk_name).await?;
        if *expected.get_or_insert_with(|| registered.clone()) != registered {
            return Err(RunError::Script(mlua::Error::RuntimeError(
                "lur.serve: app.lua registered differently across VMs".into(),
            )));
        }
        vms.push(vm);
    }
    let Registrations { routes, crons } = expected.unwrap_or_default();
    Ok((vms, routes, crons))
}

/// How `call_handler` runs a handler: its time budget, and whether
/// `lur.serve.abort` is allowed (only inside an HTTP request).
struct Run {
    timeout: Option<Duration>,
    in_request: bool,
}

/// Run `handler` in a fresh environment under the two-layer timeout:
/// the deadline interrupt stops CPU-bound code, `tokio::time::timeout` drops
/// code parked on async I/O.
async fn call_handler(
    vm: &Vm,
    handler: &Function,
    args: impl IntoLuaMulti,
    run: Run,
) -> Result<MultiValue, CallError> {
    let Run {
        timeout,
        in_request,
    } = run;
    arm_vm(vm, handler, in_request)?;

    let at = timeout.map(|d| Instant::now() + d);
    *vm.deadline.lock().expect("deadline mutex poisoned") = at;

    let call = handler.call_async::<MultiValue>(args);
    let outcome = crate::runtime::with_timeout(&vm.lua, &vm.deadline, timeout, call).await;

    *vm.deadline.lock().expect("deadline mutex poisoned") = None;
    // Don't keep a rendered error table alive on an idle pooled VM.
    raise::reset(&vm.lua);

    let pending = {
        let mut abort = vm.abort.lock().expect("abort slot poisoned");
        abort.in_request = false;
        abort.pending.take()
    };

    let result = classify_outcome(outcome, at, pending);
    if needs_reclaim(&result) {
        crate::runtime::reclaim(&vm.lua);
    }
    result
}

/// Reset what a previous run left on the pooled VM and give `handler` a fresh
/// environment.
fn arm_vm(vm: &Vm, handler: &Function, in_request: bool) -> Result<(), CallError> {
    {
        // Drop any abort left by a previous run (e.g. one `pcall` caught).
        let mut abort = vm.abort.lock().expect("abort slot poisoned");
        abort.in_request = in_request;
        abort.pending = None;
    }
    // ...and any error table a previous run left for `lur.async` to recover.
    raise::reset(&vm.lua);
    handler
        .set_environment(fresh_env(&vm.lua).map_err(CallError::Lua)?)
        .map_err(CallError::Lua)?;
    Ok(())
}

/// `outcome` of `with_timeout`: `Err(())` is the async timeout; a script error
/// past `at` is the deadline interrupt; one matching `pending` is an abort.
fn classify_outcome(
    outcome: Result<mlua::Result<MultiValue>, ()>,
    at: Option<Instant>,
    pending: Option<PendingAbort>,
) -> Result<MultiValue, CallError> {
    match outcome {
        Err(()) => Err(CallError::TimedOut),
        Ok(Ok(values)) => Ok(values),
        Ok(Err(e)) => {
            if matches!(at, Some(at) if Instant::now() >= at) {
                Err(CallError::TimedOut)
            } else if let Some(p) = pending
                && raise::is_flattened(&e, &p.message)
            {
                Err(CallError::Aborted(p))
            } else {
                Err(CallError::Lua(e))
            }
        }
    }
}

/// A timeout or out-of-memory error leaves garbage worth collecting now.
fn needs_reclaim(result: &Result<MultiValue, CallError>) -> bool {
    match result {
        Err(CallError::TimedOut) => true,
        Err(CallError::Lua(e)) => crate::runtime::is_memory_error(e),
        _ => false,
    }
}

/// Sleep to each next fire and run the job. Single-flight by default (a tick
/// is skipped while the previous run is in flight); missed ticks are never
/// replayed.
async fn cron_loop(
    server: Arc<Server>,
    job: CronJob,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    crons: TaskTracker,
) {
    let in_flight = Arc::new(AtomicBool::new(false));
    loop {
        let Some(next) = job.schedule.upcoming(Utc).next() else {
            return; // no further fires (e.g. a one-shot past spec)
        };
        let wait = (next - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
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
        crons.spawn(async move {
            server.run_cron(&job).await;
            in_flight.store(false, Ordering::SeqCst);
        });
    }
}

/// Fan `shutdown` out to the accept loop and every cron loop.
fn fan_out_shutdown(
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> tokio::sync::watch::Receiver<bool> {
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown.await;
        let _ = tx.send(true);
    });
    rx
}

async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut intr = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = intr.recv() => {}
    }
}

fn timeout_response() -> Response {
    Response {
        status: 503,
        headers: Vec::new(),
        body: b"Service Unavailable".to_vec(),
    }
}

fn oversize_response() -> Response {
    Response {
        status: 413,
        headers: Vec::new(),
        body: b"Payload Too Large".to_vec(),
    }
}

/// Per-call environment: writes land here and are discarded; reads fall
/// through to the readonly globals. Prevents cross-request global bleed.
fn fresh_env(lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
    let env = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set("__index", lua.globals())?;
    env.set_metatable(Some(meta))?;
    Ok(env)
}

impl Router {
    /// Rejects two routes with the same method and pattern shape.
    fn build(registrations: &[(String, String)]) -> Result<Self, RunError> {
        let mut routes: Vec<Route> = Vec::new();
        for (id, (method, path)) in registrations.iter().enumerate() {
            let pattern = parse_pattern(path).map_err(|e| {
                RunError::Script(mlua::Error::RuntimeError(format!(
                    "lur.serve: route {method} {path}: {e}"
                )))
            })?;
            let clash = routes
                .iter()
                .any(|r| &r.method == method && overlaps(&r.pattern, &pattern));
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

    /// The most specific match wins regardless of registration order.
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

/// Empty segments are dropped, so `/a/` ≡ `/a`. An optional `:name?` may only
/// be followed by other optional params, so a request maps to one prefix.
fn parse_pattern(path: &str) -> Result<Vec<Seg>, String> {
    let segs: Vec<Seg> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_prefix(':') {
            Some(name) => match name.strip_suffix('?') {
                Some(name) => Seg::Param {
                    name: name.to_owned(),
                    optional: true,
                },
                None => Seg::Param {
                    name: name.to_owned(),
                    optional: false,
                },
            },
            None => Seg::Static(s.to_owned()),
        })
        .collect();
    if segs
        .iter()
        .any(|s| matches!(s, Seg::Param { name, .. } if name.is_empty()))
    {
        return Err("a `:` param needs a name".to_owned());
    }
    let mut seen_optional = false;
    for seg in &segs {
        match seg {
            Seg::Param { optional: true, .. } => seen_optional = true,
            _ if seen_optional => {
                return Err("an optional `:name?` param must be the last segment(s)".to_owned());
            }
            _ => {}
        }
    }
    Ok(segs)
}

/// Fewest request segments a pattern matches (its required prefix).
fn min_len(pattern: &[Seg]) -> usize {
    pattern
        .iter()
        .take_while(|s| !matches!(s, Seg::Param { optional: true, .. }))
        .count()
}

/// Whether two patterns can match the same request (param names ignored).
fn overlaps(a: &[Seg], b: &[Seg]) -> bool {
    let lo = min_len(a).max(min_len(b));
    let hi = a.len().min(b.len());
    (lo..=hi).any(|n| {
        a[..n].iter().zip(&b[..n]).all(|(x, y)| match (x, y) {
            (Seg::Static(p), Seg::Static(q)) => p == q,
            (Seg::Param { .. }, Seg::Param { .. }) => true,
            _ => false,
        })
    })
}

fn match_pattern(pattern: &[Seg], req_segs: &[&str]) -> Option<Params> {
    if req_segs.len() < min_len(pattern) || req_segs.len() > pattern.len() {
        return None;
    }
    let mut params = Vec::new();
    for (seg, raw) in pattern.iter().zip(req_segs) {
        match seg {
            Seg::Static(s) if s == raw => {}
            Seg::Static(_) => return None,
            Seg::Param { name, .. } => {
                params.push((name.clone(), percent_decode(raw.as_bytes(), false)));
            }
        }
    }
    Some(params)
}

/// At the first differing position a static segment beats a param; a concrete
/// method beats `ANY` as the tiebreak.
fn more_specific(a: &Route, b: &Route) -> bool {
    for (x, y) in a.pattern.iter().zip(&b.pattern) {
        match (x, y) {
            (Seg::Static(_), Seg::Param { .. }) => return true,
            (Seg::Param { .. }, Seg::Static(_)) => return false,
            _ => {}
        }
    }
    a.method != "ANY" && b.method == "ANY"
}

/// `scheme://host[:port]` from `Host` and `X-Forwarded-Proto` (`http` unless the
/// first proxy value is `https`); `None` if `Host` is absent or not a bare
/// `host[:port]` / `[ipv6][:port]` authority.
fn request_origin(headers: &[(String, String)]) -> Option<String> {
    let find = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    };
    let host = find("host")?;
    if !is_bare_authority(host) {
        return None;
    }
    let scheme = match find("x-forwarded-proto").map(|p| p.split(',').next().unwrap_or("").trim()) {
        Some(p) if p.eq_ignore_ascii_case("https") => "https",
        _ => "http",
    };
    Some(format!("{scheme}://{host}"))
}

fn is_bare_authority(host: &str) -> bool {
    let (name_ok, port) = if let Some(rest) = host.strip_prefix('[') {
        let Some((v6, tail)) = rest.split_once(']') else {
            return false;
        };
        let ok = !v6.is_empty()
            && v6
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.'));
        let port = match tail {
            "" => None,
            t => match t.strip_prefix(':') {
                Some(p) => Some(p),
                None => return false,
            },
        };
        (ok, port)
    } else {
        let (name, port) = match host.split_once(':') {
            Some((n, p)) => (n, Some(p)),
            None => (host, None),
        };
        let ok = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        (ok, port)
    };
    name_ok
        && port
            .is_none_or(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn build_req(
    lua: &mlua::Lua,
    req: &RawRequest,
    params: &[(String, Vec<u8>)],
) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    table.set("method", req.method.to_uppercase())?;
    table.set("path", req.path.as_str())?;
    set_origin_and_url(&table, req)?;
    table.set("params", params_table(lua, params)?)?;

    // Query: `query` keeps the last value per key, `query_all` the full list.
    let pairs = parse_query(&req.query);
    table.set("query", last_table(lua, &pairs)?)?;
    table.set("query_all", all_table(lua, &pairs)?)?;

    let headers = headers_table(lua, req)?;
    table.set("headers", headers.clone())?;
    table.set("cookies", cookies_table(lua, req)?)?;

    // `req.body` / `req.json()` become unavailable once `req.read(n)` has run.
    // The mutex makes the closures `Send`.
    let state = Arc::new(Mutex::new(BodyStream {
        body: req.body.clone(),
        cursor: 0,
        streamed: false,
    }));
    table.set("read", read_fn(lua, &state)?)?;
    table.set("json", json_fn(lua, &state)?)?;

    let meta = lua.create_table()?;
    meta.set("__index", index_fn(lua, state, headers)?)?;
    table.set_metatable(Some(meta))?;

    Ok(table)
}

/// Both come from client-controlled headers: fine for building links, not for
/// access decisions.
fn set_origin_and_url(table: &mlua::Table, req: &RawRequest) -> mlua::Result<()> {
    let Some(origin) = request_origin(&req.headers) else {
        return Ok(());
    };
    let mut url = format!("{origin}{}", req.path);
    if !req.query.is_empty() {
        url.push('?');
        url.push_str(&req.query);
    }
    table.set("origin", origin)?;
    table.set("url", url)
}

fn params_table(lua: &mlua::Lua, params: &[(String, Vec<u8>)]) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    for (name, value) in params {
        table.set(name.as_str(), lua.create_string(value)?)?;
    }
    Ok(table)
}

/// Lower-cased names; last value wins.
fn headers_table(lua: &mlua::Lua, req: &RawRequest) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    for (name, value) in &req.headers {
        table.set(name.to_lowercase(), value.as_str())?;
    }
    Ok(table)
}

/// All `Cookie` headers fold into one table (later wins; never `nil`).
/// Cookie names are case-sensitive, so not lowered.
fn cookies_table(lua: &mlua::Lua, req: &RawRequest) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    for (name, value) in &req.headers {
        if !name.eq_ignore_ascii_case("cookie") {
            continue;
        }
        for (cname, cvalue) in crate::capabilities::cookie::cookie_pairs(value.as_bytes()) {
            table.set(lua.create_string(cname)?, lua.create_string(cvalue)?)?;
        }
    }
    Ok(table)
}

/// `req.read(n?)`: with no argument, drain the rest (not counted as
/// streaming); with `n`, return up to `n` bytes, or `nil` once exhausted.
fn read_fn(lua: &mlua::Lua, state: &Arc<Mutex<BodyStream>>) -> mlua::Result<mlua::Function> {
    let state = Arc::clone(state);
    lua.create_function(move |lua, n: Option<usize>| {
        let mut st = state.lock().expect("body stream mutex poisoned");
        let Some(n) = n else {
            let chunk = lua.create_string(&st.body[st.cursor..])?;
            st.cursor = st.body.len();
            return Ok(Value::String(chunk));
        };
        st.streamed = true;
        if st.cursor >= st.body.len() && n > 0 {
            return Ok(Value::Nil);
        }
        let end = (st.cursor + n).min(st.body.len());
        let chunk = lua.create_string(&st.body[st.cursor..end])?;
        st.cursor = end;
        Ok(Value::String(chunk))
    })
}

fn json_fn(lua: &mlua::Lua, state: &Arc<Mutex<BodyStream>>) -> mlua::Result<mlua::Function> {
    let decode: mlua::Function = lua
        .globals()
        .get::<mlua::Table>("lur")?
        .get::<mlua::Table>("json")?
        .get("decode")?;
    let state = Arc::clone(state);
    lua.create_function(move |lua, ()| {
        let st = state.lock().expect("body stream mutex poisoned");
        if st.streamed {
            return Err(mlua::Error::runtime(
                "req.json() is unavailable after req.read(n) consumed part of the body",
            ));
        }
        let raw = lua.create_string(&st.body)?;
        decode.call::<Value>(raw)
    })
}

/// `req.form()` / `req.form_all()`, returned as a function by `__index`.
fn form_fn(
    lua: &mlua::Lua,
    state: Arc<Mutex<BodyStream>>,
    headers: mlua::Table,
    all: bool,
) -> mlua::Result<mlua::Function> {
    lua.create_function(move |lua, ()| {
        let label = if all { "req.form_all()" } else { "req.form()" };
        let pairs = form_pairs(&state, &headers, label)?;
        if all {
            all_table(lua, &pairs)
        } else {
            last_table(lua, &pairs)
        }
    })
}

/// Served via `__index` so the streamed guard can fire.
fn index_fn(
    lua: &mlua::Lua,
    state: Arc<Mutex<BodyStream>>,
    headers: mlua::Table,
) -> mlua::Result<mlua::Function> {
    lua.create_function(move |lua, (t, key): (mlua::Table, Value)| {
        let Value::String(s) = &key else {
            return Ok(Value::Nil);
        };
        // `req.valid_*` and `req.form*` are built on first access, so requests
        // that never use them pay nothing.
        match &*s.as_bytes() {
            b"valid_query" => valid_fn(lua, &t, true),
            b"valid_params" => valid_fn(lua, &t, false),
            b"valid_body" => valid_body_fn(lua, &state, &headers),
            b"form" | b"form_all" => {
                let all = &*s.as_bytes() == b"form_all";
                let f = form_fn(lua, Arc::clone(&state), headers.clone(), all)?;
                Ok(Value::Function(f))
            }
            b"body" => {
                let st = state.lock().expect("body stream mutex poisoned");
                if st.streamed {
                    return Err(mlua::Error::runtime(
                        "req.body is unavailable after req.read(n) consumed part of the body",
                    ));
                }
                Ok(Value::String(lua.create_string(&st.body)?))
            }
            _ => Ok(Value::Nil),
        }
    })
}

/// `ServeCtx` for a `req.valid_*` accessor, or the "only under serve" error.
fn serve_ctx(lua: &Lua, fname: &str) -> mlua::Result<ServeCtx> {
    lua.app_data_ref::<ServeCtx>()
        .map(|c| c.clone())
        .ok_or_else(|| mlua::Error::runtime(format!("{fname} is only available under `lur serve`")))
}

/// `opts.fail_status` of `req.valid_*` (default 400).
fn fail_status_opt(opts: Value, fname: &str) -> mlua::Result<u16> {
    let opts = match opts {
        Value::Nil => return Ok(400),
        Value::Table(o) => o,
        other => {
            return Err(mlua::Error::runtime(format!(
                "{fname}: argument #2 must be table, got {}",
                other.type_name()
            )));
        }
    };
    match opts.get::<Value>("fail_status")? {
        Value::Nil => Ok(400),
        v => status_arg(&v, fname, "opts.fail_status"),
    }
}

/// The `issues` of a failed `lur.validate.try`, or `nil`.
fn issues_of(error: &Value) -> mlua::Result<Value> {
    match error {
        Value::Table(e) => e.get::<Value>("issues"),
        _ => Ok(Value::Nil),
    }
}

/// The `(false, err)` result of a `req.valid_*` call that aborts.
fn aborted(lua: &Lua, ctx: &ServeCtx, abort: Abort<'_>) -> mlua::Result<(bool, Value)> {
    let err = abort_table(lua, ctx, abort)?;
    Ok((false, Value::Table(err)))
}

/// `req.valid_query(schema, opts?)` / `req.valid_params(schema, opts?)`:
/// `lur.validate.try` over `req.query` / `req.params`; on issues, abort with
/// `opts.fail_status` (default 400) and `data = issues`. Anything else `try`
/// raises (bad schema, a validator's real error) propagates unchanged.
fn valid_fn(lua: &Lua, req: &mlua::Table, query: bool) -> mlua::Result<Value> {
    let fname = if query {
        "req.valid_query"
    } else {
        "req.valid_params"
    };
    let ctx = serve_ctx(lua, fname)?;
    let factory = ctx.factory.clone();
    let try_fn = ctx.try_fn.clone();
    let source: mlua::Table = req.raw_get(if query { "query" } else { "params" })?;
    let query_all: Option<mlua::Table> = if query {
        req.raw_get("query_all")?
    } else {
        None
    };

    let inner = lua.create_async_function(move |lua, (schema, opts): (Value, Value)| {
        let ctx = ctx.clone();
        let try_fn = try_fn.clone();
        let source = source.clone();
        let query_all = query_all.clone();
        async move {
            let fail_status = fail_status_opt(opts, fname)?;

            // Lists read every value (`query_all`); other fields the last one.
            let input = match &query_all {
                Some(all) => feed_lists(&lua, source, &schema, || Ok(all.clone()))?,
                None => source,
            };

            let (ok, value): (bool, Value) = try_fn.call_async((schema, input)).await?;
            if ok {
                return Ok((true, value));
            }
            let abort = Abort {
                status: fail_status,
                message: "validation failed",
                data: issues_of(&value)?,
            };
            aborted(&lua, &ctx, abort)
        }
    })?;
    // Fresh `error(tbl, 0)` wrapper so `pcall` sees the table.
    Ok(Value::Function(crate::capabilities::raise::raising(
        &factory, inner,
    )?))
}

/// `{key = last value}` over ordered pairs.
fn last_table(lua: &Lua, pairs: &[(Vec<u8>, Vec<u8>)]) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    for (k, v) in pairs {
        t.set(lua.create_string(k)?, lua.create_string(v)?)?;
    }
    Ok(t)
}

/// `{key = {values...}}` over ordered pairs.
fn all_table(lua: &Lua, pairs: &[(Vec<u8>, Vec<u8>)]) -> mlua::Result<mlua::Table> {
    let all = lua.create_table()?;
    for (key, value) in pairs {
        let k = lua.create_string(key)?;
        let list: mlua::Table = if let Some(t) = all.get::<Option<mlua::Table>>(k.clone())? {
            t
        } else {
            let t = lua.create_table()?;
            all.set(k, t.clone())?;
            t
        };
        list.push(lua.create_string(value)?)?;
    }
    Ok(all)
}

/// For an `object` schema with `list(...)` fields, a copy of `source` whose
/// list fields come from the all-values table (built only when needed);
/// otherwise `source` itself.
fn feed_lists(
    lua: &Lua,
    source: mlua::Table,
    schema: &Value,
    all: impl FnOnce() -> mlua::Result<mlua::Table>,
) -> mlua::Result<mlua::Table> {
    match crate::capabilities::validate::list_field_names(schema) {
        Some(lists) if !lists.is_empty() => {
            let all = all()?;
            let copy = lua.create_table()?;
            for pair in source.pairs::<Value, Value>() {
                let (k, v) = pair?;
                copy.raw_set(k, v)?;
            }
            for name in &lists {
                let v: Value = all.raw_get(name.as_str())?;
                if !v.is_nil() {
                    copy.raw_set(name.as_str(), v)?;
                }
            }
            Ok(copy)
        }
        _ => Ok(source),
    }
}

/// Lower-cased media type of the request's `Content-Type`, parameters dropped.
fn media_type(headers: &mlua::Table) -> mlua::Result<Option<String>> {
    let ct: Option<mlua::LuaString> = headers.raw_get("content-type")?;
    Ok(ct.map(|ct| {
        let raw = String::from_utf8_lossy(&ct.as_bytes()).into_owned();
        raw.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    }))
}

const FORM_TYPE: &str = "application/x-www-form-urlencoded";

fn is_json_type(mt: &str) -> bool {
    mt == "application/json" || mt.ends_with("+json")
}

/// Pairs of an urlencoded body, for `req.form()` / `req.form_all()`. Raises
/// after `read(n)` or for any other content type.
fn form_pairs(
    state: &Mutex<BodyStream>,
    headers: &mlua::Table,
    fname: &str,
) -> mlua::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let st = state.lock().expect("body stream mutex poisoned");
    if st.streamed {
        return Err(mlua::Error::runtime(format!(
            "{fname} is unavailable after req.read(n) consumed part of the body"
        )));
    }
    match media_type(headers)? {
        Some(mt) if mt == FORM_TYPE => {}
        other => {
            return Err(mlua::Error::runtime(format!(
                "{fname}: content type is {}, expected {FORM_TYPE}",
                match other {
                    Some(mt) => format!("`{mt}`"),
                    None => "missing".to_owned(),
                }
            )));
        }
    }
    Ok(parse_query(&String::from_utf8_lossy(&st.body)))
}

/// What `req.valid_body` reads its body from.
#[derive(Clone)]
struct BodyReq {
    state: Arc<Mutex<BodyStream>>,
    headers: mlua::Table,
    decode: Function,
}

/// The body as `lur.validate.try` input, or the abort that rejects it.
enum BodyInput {
    /// `empty_json`: an empty JSON body, which only a `nil`-taking schema accepts.
    Ready {
        input: Value,
        empty_json: bool,
    },
    Rejected(Abort<'static>),
}

impl BodyReq {
    /// Synchronous on purpose: the body lock must not be held across an await.
    fn read(&self, lua: &Lua, schema: &Value) -> mlua::Result<BodyInput> {
        let st = self.state.lock().expect("body stream mutex poisoned");
        if st.streamed {
            return Err(mlua::Error::runtime(
                "req.valid_body is unavailable after req.read(n) consumed part of the body",
            ));
        }
        let mt = media_type(&self.headers)?;
        match mt.as_deref() {
            Some(mt) if is_json_type(mt) => json_input(lua, &self.decode, &st.body),
            Some(FORM_TYPE) => form_input(lua, &st.body, schema),
            other => unsupported_input(lua, other),
        }
    }
}

fn json_input(lua: &Lua, decode: &Function, body: &[u8]) -> mlua::Result<BodyInput> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(BodyInput::Ready {
            input: Value::Nil,
            empty_json: true,
        });
    }
    let raw = lua.create_string(body)?;
    match decode.call::<Value>(raw) {
        Ok(input) => Ok(BodyInput::Ready {
            input,
            empty_json: false,
        }),
        Err(mlua::Error::MemoryError(m)) => Err(mlua::Error::MemoryError(m)),
        Err(_) => Ok(BodyInput::Rejected(Abort {
            status: 400,
            message: "invalid JSON body",
            data: Value::Nil,
        })),
    }
}

fn form_input(lua: &Lua, body: &[u8], schema: &Value) -> mlua::Result<BodyInput> {
    let pairs = parse_query(&String::from_utf8_lossy(body));
    let source = last_table(lua, &pairs)?;
    let input = feed_lists(lua, source, schema, || all_table(lua, &pairs))?;
    Ok(BodyInput::Ready {
        input: Value::Table(input),
        empty_json: false,
    })
}

fn unsupported_input(lua: &Lua, media_type: Option<&str>) -> mlua::Result<BodyInput> {
    let data = match media_type {
        Some(mt) => {
            let data = lua.create_table()?;
            data.set("content_type", mt)?;
            Value::Table(data)
        }
        None => Value::Nil,
    };
    Ok(BodyInput::Rejected(Abort {
        status: 415,
        message: "unsupported content type",
        data,
    }))
}

/// `req.valid_body(schema, opts?)`: decode the body by `Content-Type` (JSON or
/// urlencoded), then `lur.validate.try`; failures abort like `req.valid_query`.
fn valid_body_fn(
    lua: &Lua,
    state: &Arc<Mutex<BodyStream>>,
    headers: &mlua::Table,
) -> mlua::Result<Value> {
    const F: &str = "req.valid_body";

    let ctx = serve_ctx(lua, F)?;
    let factory = ctx.factory.clone();
    let try_fn = ctx.try_fn.clone();
    let decode: Function = lua
        .globals()
        .get::<mlua::Table>("lur")?
        .get::<mlua::Table>("json")?
        .get("decode")?;
    let body = BodyReq {
        state: Arc::clone(state),
        headers: headers.clone(),
        decode,
    };

    let inner = lua.create_async_function(move |lua, (schema, opts): (Value, Value)| {
        let ctx = ctx.clone();
        let try_fn = try_fn.clone();
        let body = body.clone();
        async move {
            let fail_status = fail_status_opt(opts, F)?;
            let (input, empty_json) = match body.read(&lua, &schema)? {
                BodyInput::Ready { input, empty_json } => (input, empty_json),
                BodyInput::Rejected(abort) => return aborted(&lua, &ctx, abort),
            };

            let (ok, value): (bool, Value) = try_fn.call_async((schema, input)).await?;
            if ok {
                return Ok((true, value));
            }
            // An empty JSON body is only acceptable to a schema taking `nil`.
            let abort = if empty_json {
                Abort {
                    status: 400,
                    message: "invalid JSON body",
                    data: Value::Nil,
                }
            } else {
                Abort {
                    status: fail_status,
                    message: "validation failed",
                    data: issues_of(&value)?,
                }
            };
            aborted(&lua, &ctx, abort)
        }
    })?;
    Ok(Value::Function(crate::capabilities::raise::raising(
        &factory, inner,
    )?))
}

/// `streamed` is set once `read(n)` runs.
struct BodyStream {
    body: Vec<u8>,
    cursor: usize,
    streamed: bool,
}

/// Ordered, percent-decoded `(key, value)` pairs; `+` decodes to a space.
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

/// Raw bytes out (no UTF-8 assumption); an invalid `%` escape is left verbatim.
/// `+` is mapped before decoding so that `%2B` stays a literal plus.
fn percent_decode(input: &[u8], plus_as_space: bool) -> Vec<u8> {
    if plus_as_space {
        let spaced: Vec<u8> = input
            .iter()
            .map(|&b| if b == b'+' { b' ' } else { b })
            .collect();
        percent_encoding::percent_decode(&spaced).collect()
    } else {
        percent_encoding::percent_decode(input).collect()
    }
}

/// The first value must be a table; `status` defaults to 200 (must be in
/// `100..=599`), `body` to empty.
fn response_from(values: MultiValue) -> Result<Response, RunError> {
    let Some(Value::Table(table)) = values.into_iter().next() else {
        return Err(RunError::Script(mlua::Error::RuntimeError(
            "handler must return a table { status, headers, body }".into(),
        )));
    };

    // `as u16` would silently wrap an out-of-range status into a valid-looking
    // code on the wire (-1 → 65535, 65736 → 200), so validate instead.
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
    let headers = match table.get::<Value>("headers").map_err(RunError::Script)? {
        Value::Nil => Vec::new(),
        Value::Table(t) => headers_from(&t).map_err(RunError::Script)?,
        other => {
            return Err(RunError::Script(mlua::Error::RuntimeError(format!(
                "response headers must be a table, got {}",
                other.type_name()
            ))));
        }
    };

    Ok(Response {
        status,
        headers,
        body,
    })
}

/// `{ [name] = value | { value, ... } }` → flat pairs, array order kept.
/// Invalid names/values (incl. CR/LF) error, so a bad header is a 500 rather
/// than a split response. Framing headers are hyper's job: a handler-set
/// `Content-Length` could disagree with the body.
fn headers_from(table: &mlua::Table) -> mlua::Result<Vec<(HeaderName, HeaderValue)>> {
    let err = |msg: String| mlua::Error::RuntimeError(format!("response headers: {msg}"));
    let mut out = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::String(key) = key else {
            return Err(err(format!(
                "name must be a string, got {}",
                key.type_name()
            )));
        };
        let name = HeaderName::from_bytes(&key.as_bytes())
            .map_err(|e| err(format!("{e}: {:?}", key.to_string_lossy())))?;
        if name == CONTENT_LENGTH || name == TRANSFER_ENCODING {
            return Err(err(format!("{name} is set by the server")));
        }
        let parse = |v: &Value| match v {
            Value::String(s) => {
                HeaderValue::from_bytes(&s.as_bytes()).map_err(|e| err(format!("{e} for {name}")))
            }
            other => Err(err(format!(
                "value for {name} must be a string or array of strings, got {}",
                other.type_name()
            ))),
        };
        match &value {
            Value::Table(values) => {
                let len = values.raw_len();
                if values.pairs::<Value, Value>().count() != len {
                    return Err(err(format!("values for {name} must be an array")));
                }
                for i in 1..=len {
                    out.push((name.clone(), parse(&values.raw_get::<Value>(i)?)?));
                }
            }
            other => out.push((name.clone(), parse(other)?)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_keeps_raw_bytes_and_bad_escapes() {
        assert_eq!(percent_decode(b"a%20b", false), b"a b");
        assert_eq!(percent_decode(b"%E4%BD%A0", false), "你".as_bytes());
        assert_eq!(percent_decode(b"%FF", false), [0xFF], "not forced to UTF-8");
        assert_eq!(percent_decode(b"100%", false), b"100%");
        assert_eq!(percent_decode(b"%zz%4", false), b"%zz%4");
        assert_eq!(
            percent_decode(b"a%41", false),
            b"aA",
            "escape at the very end"
        );
        assert_eq!(percent_decode(b"a+b", false), b"a+b", "paths keep +");
    }

    #[test]
    fn query_plus_is_a_space_but_encoded_plus_is_not() {
        assert_eq!(
            parse_query("q=a+b%2Bc&flag&e="),
            vec![
                (b"q".to_vec(), b"a b+c".to_vec()),
                (b"flag".to_vec(), Vec::new()),
                (b"e".to_vec(), Vec::new()),
            ]
        );
    }
}
