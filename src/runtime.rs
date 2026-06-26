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
}

/// A single sandboxed Luau VM that can execute scripts.
pub struct Runtime {
    lua: Lua,
    /// When set, the interrupt hook keeps raising past this instant so that no
    /// `pcall`-loop can outlive the deadline.
    deadline: Arc<Mutex<Option<Instant>>>,
}

impl Runtime {
    /// Build a new sandboxed runtime.
    pub fn new() -> Result<Self, RunError> {
        let lua = Lua::new();
        // `require` survives `sandbox(true)` and loads on-disk .luau files,
        // bypassing the capability layer — strip it before freezing globals.
        lua.globals()
            .set("require", mlua::Value::Nil)
            .map_err(RunError::Init)?;

        inject_capabilities(&lua)?;

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

        result.map_err(|e| match at {
            Some(at) if Instant::now() >= at => RunError::Timeout,
            _ => RunError::Script(e),
        })
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

/// Build the flat `lur.*` capability table and install it as a global.
///
/// Must run before `sandbox(true)` freezes the global table.
fn inject_capabilities(lua: &Lua) -> Result<(), RunError> {
    let lur = lua.create_table().map_err(RunError::Init)?;

    // `lur.log(msg)` — write a line to stderr. Bytes are passed through
    // verbatim (§4 byte semantics); no UTF-8 validation at this boundary.
    let log = lua
        .create_function(|_, msg: mlua::String| {
            use std::io::Write;
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(&msg.as_bytes());
            let _ = err.write_all(b"\n");
            Ok(())
        })
        .map_err(RunError::Init)?;
    lur.set("log", log).map_err(RunError::Init)?;

    lua.globals().set("lur", lur).map_err(RunError::Init)?;
    Ok(())
}
