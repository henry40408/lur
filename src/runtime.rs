//! The shared execution core: a single sandboxed Luau VM.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, VmState};
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
        self.lua.load(source).exec().map_err(RunError::Script)
    }

    /// Run `source` to completion, interrupting it if it runs longer than
    /// `timeout` of wall-clock time.
    pub fn run_with_timeout(&self, source: &str, timeout: Duration) -> Result<(), RunError> {
        let at = Instant::now() + timeout;
        *self.deadline.lock().expect("deadline mutex poisoned") = Some(at);

        let result = self.lua.load(source).exec();

        *self.deadline.lock().expect("deadline mutex poisoned") = None;

        result.map_err(|e| {
            if Instant::now() >= at {
                RunError::Timeout
            } else {
                RunError::Script(e)
            }
        })
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
