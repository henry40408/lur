//! `lur.null` — sentinel for SQL/JSON null, distinct from `nil`
//! (which means absent).

use mlua::{Lua, Table, UserData, Value};

use crate::runtime::RunError;

pub struct Null;

impl UserData for Null {}

/// Registry key so host code can return the same singleton the script sees.
const REGISTRY_KEY: &str = "lur.null";

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let null = lua.create_userdata(Null)?;
    lua.set_named_registry_value(REGISTRY_KEY, &null)?;
    lur.set("null", null)?;
    Ok(())
}

pub fn value(lua: &Lua) -> mlua::Result<Value> {
    lua.named_registry_value(REGISTRY_KEY)
}

pub fn is_null(v: &Value) -> bool {
    matches!(v, Value::UserData(ud) if ud.is::<Null>())
}
