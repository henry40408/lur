//! Raising a *table* as a Lua error value.
//!
//! A Rust callback cannot raise a table: `Err(..)` becomes a userdata, so
//! scripts could not read fields off the caught value. The one clean way is
//! Lua's own `error(tbl, 0)`, so a capability that fails with a structured
//! value is split in two:
//!
//! 1. a Rust function returning `(true, value)` or `(false, error_table)`;
//! 2. a wrapper from [`factory`] + [`raising`], a tiny Lua function that
//!    returns `value` or calls `error(error_table, 0)`.
//!
//! `pcall` then hands scripts the original table. Uncaught, mlua renders it
//! through `__tostring` ([`error_meta`] makes that return `message`).
//!
//! Crossing Rust: a table error raised inside a `lur.async` task reaches the
//! combinator as a plain string, because mlua's `call_async` renders a failed
//! thread's error value with `luaL_tolstring` (plus a traceback) and keeps
//! only the text. That rendering calls the table's `__tostring`, which is
//! ours ([`error_meta`]): it records the table in a per-VM [`Flattened`] slot,
//! and the combinator, on a task error, takes it back with [`recover`] and
//! re-raises it through a [`raising`] wrapper. Only error tables built with
//! [`error_meta`] survive; any other table error still arrives as a string.

use std::sync::OnceLock;

use mlua::{Function, Lua, Table};

/// The error table whose `__tostring` ran last, with the text it rendered.
/// Per VM (`lua` app data); taken by [`recover`] and cleared by [`reset`] at
/// the start and end of every run, so it never outlives the run that set it.
#[derive(Default)]
pub(crate) struct Flattened(Option<(Table, String)>);

/// Forget any recorded error table (start of a run on a pooled VM).
pub(crate) fn reset(lua: &Lua) {
    if let Ok(Some(mut slot)) = lua.try_app_data_mut::<Flattened>() {
        slot.0 = None;
    }
}

/// Whether `e` is the flattened form of an error whose `__tostring` is
/// `message`: exactly the text, or the text followed by a newline (where mlua
/// appends the traceback). Any text starting with `message` + `"\n"` matches;
/// only the script itself can produce such a string, and a match can only
/// hand it back a table it already held.
pub(crate) fn is_flattened(e: &mlua::Error, message: &str) -> bool {
    match e {
        mlua::Error::RuntimeError(s) => s
            .strip_prefix(message)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('\n')),
        mlua::Error::CallbackError { cause, .. } | mlua::Error::WithContext { cause, .. } => {
            is_flattened(cause, message)
        }
        _ => false,
    }
}

/// The error table `e` was flattened from, if it is the one recorded last.
/// Always empties the slot, so a recorded table is handed back at most once.
///
/// Called right after a task's thread fails: its `__tostring` ran during that
/// failure, after any other Lua, so a recorded table with the same text is
/// that error. A string error whose text equals a table error rendered
/// earlier in the run (and not consumed) would also match; only the script
/// itself can arrange that.
pub(crate) fn recover(lua: &Lua, e: &mlua::Error) -> Option<Table> {
    let (table, message) = lua.try_app_data_mut::<Flattened>().ok()??.0.take()?;
    is_flattened(e, &message).then_some(table)
}

/// Each wrapper is one closure call of the compiled factory.
const FACTORY: &str = "local error = error
return function(inner)
  return function(...)
    local ok, r = inner(...)
    if ok then return r end
    error(r, 0)
  end
end";

/// The wrapper factory; compile it once and pass it to [`raising`].
///
/// The source is compiled to bytecode once per process: parsing it per VM
/// measured +14% on `vm_cold_start`, loading bytecode is free.
pub(crate) fn factory(lua: &Lua) -> mlua::Result<Function> {
    static BYTECODE: OnceLock<Vec<u8>> = OnceLock::new();
    let code = if let Some(code) = BYTECODE.get() {
        code
    } else {
        let compiled = mlua::chunk::Compiler::new().compile(FACTORY)?;
        BYTECODE.get_or_init(|| compiled)
    };
    lua.load(code.as_slice())
        .set_name("=lur.raise")
        .set_mode(mlua::chunk::ChunkMode::Binary)
        .eval()
}

/// Wrap `inner` (returning `(ok, value_or_error_table)`) so a `false` raises
/// the table with `error(tbl, 0)`.
pub(crate) fn raising(factory: &Function, inner: Function) -> mlua::Result<Function> {
    factory.call(inner)
}

/// Metatable for error tables: `tostring(err)` is `err.message`. Locked:
/// read-only, and `getmetatable`/`setmetatable` on an error table are refused,
/// so a script cannot rewrite it for every later error on a pooled VM.
///
/// `__tostring` also records the table in the VM's [`Flattened`] slot (see
/// the module docs); the slot is created here, during install.
pub(crate) fn error_meta(lua: &Lua) -> mlua::Result<Table> {
    lua.set_app_data(Flattened::default());
    let meta = lua.create_table()?;
    meta.set(
        "__tostring",
        lua.create_function(|lua, t: Table| {
            let message = t
                .raw_get::<Option<String>>("message")?
                .unwrap_or_else(|| "error".to_owned());
            if let Ok(Some(mut slot)) = lua.try_app_data_mut::<Flattened>() {
                slot.0 = Some((t, message.clone()));
            }
            Ok(message)
        })?,
    )?;
    meta.set("__metatable", false)?;
    meta.set_readonly(true);
    Ok(meta)
}
