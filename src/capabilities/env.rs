//! `lur.env` — allowlisted environment-variable access (spec §4/§5).
//!
//! `lur.env(name)` returns the value if the name is on the policy allowlist,
//! else `nil` — indistinguishable from unset, so it can't be used as an oracle
//! for which variables exist.

use std::sync::Arc;

use mlua::{Error, Lua, Table, Value};

use crate::policy::Policy;
use crate::runtime::RunError;

/// Install `lur.env`, gated by `policy`.
pub fn install(lua: &Lua, lur: &Table, policy: Arc<Policy>) -> Result<(), RunError> {
    let env = lua
        .create_function(move |lua, name: mlua::String| {
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
