//! The flat `lur.*` capability surface installed into the VM (spec §4).
//!
//! Each submodule installs its slice of the single `lur` table; [`install`]
//! orchestrates them and must run before `sandbox(true)` freezes the globals.

pub mod args;
pub mod async_ops;
pub mod base64;
pub mod env;
pub mod fs;
pub mod http;
pub mod io;
pub mod json;
pub mod log;
pub mod null;

use mlua::Lua;

use crate::runtime::{RunError, RuntimeConfig};

/// Build the flat `lur` table and install it as the only global capability.
pub fn install(lua: &Lua, config: &RuntimeConfig) -> Result<(), RunError> {
    let lur = lua.create_table().map_err(RunError::Init)?;

    null::install(lua, &lur)?;
    log::install(lua, &lur)?;
    json::install(lua, &lur)?;
    base64::install(lua, &lur)?;
    io::install(lua, &lur)?;
    fs::install(lua, &lur, config.policy.clone())?;
    http::install(lua, &lur, config.policy.clone())?;
    env::install(lua, &lur, config.policy.clone())?;
    async_ops::install(lua, &lur)?;
    args::install(lua, &lur, &config.args)?;

    lua.globals().set("lur", lur).map_err(RunError::Init)?;
    Ok(())
}
