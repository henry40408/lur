//! The flat `lur.*` capability surface installed into the VM (spec §4).
//!
//! Each submodule installs its slice of the single `lur` table; [`install`]
//! orchestrates them and must run before `sandbox(true)` freezes the globals.

pub mod args;
pub mod async_ops;
pub mod base64;
pub mod cookie;
pub mod crypto;
pub mod db;
pub mod env;
pub mod fs;
pub mod http;
pub mod io;
pub mod json;
pub mod log;
pub mod null;
pub mod serve;
pub mod state;

use mlua::Lua;

use crate::runtime::{RunError, RuntimeConfig};

/// Build the flat `lur` table and install it as the only global capability.
///
/// `serve_registry` is `Some` only under `lur serve`; it makes `lur.serve.http`
/// collect routes instead of raising the one-shot registration error.
pub fn install(
    lua: &Lua,
    config: &RuntimeConfig,
    serve_registry: Option<&serve::Registry>,
) -> Result<(), RunError> {
    let lur = lua.create_table().map_err(RunError::Init)?;

    null::install(lua, &lur)?;
    log::install(lua, &lur)?;
    json::install(lua, &lur)?;
    base64::install(lua, &lur)?;
    crypto::install(lua, &lur)?;
    cookie::install(lua, &lur)?;
    io::install(lua, &lur)?;
    fs::install(lua, &lur, config.policy.clone())?;
    http::install(lua, &lur, config.policy.clone(), config.max_http_body)?;
    env::install(lua, &lur, config.policy.clone())?;
    db::install(lua, &lur, config.db_path.clone())?;
    async_ops::install(lua, &lur, config.max_concurrency)?;
    args::install(lua, &lur, &config.args)?;
    serve::install(lua, &lur, serve_registry)?;
    state::install(lua, &lur, config.state.clone())?;

    lua.globals().set("lur", lur).map_err(RunError::Init)?;
    Ok(())
}
