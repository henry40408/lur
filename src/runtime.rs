//! The shared execution core: a single sandboxed Luau VM.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, MultiValue, Value, VmState};
use thiserror::Error;

use crate::capabilities::serve::Registry;
use crate::policy::Policy;

#[derive(Debug, Error)]
pub enum RunError {
    #[error("failed to initialize the runtime: {0}")]
    Init(#[source] mlua::Error),

    #[error("script error: {0}")]
    Script(#[source] mlua::Error),

    #[error("script exceeded its time limit")]
    Timeout,

    #[error("script exceeded its memory limit")]
    OutOfMemory,

    #[error("failed to start the async runtime: {0}")]
    AsyncRuntime(#[source] std::io::Error),
}

pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 10_000;

/// Fields marked *serve* are ignored in one-shot mode.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Bytes; 0 means unlimited.
    pub memory_limit: usize,
    /// Everything after the script path, exposed as `lur.args`.
    pub args: Vec<String>,
    pub policy: Arc<Policy>,
    /// Cap on a buffered `lur.http` response body.
    pub max_http_body: usize,
    /// *serve*: larger request bodies get a 413 before the handler runs.
    pub max_body: Option<usize>,
    /// `lur.db` / `lur.kv` raise when `None`.
    pub db_path: Option<PathBuf>,
    /// *serve*: VM pool size, which caps concurrent handlers.
    pub pool_size: usize,
    /// *serve*: per-request limit (a timed-out request gets a 503); also the
    /// default for cron jobs without their own `timeout`.
    pub per_event_timeout: Option<Duration>,
    /// Shared by every VM built from this config so `lur.state` spans the pool (spec §6).
    pub state: Arc<crate::capabilities::state::StateStore>,
    /// *serve*: drain window on shutdown before remaining work is aborted.
    pub shutdown_grace: Duration,
    /// Cap on in-flight `lur.async.*` tasks per VM; `None` is unbounded.
    pub max_concurrency: Option<usize>,
    /// Chunk name in error positions (`app.lua:2:`); `None` → `script`.
    pub chunk_name: Option<String>,
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
            chunk_name: None,
        }
    }
}

/// When set, the interrupt hook keeps raising past this instant so no
/// `pcall`-loop can outlive it.
pub(crate) type Deadline = Arc<Mutex<Option<Instant>>>;

/// Build a sandboxed VM for one-shot and the server pool. The step order is
/// load-bearing: strip globals → install capabilities → freeze → interrupt →
/// memory cap.
pub(crate) fn build_lua(
    config: &RuntimeConfig,
    serve: Option<&Registry>,
) -> Result<(Lua, Deadline), RunError> {
    let lua = Lua::new();
    // These survive `sandbox(true)`: `require` reads files bypassing lur.fs;
    // `getfenv`/`setfenv`/`loadstring` reach the writable global env, bypassing
    // the per-call environment that isolates server requests (spec §3, §5.1).
    for name in ["require", "getfenv", "setfenv", "loadstring"] {
        lua.globals()
            .set(name, mlua::Value::Nil)
            .map_err(RunError::Init)?;
    }

    crate::capabilities::install(&lua, config, serve)?;

    lua.sandbox(true).map_err(RunError::Init)?;

    let deadline: Deadline = Arc::new(Mutex::new(None));
    let hook_deadline = Arc::clone(&deadline);
    lua.set_interrupt(
        move |_lua| match *hook_deadline.lock().expect("deadline mutex poisoned") {
            Some(at) if Instant::now() >= at => {
                // Raise on every interrupt past the deadline, so a pcall
                // can swallow one but never escape.
                Err(mlua::Error::RuntimeError("lur: deadline exceeded".into()))
            }
            _ => Ok(VmState::Continue),
        },
    );

    // Last, so construction/sandbox/injection do not count against the cap.
    lua.set_memory_limit(config.memory_limit)
        .map_err(RunError::Init)?;

    Ok((lua, deadline))
}

/// A single sandboxed Luau VM (one-shot mode).
pub struct Runtime {
    lua: Lua,
    deadline: Deadline,
    /// Drives async host calls and the wall-clock timeout layer.
    rt: tokio::runtime::Runtime,
    /// `@`-prefixed.
    chunk_name: String,
}

impl Runtime {
    pub fn new() -> Result<Self, RunError> {
        Self::with_config(RuntimeConfig::default())
    }

    /// `memory_limit` of 0 means unlimited.
    pub fn with_memory_limit(memory_limit: usize) -> Result<Self, RunError> {
        Self::with_config(RuntimeConfig {
            memory_limit,
            ..Default::default()
        })
    }

    pub fn with_config(config: RuntimeConfig) -> Result<Self, RunError> {
        let (lua, deadline) = build_lua(&config, None)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(RunError::AsyncRuntime)?;
        let chunk_name = format!("@{}", config.chunk_name.as_deref().unwrap_or("script"));
        Ok(Self {
            lua,
            deadline,
            rt,
            chunk_name,
        })
    }

    pub fn run(&self, source: &str) -> Result<(), RunError> {
        self.guarded(
            None,
            self.lua
                .load(source)
                .set_name(&self.chunk_name)
                .exec_async(),
        )
    }

    pub fn run_with_timeout(&self, source: &str, timeout: Duration) -> Result<(), RunError> {
        self.guarded(
            Some(timeout),
            self.lua
                .load(source)
                .set_name(&self.chunk_name)
                .exec_async(),
        )
    }

    /// Map the top-level `return` to an exit code (spec §8): a number → that
    /// code, `nil`/`false` → 1, anything else (including no `return`) → 0.
    pub fn run_to_exit_code(
        &self,
        source: &str,
        timeout: Option<Duration>,
    ) -> Result<i32, RunError> {
        let values = self.guarded(
            timeout,
            self.lua
                .load(source)
                .set_name(&self.chunk_name)
                .eval_async::<MultiValue>(),
        )?;
        Ok(exit_code_of(values))
    }

    /// Drive `fut` under both timeout layers (spec §5): the interrupt kills
    /// CPU-bound code, `tokio::time::timeout` kills code parked on async I/O.
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
                Some(d) => tokio::time::timeout(d, fut).await.map_err(|_elapsed| ()),
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

fn is_memory_error(e: &mlua::Error) -> bool {
    match e {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } => is_memory_error(cause),
        _ => false,
    }
}

fn exit_code_of(values: MultiValue) -> i32 {
    match values.into_iter().next() {
        Some(Value::Integer(n)) => n as i32,
        Some(Value::Number(f)) => f as i32,
        Some(Value::Nil | Value::Boolean(false)) => 1,
        None | Some(_) => 0,
    }
}
