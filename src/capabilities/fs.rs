//! `lur.fs` — filesystem access gated by the [`Policy`] allowlists: paths are
//! canonicalized to pick a root, then opened confined beneath that root's
//! directory handle. Data and paths are raw bytes.
//!
//! ## Design notes
//!
//! - Files are opened beneath a root's directory handle (`cap-std`) instead of
//!   canonicalize-then-`std::fs::open`, which had a TOCTOU window (a symlink swapped in after
//!   the check). Canonicalizing still picks the granting root, but the OS confines the open to
//!   that root, so losing the race can only reach files inside it. Cost: +17 KB binary.
//! - Rejected: hand-rolled `openat2` (`unsafe`, no macOS equivalent).
//! - Consequences: a file root is a handle on its parent plus the one allowed name; `--loose`
//!   is a handle on `/` (one code path); a dangling symlink pointing inside the root can be
//!   written through, one pointing out is refused at open time.

use std::path::PathBuf;
use std::sync::Arc;

use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::policy::Policy;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table, policy: Arc<Policy>) -> Result<(), RunError> {
    let fs = lua.create_table().map_err(RunError::Init)?;

    let read_policy = Arc::clone(&policy);
    let read = lua
        .create_function(move |lua, path: Value| {
            let path: mlua::LuaString = argcheck::arg(lua, path, "lur.fs.read", 1, "string")?;
            let requested = bytes_to_path(&path.as_bytes());
            let file = read_policy
                .open_read(&requested)
                .map_err(|e| Error::runtime(e.to_string()))?;
            let data = file
                .read()
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
            let file = write_policy
                .open_write(&requested)
                .map_err(|e| Error::runtime(e.to_string()))?;
            file.write(&data.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.fs.write: {e}")))?;
            Ok(())
        })
        .map_err(RunError::Init)?;
    fs.set("write", write).map_err(RunError::Init)?;

    lur.set("fs", fs).map_err(RunError::Init)?;
    Ok(())
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}
