//! `lur.fs` — policy-gated filesystem access (spec §4/§5).
//!
//! Every call is checked against the [`Policy`] read/write allowlists, which
//! canonicalize the path before the prefix check. Data is raw bytes in both
//! directions; paths are raw bytes too (no encoding assumed).

use std::path::PathBuf;
use std::sync::Arc;

use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::policy::Policy;
use crate::runtime::RunError;

/// Install `lur.fs.read` / `lur.fs.write`, gated by `policy`.
pub fn install(lua: &Lua, lur: &Table, policy: Arc<Policy>) -> Result<(), RunError> {
    let fs = lua.create_table().map_err(RunError::Init)?;

    let read_policy = Arc::clone(&policy);
    let read = lua
        .create_function(move |lua, path: Value| {
            let path: mlua::LuaString = argcheck::arg(lua, path, "lur.fs.read", 1, "string")?;
            let requested = bytes_to_path(&path.as_bytes());
            let resolved = read_policy
                .allows_read(&requested)
                .map_err(|e| Error::runtime(e.to_string()))?;
            let data = std::fs::read(&resolved)
                .map_err(|e| Error::runtime(format!("lur.fs.read: {e}")))?;
            lua.create_string(&data)
        })
        .map_err(RunError::Init)?;
    fs.set("read", read).map_err(RunError::Init)?;

    let write_policy = Arc::clone(&policy);
    let write = lua
        .create_function(move |lua, (path, data): (Value, Value)| {
            let path: mlua::LuaString = argcheck::arg(lua, path, "lur.fs.write", 1, "string")?;
            let data: mlua::LuaString = argcheck::arg(lua, data, "lur.fs.write", 2, "string")?;
            let requested = bytes_to_path(&path.as_bytes());
            let resolved = write_policy
                .allows_write(&requested)
                .map_err(|e| Error::runtime(e.to_string()))?;
            std::fs::write(&resolved, data.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.fs.write: {e}")))?;
            Ok(())
        })
        .map_err(RunError::Init)?;
    fs.set("write", write).map_err(RunError::Init)?;

    lur.set("fs", fs).map_err(RunError::Init)?;
    Ok(())
}

/// Build a `PathBuf` from raw bytes — paths carry no encoding (§4).
#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}
