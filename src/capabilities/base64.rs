//! `lur.base64` — standard base64; how binary crosses UTF-8-only JSON.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let base64 = lua.create_table()?;

    let encode = lua.create_function(|lua, data: Value| {
        let data: mlua::LuaString = argcheck::arg(
            lua,
            data,
            argcheck::ArgSpec {
                fname: "lur.base64.encode",
                n: 1,
                expected: "string",
            },
        )?;
        lua.create_string(STANDARD.encode(data.as_bytes()))
    })?;
    base64.set("encode", encode)?;

    let decode = lua.create_function(|lua, text: Value| {
        let text: mlua::LuaString = argcheck::arg(
            lua,
            text,
            argcheck::ArgSpec {
                fname: "lur.base64.decode",
                n: 1,
                expected: "string",
            },
        )?;
        let bytes = STANDARD
            .decode(text.as_bytes())
            .map_err(|e| Error::runtime(format!("lur.base64.decode: {e}")))?;
        lua.create_string(&bytes)
    })?;
    base64.set("decode", decode)?;

    lur.set("base64", base64)?;
    Ok(())
}
