//! `lur.crypto` — hashing, HMAC, secure random, and constant-time compare.
//!
//! Pure-compute capability with no policy gate, in the spirit of `lur.base64`:
//! raw bytes in, raw digest bytes out. `lur.crypto.hex` bridges a raw digest to
//! the lowercase hex string most signatures are compared against.

use mlua::{Error, Lua, Table};

use crate::runtime::RunError;

/// Install the flat `lur.crypto` table.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let crypto = lua.create_table().map_err(RunError::Init)?;

    install_hex(lua, &crypto)?;

    lur.set("crypto", crypto).map_err(RunError::Init)?;
    Ok(())
}

/// `lur.crypto.hex.encode` / `lur.crypto.hex.decode`.
fn install_hex(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let hex = lua.create_table().map_err(RunError::Init)?;

    let encode = lua
        .create_function(|lua, data: mlua::String| lua.create_string(hex::encode(data.as_bytes())))
        .map_err(RunError::Init)?;
    hex.set("encode", encode).map_err(RunError::Init)?;

    let decode = lua
        .create_function(|lua, text: mlua::String| {
            let bytes = hex::decode(text.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hex.decode: {e}")))?;
            lua.create_string(&bytes)
        })
        .map_err(RunError::Init)?;
    hex.set("decode", decode).map_err(RunError::Init)?;

    crypto.set("hex", hex).map_err(RunError::Init)?;
    Ok(())
}
