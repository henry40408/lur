//! `lur.log` — diagnostic logging to stderr (stdout is the data channel).

use mlua::{Lua, Table};

use crate::runtime::RunError;

/// Install `lur.log(msg)`.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    // Bytes pass through verbatim (§4 byte semantics); no UTF-8 validation.
    let log = lua
        .create_function(|_, msg: mlua::String| {
            use std::io::Write;
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(&msg.as_bytes());
            let _ = err.write_all(b"\n");
            Ok(())
        })
        .map_err(RunError::Init)?;
    lur.set("log", log).map_err(RunError::Init)?;
    Ok(())
}
