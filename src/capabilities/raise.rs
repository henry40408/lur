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
//! Limitation: a table error that crosses Rust (e.g. raised inside a
//! `lur.async` task, which mlua calls via `call_async`) reaches the other side
//! as a plain string (its `__tostring`), not a table.

use std::sync::OnceLock;

use mlua::{Function, Lua, Table};

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
pub(crate) fn error_meta(lua: &Lua) -> mlua::Result<Table> {
    let meta = lua.create_table()?;
    meta.set(
        "__tostring",
        lua.create_function(|_, t: Table| {
            Ok(t.raw_get::<Option<String>>("message")?
                .unwrap_or_else(|| "error".to_owned()))
        })?,
    )?;
    meta.set("__metatable", false)?;
    meta.set_readonly(true);
    Ok(meta)
}
