//! Raising a *table* as a Lua error value.
//!
//! A Rust callback cannot raise a table: `Err(..)` becomes a userdata (or a
//! string once it crosses `call_async`), so scripts could not read fields off
//! the caught value. The one clean way is Lua's own `error(tbl, 0)`, so a
//! capability that fails with a structured value is split in two:
//!
//! 1. a Rust function returning `(true, value)` or `(false, error_table)`;
//! 2. [`raising`], which wraps it in a tiny Lua function that returns
//!    `value` or calls `error(error_table, 0)`.
//!
//! `pcall` then hands scripts the original table. Uncaught, mlua renders it
//! through `__tostring` ([`error_meta`] makes that return `message`).
//!
//! Crossing Rust (`lur.async` calls tasks via `call_async`) would flatten the
//! table to a string, so [`rescue`] — a Lua trampoline run around each task —
//! turns a table error back into `(false, table)`, which Rust carries as
//! [`Raised`] and hands to a [`raising`] wrapper at the end.

use std::fmt;

use mlua::{Error, Function, Lua, Table, Value};

const RAISING: &str = "local inner = ...
local error = error
return function(...)
  local ok, r = inner(...)
  if ok then return r end
  error(r, 0)
end";

const RESCUE: &str = "local xpcall, error, type, traceback = xpcall, error, type, debug.traceback
local function handler(e)
  if type(e) == 'string' then return traceback(e, 2) end
  return e
end
return function(f, ...)
  local ok, r = xpcall(f, handler, ...)
  if ok then return true, r end
  if type(r) == 'table' then return false, r end
  error(r, 0)
end";

/// Wrap `inner` (returning `(ok, value_or_error_table)`) so a `false` raises
/// the table with `error(tbl, 0)`.
pub(crate) fn raising(lua: &Lua, inner: Function) -> mlua::Result<Function> {
    lua.load(RAISING).set_name("=lur.raise").call(inner)
}

/// A Lua function `(f, ...) -> (true, result) | (false, error_table)` that
/// re-raises every non-table error unchanged.
pub(crate) fn rescue(lua: &Lua) -> mlua::Result<Function> {
    lua.load(RESCUE).set_name("=lur.rescue").eval()
}

/// Metatable for error tables: `tostring(err)` is `err.message`.
pub(crate) fn error_meta(lua: &Lua) -> mlua::Result<Table> {
    let meta = lua.create_table()?;
    meta.set(
        "__tostring",
        lua.create_function(|_, t: Table| {
            Ok(t.raw_get::<Option<String>>("message")?
                .unwrap_or_else(|| "error".to_owned()))
        })?,
    )?;
    Ok(meta)
}

/// A table error carried across Rust (see [`rescue`]).
#[derive(Debug)]
pub(crate) struct Raised {
    pub table: Table,
    pub message: String,
}

impl Raised {
    pub(crate) fn new(table: Table) -> Self {
        let message = table
            .raw_get::<Option<String>>("message")
            .ok()
            .flatten()
            .unwrap_or_else(|| "error".to_owned());
        Self { table, message }
    }
}

impl fmt::Display for Raised {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Raised {}

/// The table behind an error produced from a [`rescue`]d task, if any.
pub(crate) fn raised_table(e: &Error) -> Option<Table> {
    e.downcast_ref::<Raised>().map(|r| r.table.clone())
}

/// `Ok(v)` → `(true, v)`; a [`Raised`] error → `(false, table)` for a
/// [`raising`] wrapper to re-raise; any other error stays an error.
pub(crate) fn split(r: mlua::Result<Value>) -> mlua::Result<(bool, Value)> {
    match r {
        Ok(v) => Ok((true, v)),
        Err(e) => match raised_table(&e) {
            Some(t) => Ok((false, Value::Table(t))),
            None => Err(e),
        },
    }
}
