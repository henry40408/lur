//! `lur.env(name)` — allowlisted env vars (spec §4/§5). Denied and unset both
//! return `nil`, so it can't probe which variables exist.

use std::sync::Arc;

use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::policy::Policy;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table, policy: Arc<Policy>) -> Result<(), RunError> {
    let env = lua
        .create_function(move |lua, name: Value| {
            let name: mlua::LuaString = argcheck::arg(lua, name, "lur.env", 1, "string")?;
            let bytes = name.as_bytes();
            let name = std::str::from_utf8(&bytes)
                .map_err(|e| Error::runtime(format!("lur.env: variable name is not UTF-8: {e}")))?;
            if policy.allows_env(name) {
                match std::env::var(name) {
                    Ok(value) => Ok(Value::String(lua.create_string(&value)?)),
                    Err(_) => Ok(Value::Nil),
                }
            } else {
                Ok(Value::Nil)
            }
        })
        .map_err(RunError::Init)?;
    lur.set("env", env).map_err(RunError::Init)?;
    Ok(())
}
