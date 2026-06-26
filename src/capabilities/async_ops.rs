//! `lur.async` — concurrency helpers (spec §7). v1 ships `sleep`; the
//! combinators (`all`/`race`/`settled`/`any`) are a later slice.

use std::time::Duration;

use mlua::{Lua, Table};

use crate::runtime::RunError;

/// Install `lur.async.sleep`.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let async_tbl = lua.create_table().map_err(RunError::Init)?;

    // `lur.async.sleep(ms)` parks on the tokio timer; while parked no Lua runs,
    // so this is the path the wall-clock timeout layer (not the interrupt)
    // guards (§5).
    let sleep = lua
        .create_async_function(|_, ms: u64| async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(())
        })
        .map_err(RunError::Init)?;
    async_tbl.set("sleep", sleep).map_err(RunError::Init)?;

    lur.set("async", async_tbl).map_err(RunError::Init)?;
    Ok(())
}
