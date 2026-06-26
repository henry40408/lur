//! `lur.base64` — standard base64 encode/decode (spec §4).
//!
//! The bridge for putting binary data through the UTF-8-only JSON boundary:
//! `lur.base64.encode` raw bytes, then `lur.json.encode` the resulting ASCII.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use mlua::{Error, Lua, Table};

use crate::runtime::RunError;

/// Install `lur.base64.encode` / `lur.base64.decode`.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let base64 = lua.create_table().map_err(RunError::Init)?;

    let encode = lua
        .create_function(|lua, data: mlua::String| {
            lua.create_string(STANDARD.encode(data.as_bytes()))
        })
        .map_err(RunError::Init)?;
    base64.set("encode", encode).map_err(RunError::Init)?;

    let decode = lua
        .create_function(|lua, text: mlua::String| {
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
