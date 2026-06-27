//! The shared execution core: a single sandboxed Luau VM.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, MultiValue, Value, VmState};
use thiserror::Error;

use crate::capabilities::serve::Registry;
use crate::policy::Policy;

/// Errors that can arise while building or running a script.
#[derive(Debug, Error)]
pub enum RunError {
    /// The VM could not be constructed or configured.
    #[error("failed to initialize the runtime: {0}")]
    Init(#[source] mlua::Error),

    /// The script raised an error during execution.
    #[error("script error: {0}")]
    Script(#[source] mlua::Error),

    /// The script exceeded its wall-clock deadline and was interrupted.
    #[error("script exceeded its time limit")]
    Timeout,

    /// The script tried to allocate past its memory cap.
    #[error("script exceeded its memory limit")]
    OutOfMemory,

    /// The async runtime backing I/O could not be started.
    #[error("failed to start the async runtime: {0}")]
    AsyncRuntime(#[source] std::io::Error),
}

/// Default per-VM memory cap applied by [`Runtime::new`].
pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;

/// Default cap on a buffered `lur.http` response body.
pub const DEFAULT_MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Default grace period for draining in-flight work on graceful shutdown.
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 10_000;

/// Configuration for building a [`Runtime`].
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Per-VM memory cap in bytes (0 means unlimited).
    pub memory_limit: usize,
    /// The script's argument vector — everything after the script path. Parsed
    /// into `lur.args.flags` / `lur.args.positional`.
    pub args: Vec<String>,
    /// Capability policy enforced by the gated `lur.*` modules. Shared into
    /// host callbacks, hence `Arc`.
    pub policy: Arc<Policy>,
    /// Cap on a buffered `lur.http` response body, in bytes.
    pub max_http_body: usize,
    /// Cap on the request body in server mode (`--max-body`); a larger request
    /// is rejected with 413 at the host edge before the handler runs (spec §3).
    /// `None` leaves the body unbounded (ignored in one-shot).
    pub max_body: Option<usize>,
    /// SQLite database path for `lur.db` / `lur.kv` (`--db`). `None` makes those
    /// modules raise a clear error when used.
    pub db_path: Option<PathBuf>,
    /// Number of pre-warmed VMs in the server-mode pool (ignored in one-shot).
    /// Each is checked out exclusively per request; the count caps concurrency.
    pub pool_size: usize,
    /// Per-request wall-clock limit in server mode (ignored in one-shot). `None`
    /// leaves handlers unbounded; on timeout the request gets a 5xx.
    pub per_event_timeout: Option<Duration>,
    /// Host-side `lur.state` store, shared by every VM built from this config so
    /// cross-request state survives the pool (spec §6).
    pub state: Arc<crate::capabilities::state::StateStore>,
    /// Grace period for draining in-flight requests/jobs on graceful shutdown
    /// (server mode only); after it elapses, remaining work is aborted (§3/§5).
    pub shutdown_grace: Duration,
    /// Cap on concurrently in-flight `lur.async.*` tasks per VM (`--max-concurrency`).
    /// `None` leaves the fan-out unbounded (spec §7/§9).
    pub max_concurrency: Option<usize>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            memory_limit: DEFAULT_MEMORY_LIMIT_BYTES,
            args: Vec::new(),
            policy: Arc::new(Policy::strict()),
            max_http_body: DEFAULT_MAX_HTTP_BODY_BYTES,
            max_body: None,
            db_path: None,
            pool_size: 1,
            per_event_timeout: None,
            state: Arc::new(crate::capabilities::state::StateStore::default()),
            shutdown_grace: Duration::from_millis(DEFAULT_SHUTDOWN_GRACE_MS),
            max_concurrency: None,
        }
    }
}

/// Shared deadline cell: when set, the interrupt hook keeps raising past this
/// instant so no `pcall`-loop can outlive it.
pub(crate) type Deadline = Arc<Mutex<Option<Instant>>>;

/// Build and configure a sandboxed Luau VM: strip `require`, install the `lur.*`
/// capabilities (with the optional serve registry), freeze globals, install the
/// deadline interrupt, and apply the memory cap. Shared by [`Runtime`] (one-shot)
/// and the server-mode VM pool.
pub(crate) fn build_lua(
    config: &RuntimeConfig,
    serve: Option<&Registry>,
) -> Result<(Lua, Deadline), RunError> {
    let lua = Lua::new();
    // `require` survives `sandbox(true)` and loads on-disk .luau files,
    // bypassing the capability layer — strip it before freezing globals.
    lua.globals()
        .set("require", mlua::Value::Nil)
        .map_err(RunError::Init)?;

    crate::capabilities::install(&lua, config, serve)?;

    lua.sandbox(true).map_err(RunError::Init)?;

    let deadline: Deadline = Arc::new(Mutex::new(None));
    let hook_deadline = Arc::clone(&deadline);
    lua.set_interrupt(
        move |_lua| match *hook_deadline.lock().expect("deadline mutex poisoned") {
            Some(at) if Instant::now() >= at => {
                // Keep raising on every interrupt past the deadline: the
                // outermost driving loop cannot wrap itself in pcall, so it
                // cannot swallow this.
                Err(mlua::Error::RuntimeError("lur: deadline exceeded".into()))
            }
            _ => Ok(VmState::Continue),
        },
    );

    // Apply the memory cap last, after construction/sandbox/injection have
    // done their own allocations.
    lua.set_memory_limit(config.memory_limit)
        .map_err(RunError::Init)?;

    Ok((lua, deadline))
}

