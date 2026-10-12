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
    /// Shared by every VM built from this config so `lur.state` spans the pool.
    pub state: Arc<crate::capabilities::state::StateStore>,
    /// Shared by every VM built from this config so `opts.cache` spans the pool.
    pub http_cache: Arc<crate::capabilities::http::HttpCache>,
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
            http_cache: Arc::default(),
            shutdown_grace: Duration::from_millis(DEFAULT_SHUTDOWN_GRACE_MS),
            max_concurrency: None,
            chunk_name: None,
        }
    }
}

/// When set, the interrupt hook keeps raising past this instant so no
/// `pcall`-loop can outlive it.
pub(crate) type Deadline = Arc<Mutex<Option<Instant>>>;

/// The VM's [`Deadline`], kept in `lua` app data.
pub(crate) struct VmDeadline(Deadline);

/// Whether `lua`'s current run is past its deadline (the interrupt layer has
/// fired or is about to). `false` with no deadline set.
pub(crate) fn deadline_passed(lua: &Lua) -> bool {
    lua.app_data_ref::<VmDeadline>().is_some_and(|d| {
        d.0.lock()
            .expect("deadline mutex poisoned")
            .is_some_and(|at| Instant::now() >= at)
    })
}

/// Chunk name of the Lua loop through which mlua drives every async host call
/// (`mlua::state::raw`); a test pins it so an mlua bump that renames it fails.
pub(crate) const ASYNC_POLL_SOURCE: &str = "=__mlua_async_poll";

/// Whether the interrupted frame is mlua's async poll loop. Only consulted
/// past a deadline.
fn in_async_poll(lua: &Lua) -> bool {
    lua.inspect_stack(0, |d| {
        d.source().source.as_deref() == Some(ASYNC_POLL_SOURCE)
    })
    .unwrap_or(false)
}

/// The interrupt past the deadline. Raise on every such interrupt, so a
/// pcall can swallow one but never escape, except in mlua's own poll loop:
/// raising there would kill the thread with its pending host futures still
/// on the stack, skipping their drop guards. That loop only polls and yields,
/// so the user code it returns to (CPU-bound) or the wall-clock layer
/// (parked) still ends the run. Kept out of line so the per-interrupt check
/// stays as small as it was before this path existed.
#[cold]
#[inline(never)]
fn past_deadline(lua: &Lua) -> mlua::Result<VmState> {
    if in_async_poll(lua) {
        Ok(VmState::Continue)
    } else {
        Err(mlua::Error::RuntimeError("lur: deadline exceeded".into()))
    }
}

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
    // the per-call environment that isolates server requests.
    for name in ["require", "getfenv", "setfenv", "loadstring"] {
        lua.globals()
            .set(name, mlua::Value::Nil)
            .map_err(RunError::Init)?;
    }

    crate::capabilities::install(&lua, config, serve)?;

    lua.sandbox(true).map_err(RunError::Init)?;

    let deadline: Deadline = Arc::new(Mutex::new(None));
    // Read back only by `lur.state.fetch` to tell its leader's own timeout
    // apart from an error its loader raised.
    lua.set_app_data(VmDeadline(Arc::clone(&deadline)));
    let hook_deadline = Arc::clone(&deadline);
    lua.set_interrupt(
        move |lua| match *hook_deadline.lock().expect("deadline mutex poisoned") {
            Some(at) if Instant::now() >= at => past_deadline(lua),
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

    /// Map the top-level `return` to an exit code: a number → that
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

    /// Drive `fut` under both timeout layers: the interrupt kills
    /// CPU-bound code, `tokio::time::timeout` kills code parked on async I/O.
    fn guarded<T>(
        &self,
        timeout: Option<Duration>,
        fut: impl Future<Output = mlua::Result<T>>,
    ) -> Result<T, RunError> {
        let at = timeout.map(|d| Instant::now() + d);
        *self.deadline.lock().expect("deadline mutex poisoned") = at;
        crate::capabilities::raise::reset(&self.lua);

        // Outer Err = the tokio wall-clock layer fired (I/O-parked code).
        let outcome: Result<mlua::Result<T>, ()> =
            self.rt
                .block_on(with_timeout(&self.lua, &self.deadline, timeout, fut));

        *self.deadline.lock().expect("deadline mutex poisoned") = None;
        // Don't keep a rendered error table alive between runs.
        crate::capabilities::raise::reset(&self.lua);

        let result = match outcome {
            Err(()) => Err(RunError::Timeout),
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(if is_memory_error(&e) {
                RunError::OutOfMemory
            } else if matches!(at, Some(at) if Instant::now() >= at) {
                RunError::Timeout
            } else {
                RunError::Script(e)
            }),
        };
        if matches!(result, Err(RunError::Timeout | RunError::OutOfMemory)) {
            reclaim(&self.lua);
        }
        result
    }
}

/// The wall-clock layer: await `fut` (a Lua call) for at most `timeout`
/// (`Err(())` once it elapses).
///
/// However this ends (completion, expiry, or this future being dropped), the
/// deadline is cleared *before* `fut` is dropped: mlua cancels a parked Lua
/// call by resuming it once with a terminate signal, and an interrupt still
/// raising past the deadline would abort that resume, orphaning the call's
/// pending host futures (and their drop guards, e.g. a `lur.state.fetch`
/// leader's) until the next GC. See [`reclaim`] for the other half.
///
/// After `fut` is dropped, [`state::end_run`](crate::capabilities::state::end_run)
/// removes any `lur.state.fetch` flight the run still leads (a leader parked
/// in a suspended coroutine), on every ending including cancellation.
pub(crate) async fn with_timeout<F: Future>(
    lua: &Lua,
    deadline: &Deadline,
    timeout: Option<Duration>,
    fut: F,
) -> Result<F::Output, ()> {
    // Drop order is the reverse of declaration: `_clear`, then `fut`, then
    // `_end`.
    let _end = EndRun(lua);
    let mut fut = std::pin::pin!(fut);
    let _clear = ClearDeadline(deadline);
    match timeout {
        Some(d) => tokio::time::timeout(d, fut.as_mut())
            .await
            .map_err(|_elapsed| ()),
        None => Ok(fut.as_mut().await),
    }
}

struct EndRun<'a>(&'a Lua);

