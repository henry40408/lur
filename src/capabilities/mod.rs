//! The flat `lur.*` capability table. [`install`] must run before
//! `sandbox(true)` freezes the globals; it also freezes each direct child of
//! `lur` (deeper tables are frozen by the capability that creates them).

pub(crate) mod argcheck;
pub mod args;
pub mod async_ops;
pub mod base64;
pub mod charset;
pub mod cookie;
pub mod crypto;
pub mod db;
pub mod env;
pub mod feed;
pub mod fs;
pub mod html;
pub mod http;
pub mod io;
pub mod json;
pub mod kv;
pub mod log;
pub mod null;
pub(crate) mod raise;
pub mod serve;
pub mod state;
mod storage;
pub mod text;
pub mod time;
pub mod url;
pub mod validate;
pub mod xml;

use mlua::{Lua, Table, Value};

use crate::runtime::{RunError, RuntimeConfig};

/// `serve_registry` is `Some` only under `lur serve`; otherwise `lur.serve.*` raises.
pub fn install(
    lua: &Lua,
    config: &RuntimeConfig,
    serve_registry: Option<&serve::Registry>,
) -> Result<(), RunError> {
    let lur = lua.create_table().map_err(RunError::Init)?;
    // One table-raising wrapper factory per VM, shared by every capability.
    let factory = raise::factory(lua).map_err(RunError::Init)?;

    null::install(lua, &lur)?;
    log::install(lua, &lur)?;
    json::install(lua, &lur)?;
    base64::install(lua, &lur)?;
    crypto::install(lua, &lur)?;
    cookie::install(lua, &lur)?;
    time::install(lua, &lur)?;
    url::install(lua, &lur)?;
    charset::install(lua, &lur)?;
    html::install(lua, &lur)?;
    xml::install(lua, &lur)?;
    feed::install(lua, &lur)?;
    validate::install(lua, &lur, &factory)?;
    io::install(lua, &lur)?;
    fs::install(lua, &lur, config.policy.clone())?;
    http::install(
        lua,
        &lur,
        config.policy.clone(),
        config.max_http_body,
        config.http_cache.clone(),
    )?;
    env::install(lua, &lur, config.policy.clone())?;
    let shared = db::install(lua, &lur, config.db_path.clone())?;
    kv::install(lua, &lur, &shared)?;
    async_ops::install(lua, &lur, &factory, config.max_concurrency)?;
    args::install(lua, &lur, &config.args)?;
    serve::install(lua, &lur, &factory, serve_registry)?;
    state::install(lua, &lur, config.state.clone())?;
    text::install(lua, &lur, config.replacers.clone(), config.memory_limit)?;

    freeze_children(&lur);
    lua.globals().set("lur", lur).map_err(RunError::Init)?;
    Ok(())
}

/// Mark the capability tables read-only. `luaL_sandbox` freezes only `lur`
/// itself, so without this a script could overwrite `lur.json.encode` and the
/// write would persist on a pooled VM. Tables nested one level deeper
/// (`lur.crypto.hex`, `lur.args.flags`, ...) are frozen by their own capability
/// before being attached; `every_lur_table_is_deeply_frozen` (one-shot) and
/// `lur_tree_is_deeply_frozen_inside_a_serve_handler` walk the whole
/// tree so a new unfrozen one fails CI. (A generic recursive walk was tried and
/// cost ~10% of VM cold start: every function value becomes a registry ref.)
fn freeze_children(lur: &Table) {
    for (_, v) in lur.pairs::<Value, Value>().flatten() {
        if let Value::Table(t) = v {
            t.set_readonly(true);
        }
    }
}