/// A single sandboxed Luau VM that can execute scripts.
pub struct Runtime {
    lua: Lua,
    /// When set, the interrupt hook keeps raising past this instant so that no
    /// `pcall`-loop can outlive the deadline.
    deadline: Arc<Mutex<Option<Instant>>>,
    /// Current-thread runtime that drives async host calls (`lur.http`,
    /// `lur.async.sleep`, …) and the wall-clock timeout layer.
    rt: tokio::runtime::Runtime,
}

impl Runtime {
    /// Build a new sandboxed runtime with default configuration.
    pub fn new() -> Result<Self, RunError> {
        Self::with_config(RuntimeConfig::default())
    }

    /// Build a new sandboxed runtime capped at `memory_limit` bytes
    /// (0 means unlimited).
    pub fn with_memory_limit(memory_limit: usize) -> Result<Self, RunError> {
        Self::with_config(RuntimeConfig {
            memory_limit,
            ..Default::default()
        })
    }

    /// Build a new sandboxed runtime from an explicit [`RuntimeConfig`].
    pub fn with_config(config: RuntimeConfig) -> Result<Self, RunError> {
        let (lua, deadline) = build_lua(&config, None)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(RunError::AsyncRuntime)?;
        Ok(Self { lua, deadline, rt })
    }

    // ---------------------------------------------------------------------

    /// Run `source` to completion with no time limit.
    pub fn run(&self, source: &str) -> Result<(), RunError> {
        self.guarded(None, self.lua.load(source).exec_async())
    }

    /// Run `source` to completion, interrupting it if it runs longer than
    /// `timeout` of wall-clock time.
    pub fn run_with_timeout(&self, source: &str, timeout: Duration) -> Result<(), RunError> {
        self.guarded(Some(timeout), self.lua.load(source).exec_async())
    }

    /// Run `source` and map its top-level `return` value to a process exit
    /// code per spec §8: a number → that code, `nil`/`false` → 1, anything
    /// else (including no `return`) → 0.
    pub fn run_to_exit_code(
        &self,
        source: &str,
        timeout: Option<Duration>,
    ) -> Result<i32, RunError> {
        let values = self.guarded(timeout, self.lua.load(source).eval_async::<MultiValue>())?;
        Ok(exit_code_of(values))
    }

    /// Drive `fut` on the async runtime under both timeout layers (spec §5):
    /// the deadline-checking interrupt kills CPU-bound code, while
    /// `tokio::time::timeout` kills code parked on async I/O where the interrupt
    /// cannot fire. Any error is then classified.
    fn guarded<T>(
        &self,
        timeout: Option<Duration>,
        fut: impl Future<Output = mlua::Result<T>>,
    ) -> Result<T, RunError> {
        let at = timeout.map(|d| Instant::now() + d);
        *self.deadline.lock().expect("deadline mutex poisoned") = at;

        // Outer Err = the tokio wall-clock layer fired (I/O-parked code).
        let outcome: Result<mlua::Result<T>, ()> = self.rt.block_on(async {
            match timeout {
                Some(d) => tokio::time::timeout(d, fut).await.map_err(|_| ()),
                None => Ok(fut.await),
            }
        });

        *self.deadline.lock().expect("deadline mutex poisoned") = None;

        match outcome {
            Err(()) => Err(RunError::Timeout),
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(if is_memory_error(&e) {
                RunError::OutOfMemory
            } else if matches!(at, Some(at) if Instant::now() >= at) {
                RunError::Timeout
            } else {
                RunError::Script(e)
            }),
        }
    }
}

/// Whether `e` (or a cause it wraps) is a Lua out-of-memory error.
fn is_memory_error(e: &mlua::Error) -> bool {
    match e {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } => is_memory_error(cause),
        _ => false,
    }
}

/// Map a chunk's top-level return values to an exit code (spec §8).
fn exit_code_of(values: MultiValue) -> i32 {
    match values.into_iter().next() {
        None => 0,
        Some(Value::Integer(n)) => n as i32,
        Some(Value::Number(f)) => f as i32,
        Some(Value::Nil) | Some(Value::Boolean(false)) => 1,
        Some(_) => 0,
    }
}
