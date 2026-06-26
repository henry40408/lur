//! `lur.null` — the singleton sentinel for SQL NULL / JSON null (spec §4/§6).
//!
//! It is a distinct value, not `nil`: a `nil` in a Lua table means "absent",
//! whereas `lur.null` round-trips an explicit null through JSON and SQL.

use mlua::{Lua, Table, UserData, Value};

use crate::runtime::RunError;

/// Zero-sized marker type backing the sentinel.
pub struct Null;

impl UserData for Null {}

/// Registry key under which the singleton is stored so host callbacks (e.g.
/// JSON decode) can produce the very same value the script sees as `lur.null`.
const REGISTRY_KEY: &str = "lur.null";

/// Install `lur.null` and stash the singleton in the named registry.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let null = lua.create_userdata(Null).map_err(RunError::Init)?;
    lua.set_named_registry_value(REGISTRY_KEY, &null)
        .map_err(RunError::Init)?;
    lur.set("null", null).map_err(RunError::Init)?;
    Ok(())
}

/// Fetch the `lur.null` singleton value.
pub fn value(lua: &Lua) -> mlua::Result<Value> {
    lua.named_registry_value(REGISTRY_KEY)
}

/// Whether `v` is the `lur.null` sentinel.
pub fn is_null(v: &Value) -> bool {
    matches!(v, Value::UserData(ud) if ud.is::<Null>())
}
