//! The shared execution core: a single sandboxed Luau VM.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, MultiValue, Value, VmState};
use thiserror::Error;

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
}

/// Default per-VM memory cap applied by [`Runtime::new`].
pub const DEFAULT_MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;

/// Configuration for building a [`Runtime`].
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Per-VM memory cap in bytes (0 means unlimited).
    pub memory_limit: usize,
    /// The script's argument vector — everything after the script path. Parsed
    /// into `lur.args.flags` / `lur.args.positional`.
    pub args: Vec<String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            memory_limit: DEFAULT_MEMORY_LIMIT_BYTES,
            args: Vec::new(),
        }
    }
}

/// A single sandboxed Luau VM that can execute scripts.
pub struct Runtime {
    lua: Lua,
    /// When set, the interrupt hook keeps raising past this instant so that no
    /// `pcall`-loop can outlive the deadline.
    deadline: Arc<Mutex<Option<Instant>>>,
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
        let lua = Lua::new();
        // `require` survives `sandbox(true)` and loads on-disk .luau files,
        // bypassing the capability layer — strip it before freezing globals.
        lua.globals()
            .set("require", mlua::Value::Nil)
            .map_err(RunError::Init)?;

        crate::capabilities::install(&lua, &config)?;

        lua.sandbox(true).map_err(RunError::Init)?;

        let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let hook_deadline = Arc::clone(&deadline);
        lua.set_interrupt(move |_lua| {
            match *hook_deadline.lock().expect("deadline mutex poisoned") {
                Some(at) if Instant::now() >= at => {
                    // Keep raising on every interrupt past the deadline: the
                    // outermost driving loop cannot wrap itself in pcall, so it
                    // cannot swallow this.
                    Err(mlua::Error::RuntimeError("lur: deadline exceeded".into()))
                }
                _ => Ok(VmState::Continue),
            }
        });

        // Apply the memory cap last, after construction/sandbox/injection have
        // done their own allocations.
        lua.set_memory_limit(config.memory_limit)
            .map_err(RunError::Init)?;

        Ok(Self { lua, deadline })
    }

    // ---------------------------------------------------------------------

    /// Run `source` to completion with no time limit.
    pub fn run(&self, source: &str) -> Result<(), RunError> {
        self.guarded(None, |lua| lua.load(source).exec())
    }

    /// Run `source` to completion, interrupting it if it runs longer than
    /// `timeout` of wall-clock time.
    pub fn run_with_timeout(&self, source: &str, timeout: Duration) -> Result<(), RunError> {
        self.guarded(Some(timeout), |lua| lua.load(source).exec())
    }

    /// Run `source` and map its top-level `return` value to a process exit
    /// code per spec §8: a number → that code, `nil`/`false` → 1, anything
    /// else (including no `return`) → 0.
    pub fn run_to_exit_code(
        &self,
        source: &str,
        timeout: Option<Duration>,
    ) -> Result<i32, RunError> {
        let values = self.guarded(timeout, |lua| lua.load(source).eval::<MultiValue>())?;
        Ok(exit_code_of(values))
    }

    /// Set the deadline around `f`, then classify any error as a timeout if the
    /// deadline has passed, or a script error otherwise.
    fn guarded<T>(
        &self,
        timeout: Option<Duration>,
        f: impl FnOnce(&Lua) -> mlua::Result<T>,
    ) -> Result<T, RunError> {
        let at = timeout.map(|d| Instant::now() + d);
        *self.deadline.lock().expect("deadline mutex poisoned") = at;

        let result = f(&self.lua);

        *self.deadline.lock().expect("deadline mutex poisoned") = None;

        result.map_err(|e| {
            if is_memory_error(&e) {
                RunError::OutOfMemory
            } else if matches!(at, Some(at) if Instant::now() >= at) {
                RunError::Timeout
            } else {
                RunError::Script(e)
            }
        })
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
