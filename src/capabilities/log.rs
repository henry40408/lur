//! `lur.log` — leveled diagnostic logging to stderr (stdout is the data
//! channel). `lur.log.info/warn/error(msg)`.

use std::io::Write;

use mlua::{Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let log = lua.create_table()?;
    for level in ["info", "warn", "error"] {
        let fname = format!("lur.log.{level}");
        let f = lua.create_function(move |lua, msg: Value| {
            let msg: mlua::LuaString = argcheck::arg(
                lua,
                msg,
                argcheck::ArgSpec {
                    fname: &fname,
                    n: 1,
                    expected: "string",
                },
            )?;
            // Bytes pass through verbatim; no UTF-8 validation.
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "{level}: ");
            let _ = err.write_all(&msg.as_bytes());
            let _ = err.write_all(b"\n");
            Ok(())
        })?;
        log.set(level, f)?;
    }
    lur.set("log", log)?;
    Ok(())
}
