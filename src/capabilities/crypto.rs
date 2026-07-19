//! `lur.crypto` — hashing, HMAC, secure random, and constant-time compare.
//!
//! Pure-compute capability with no policy gate, in the spirit of `lur.base64`:
//! raw bytes in, raw digest bytes out. `lur.crypto.hex` bridges a raw digest to
//! the lowercase hex string most signatures are compared against.

use hmac::{Hmac, KeyInit, Mac};
use md5::Md5;
use mlua::{Error, Lua, Table, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// Upper bound on a single `random_bytes` draw (1 MiB) — a guard against a
/// script accidentally requesting an enormous allocation.
const MAX_RANDOM_BYTES: i64 = 1 << 20;

/// Install the flat `lur.crypto` table.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let crypto = lua.create_table().map_err(RunError::Init)?;

    install_hex(lua, &crypto)?;
    install_hashes(lua, &crypto)?;
    install_hmac(lua, &crypto)?;
    install_constant_eq(lua, &crypto)?;
    install_random(lua, &crypto)?;

    lur.set("crypto", crypto).map_err(RunError::Init)?;
    Ok(())
}

/// One hashing function: raw bytes in, raw digest bytes out.
fn hash_fn<D: Digest>(lua: &Lua, fname: &'static str) -> Result<mlua::Function, RunError> {
    lua.create_function(move |lua, data: Value| {
        let data: mlua::LuaString = argcheck::arg(lua, data, fname, 1, "string")?;
        lua.create_string(D::digest(data.as_bytes()).as_slice())
    })
    .map_err(RunError::Init)
}

/// `lur.crypto.sha256` / `sha512` / `sha1` / `md5`.
fn install_hashes(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    crypto
        .set("sha256", hash_fn::<Sha256>(lua, "lur.crypto.sha256")?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha512", hash_fn::<Sha512>(lua, "lur.crypto.sha512")?)
        .map_err(RunError::Init)?;
    crypto
        .set("sha1", hash_fn::<Sha1>(lua, "lur.crypto.sha1")?)
        .map_err(RunError::Init)?;
    crypto
        .set("md5", hash_fn::<Md5>(lua, "lur.crypto.md5")?)
        .map_err(RunError::Init)?;
    Ok(())
}

/// `lur.crypto.hmac_sha256` / `hmac_sha512` / `hmac_sha1`.
fn install_hmac(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let hmac_sha256 = lua
        .create_function(|lua, (key, msg): (Value, Value)| {
            let key: mlua::LuaString =
                argcheck::arg(lua, key, "lur.crypto.hmac_sha256", 1, "string")?;
            let msg: mlua::LuaString =
                argcheck::arg(lua, msg, "lur.crypto.hmac_sha256", 2, "string")?;
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
        .create_function(|lua, (key, msg): (Value, Value)| {
            let key: mlua::LuaString =
                argcheck::arg(lua, key, "lur.crypto.hmac_sha512", 1, "string")?;
            let msg: mlua::LuaString =
                argcheck::arg(lua, msg, "lur.crypto.hmac_sha512", 2, "string")?;
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
        .create_function(|lua, (key, msg): (Value, Value)| {
            let key: mlua::LuaString =
                argcheck::arg(lua, key, "lur.crypto.hmac_sha1", 1, "string")?;
            let msg: mlua::LuaString =
                argcheck::arg(lua, msg, "lur.crypto.hmac_sha1", 2, "string")?;
            let mut mac = Hmac::<Sha1>::new_from_slice(&key.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hmac_sha1: {e}")))?;
            mac.update(&msg.as_bytes());
            lua.create_string(mac.finalize().into_bytes().as_slice())
        })
        .map_err(RunError::Init)?;
    crypto.set("hmac_sha1", hmac_sha1).map_err(RunError::Init)?;

    Ok(())
}

/// `lur.crypto.constant_eq` — timing-safe byte comparison.
fn install_constant_eq(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let constant_eq = lua
        .create_function(|lua, (a, b): (Value, Value)| {
            let a: mlua::LuaString = argcheck::arg(lua, a, "lur.crypto.constant_eq", 1, "string")?;
            let b: mlua::LuaString = argcheck::arg(lua, b, "lur.crypto.constant_eq", 2, "string")?;
            let a = a.as_bytes();
            let b = b.as_bytes();
            // Length is not secret; bail before the constant-time content compare.
            if a.len() != b.len() {
                return Ok(false);
            }
            Ok(bool::from(a.ct_eq(&b)))
        })
        .map_err(RunError::Init)?;
    crypto
        .set("constant_eq", constant_eq)
        .map_err(RunError::Init)?;
    Ok(())
}

/// `lur.crypto.random_bytes` — `n` bytes from the OS CSPRNG.
fn install_random(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let random_bytes = lua
        .create_function(|lua, n: Value| {
            let n: i64 = argcheck::arg(lua, n, "lur.crypto.random_bytes", 1, "number")?;
            if n <= 0 {
                return Err(Error::runtime(
                    "lur.crypto.random_bytes: n must be a positive integer",
                ));
            }
            if n > MAX_RANDOM_BYTES {
                return Err(Error::runtime(format!(
                    "lur.crypto.random_bytes: n must be <= {MAX_RANDOM_BYTES}"
                )));
            }
            #[allow(
                clippy::cast_sign_loss,
                reason = "n is checked `> 0` and `<= MAX_RANDOM_BYTES` above"
            )]
            let mut buf = vec![0u8; n as usize];
            getrandom::fill(&mut buf)
                .map_err(|e| Error::runtime(format!("lur.crypto.random_bytes: {e}")))?;
            lua.create_string(&buf)
        })
        .map_err(RunError::Init)?;
    crypto
        .set("random_bytes", random_bytes)
        .map_err(RunError::Init)?;
    Ok(())
}

/// `lur.crypto.hex.encode` / `lur.crypto.hex.decode`.
fn install_hex(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let hex = lua.create_table().map_err(RunError::Init)?;

    let encode = lua
        .create_function(|lua, data: Value| {
            let data: mlua::LuaString =
                argcheck::arg(lua, data, "lur.crypto.hex.encode", 1, "string")?;
            lua.create_string(hex::encode(data.as_bytes()))
        })
        .map_err(RunError::Init)?;
    hex.set("encode", encode).map_err(RunError::Init)?;

    let decode = lua
        .create_function(|lua, text: Value| {
            let text: mlua::LuaString =
                argcheck::arg(lua, text, "lur.crypto.hex.decode", 1, "string")?;
            let bytes = hex::decode(text.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.crypto.hex.decode: {e}")))?;
            lua.create_string(&bytes)
        })
        .map_err(RunError::Init)?;
    hex.set("decode", decode).map_err(RunError::Init)?;

    crypto.set("hex", hex).map_err(RunError::Init)?;
    Ok(())
}
