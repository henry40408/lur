//! `lur.crypto` — hashing, HMAC, secure random, and constant-time compare.
//!
//! Pure-compute capability with no policy gate, in the spirit of `lur.base64`:
//! raw bytes in, raw digest bytes out. `lur.crypto.hex` bridges a raw digest to
//! the lowercase hex string most signatures are compared against.

use hmac::{Hmac, Mac};
use md5::Md5;
use mlua::{Error, Lua, Table};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::runtime::RunError;

/// Install the flat `lur.crypto` table.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let crypto = lua.create_table().map_err(RunError::Init)?;

    install_hex(lua, &crypto)?;
    install_hashes(lua, &crypto)?;
    install_hmac(lua, &crypto)?;

    lur.set("crypto", crypto).map_err(RunError::Init)?;
    Ok(())
}

/// One hashing function: raw bytes in, raw digest bytes out.
fn hash_fn<D: Digest>(lua: &Lua) -> Result<mlua::Function, RunError> {
    lua.create_function(|lua, data: mlua::String| {
        lua.create_string(D::digest(data.as_bytes()).as_slice())
    })
    .map_err(RunError::Init)
}

/// `lur.crypto.sha256` / `sha512` / `sha1` / `md5`.
fn install_hashes(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    crypto
        .set("sha256", hash_fn::<Sha256>(lua)?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha512", hash_fn::<Sha512>(lua)?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha1", hash_fn::<Sha1>(lua)?)
        .map_err(RunError::Init)?;
    crypto
        .set("md5", hash_fn::<Md5>(lua)?)
        .map_err(RunError::Init)?;
    Ok(())
}

/// `lur.crypto.hmac_sha256` / `hmac_sha512` / `hmac_sha1`.
fn install_hmac(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let hmac_sha256 = lua
        .create_function(|lua, (key, msg): (mlua::String, mlua::String)| {
            let mut mac = Hmac::<Sha256>::new_from_slice(&key.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hmac_sha256: {e}")))?;
            mac.update(&msg.as_bytes());
            lua.create_string(mac.finalize().into_bytes().as_slice())
        })
        .map_err(RunError::Init)?;
    crypto
        .set("hmac_sha256", hmac_sha256)
        .map_err(RunError::Init)?;

    let hmac_sha512 = lua
        .create_function(|lua, (key, msg): (mlua::String, mlua::String)| {
            let mut mac = Hmac::<Sha512>::new_from_slice(&key.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hmac_sha512: {e}")))?;
            mac.update(&msg.as_bytes());
            lua.create_string(mac.finalize().into_bytes().as_slice())
        })
        .map_err(RunError::Init)?;
    crypto
        .set("hmac_sha512", hmac_sha512)
        .map_err(RunError::Init)?;

    let hmac_sha1 = lua
        .create_function(|lua, (key, msg): (mlua::String, mlua::String)| {
            let mut mac = Hmac::<Sha1>::new_from_slice(&key.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hmac_sha1: {e}")))?;
            mac.update(&msg.as_bytes());
            lua.create_string(mac.finalize().into_bytes().as_slice())
        })
        .map_err(RunError::Init)?;
    crypto.set("hmac_sha1", hmac_sha1).map_err(RunError::Init)?;

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
