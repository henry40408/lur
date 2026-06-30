//! `lur.log` — leveled diagnostic logging to stderr (stdout is the data
//! channel). `lur.log.info/warn/error(msg)` (spec §4).

use std::io::Write;

use mlua::{Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// Install `lur.log` with `info` / `warn` / `error`.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let log = lua.create_table().map_err(RunError::Init)?;
    for level in ["info", "warn", "error"] {
        let fname = format!("lur.log.{level}");
        let f = lua
            .create_function(move |lua, msg: Value| {
                let msg: mlua::String = argcheck::arg(lua, msg, &fname, 1, "string")?;
                // Bytes pass through verbatim (§4); no UTF-8 validation.
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "{level}: ");
                let _ = err.write_all(&msg.as_bytes());
                let _ = err.write_all(b"\n");
                Ok(())
            })
            .map_err(RunError::Init)?;
        log.set(level, f).map_err(RunError::Init)?;
    }
    lur.set("log", log).map_err(RunError::Init)?;
    Ok(())
}
