//! `lur.crypto` — hashing, HMAC, secure random, constant-time compare. Digests
//! are raw bytes; `lur.crypto.hex` converts to lowercase hex.
//!
//! ## Design notes
//!
//! **`lur.crypto`**: `hmac_md5` and `random_hex` omitted on purpose (extinct /
//! composable). `constant_eq` returns early on length mismatch (length isn't secret).

use hmac::{Hmac, KeyInit, Mac};
use md5::Md5;
use mlua::{Error, Lua, Table, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// Cap on one `random_bytes` draw.
const MAX_RANDOM_BYTES: i64 = 1 << 20;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let crypto = lua.create_table()?;

    install_hex(lua, &crypto)?;
    install_hashes(lua, &crypto)?;
    install_hmac(lua, &crypto)?;
    install_constant_eq(lua, &crypto)?;
    install_random(lua, &crypto)?;

    lur.set("crypto", crypto)?;
    Ok(())
}

/// One hashing function: raw bytes in, raw digest bytes out.
fn hash_fn<D: Digest>(lua: &Lua, fname: &'static str) -> Result<mlua::Function, RunError> {
    lua.create_function(move |lua, data: Value| {
        let data: mlua::LuaString = argcheck::arg(
            lua,
            data,
            argcheck::ArgSpec {
                fname,
                n: 1,
                expected: "string",
            },
        )?;
        lua.create_string(D::digest(data.as_bytes()).as_slice())
    })
    .map_err(RunError::from)
}

/// `lur.crypto.sha256` / `sha512` / `sha1` / `md5`.
fn install_hashes(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    crypto.set("sha256", hash_fn::<Sha256>(lua, "lur.crypto.sha256")?)?;
    crypto.set("sha512", hash_fn::<Sha512>(lua, "lur.crypto.sha512")?)?;
    crypto.set("sha1", hash_fn::<Sha1>(lua, "lur.crypto.sha1")?)?;
    crypto.set("md5", hash_fn::<Md5>(lua, "lur.crypto.md5")?)?;
    Ok(())
}

/// One HMAC function: `(key, msg)` in, raw MAC bytes out.
fn hmac_fn<D: hmac::EagerHash>(lua: &Lua, fname: &'static str) -> Result<mlua::Function, RunError> {
    lua.create_function(move |lua, (key, msg): (Value, Value)| {
        let key: mlua::LuaString = argcheck::arg(
            lua,
            key,
            argcheck::ArgSpec {
                fname,
                n: 1,
                expected: "string",
            },
        )?;
        let msg: mlua::LuaString = argcheck::arg(
            lua,
            msg,
            argcheck::ArgSpec {
                fname,
                n: 2,
                expected: "string",
            },
        )?;
        let mut mac = <Hmac<D> as KeyInit>::new_from_slice(&key.as_bytes())
            .map_err(|e| Error::runtime(format!("{fname}: {e}")))?;
        mac.update(&msg.as_bytes());
        lua.create_string(mac.finalize().into_bytes().as_slice())
    })
    .map_err(RunError::from)
}

/// `lur.crypto.hmac_sha256` / `hmac_sha512` / `hmac_sha1`.
fn install_hmac(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    crypto.set(
        "hmac_sha256",
        hmac_fn::<Sha256>(lua, "lur.crypto.hmac_sha256")?,
    )?;
    crypto.set(
        "hmac_sha512",
        hmac_fn::<Sha512>(lua, "lur.crypto.hmac_sha512")?,
    )?;
    crypto.set("hmac_sha1", hmac_fn::<Sha1>(lua, "lur.crypto.hmac_sha1")?)?;
    Ok(())
}

/// `lur.crypto.constant_eq` — timing-safe byte comparison.
fn install_constant_eq(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let constant_eq = lua.create_function(|lua, (a, b): (Value, Value)| {
        let a: mlua::LuaString = argcheck::arg(
            lua,
            a,
            argcheck::ArgSpec {
                fname: "lur.crypto.constant_eq",
                n: 1,
                expected: "string",
            },
        )?;
        let b: mlua::LuaString = argcheck::arg(
            lua,
            b,
            argcheck::ArgSpec {
                fname: "lur.crypto.constant_eq",
                n: 2,
                expected: "string",
            },
        )?;
        let a = a.as_bytes();
        let b = b.as_bytes();
        // Length is not secret; bail before the constant-time content compare.
        if a.len() != b.len() {
            return Ok(false);
        }
        Ok(bool::from(a.ct_eq(&b)))
    })?;
    crypto.set("constant_eq", constant_eq)?;
    Ok(())
}

/// `lur.crypto.random_bytes` — `n` bytes from the OS CSPRNG.
fn install_random(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let random_bytes = lua.create_function(|lua, n: Value| {
        let n: i64 = argcheck::arg(
            lua,
            n,
            argcheck::ArgSpec {
                fname: "lur.crypto.random_bytes",
                n: 1,
                expected: "number",
            },
        )?;
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
    })?;
    crypto.set("random_bytes", random_bytes)?;
    Ok(())
}

/// `lur.crypto.hex.encode` / `lur.crypto.hex.decode`.
fn install_hex(lua: &Lua, crypto: &Table) -> Result<(), RunError> {
    let hex = lua.create_table()?;

    let encode = lua.create_function(|lua, data: Value| {
        let data: mlua::LuaString = argcheck::arg(
            lua,
            data,
            argcheck::ArgSpec {
                fname: "lur.crypto.hex.encode",
                n: 1,
                expected: "string",
            },
        )?;
        lua.create_string(hex::encode(data.as_bytes()))
    })?;
    hex.set("encode", encode)?;

    let decode = lua.create_function(|lua, text: Value| {
        let text: mlua::LuaString = argcheck::arg(
            lua,
            text,
            argcheck::ArgSpec {
                fname: "lur.crypto.hex.decode",
                n: 1,
                expected: "string",
            },
        )?;
        let bytes = hex::decode(text.as_bytes())
            .map_err(|e| Error::runtime(format!("lur.crypto.hex.decode: {e}")))?;
        lua.create_string(&bytes)
    })?;
    hex.set("decode", decode)?;

    hex.set_readonly(true);
    crypto.set("hex", hex)?;
    Ok(())
}