impl Drop for EndRun<'_> {
    fn drop(&mut self) {
        crate::capabilities::state::end_run(self.0);
    }
}

struct ClearDeadline<'a>(&'a Deadline);

impl Drop for ClearDeadline<'_> {
    fn drop(&mut self) {
        if let Ok(mut d) = self.0.lock() {
            *d = None;
        }
    }
}

/// Call after a run that ended in a timeout or out of memory (with the
/// deadline cleared). The interrupt and the memory cap can raise inside
/// mlua's own async poll loop, killing the Lua thread with its pending host
/// futures still on its stack: nothing polls or drops them until a GC, and an
/// idle pooled VM may not collect for a long time. Their drop guards (a
/// `lur.state.fetch` leader handing off to its waiters) must run now. Only
/// these rare endings pay for the full collection.
pub(crate) fn reclaim(lua: &Lua) {
    let _ = lua.gc_collect();
}

pub(crate) fn is_memory_error(e: &mlua::Error) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The interrupt spares mlua's poll loop by its chunk name; an mlua bump
    /// that renames it must fail here, not silently strand drop guards.
    #[test]
    fn the_async_poll_loop_has_the_pinned_chunk_name() {
        let lua = Lua::new();
        let seen = Arc::new(Mutex::new(false));
        let hook_seen = Arc::clone(&seen);
        lua.set_interrupt(move |lua| {
            if in_async_poll(lua) {
                *hook_seen.lock().unwrap() = true;
            }
            Ok(VmState::Continue)
        });
        let pause = lua
            .create_async_function(|_, ()| async {
                tokio::task::yield_now().await;
                Ok(())
            })
            .unwrap();
        lua.globals().set("pause", pause).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(lua.load("for _ = 1, 3 do pause() end").exec_async())
            .unwrap();
        assert!(
            *seen.lock().unwrap(),
            "no interrupt ran in {ASYNC_POLL_SOURCE}"
        );
    }
}
