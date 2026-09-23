//! `lur.base64` — standard base64 (spec §4); how binary crosses UTF-8-only JSON.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let base64 = lua.create_table().map_err(RunError::Init)?;

    let encode = lua
        .create_function(|lua, data: Value| {
            let data: mlua::LuaString = argcheck::arg(lua, data, "lur.base64.encode", 1, "string")?;
            lua.create_string(STANDARD.encode(data.as_bytes()))
        })
        .map_err(RunError::Init)?;
    base64.set("encode", encode).map_err(RunError::Init)?;

    let decode = lua
        .create_function(|lua, text: Value| {
            let text: mlua::LuaString = argcheck::arg(lua, text, "lur.base64.decode", 1, "string")?;
            let bytes = STANDARD
                .decode(text.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.base64.decode: {e}")))?;
            lua.create_string(&bytes)
        })
        .map_err(RunError::Init)?;
    base64.set("decode", decode).map_err(RunError::Init)?;

    lur.set("base64", base64).map_err(RunError::Init)?;
    Ok(())
}
