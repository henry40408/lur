//! `lur.stdin` / `lur.stdout` — raw-byte data channels (§4). The script can't
//! pick a file or fd, so these are safe under `strict`.

use std::io::{BufRead, Read, Write};

use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    install_stdin(lua, lur)?;
    install_stdout(lua, lur)?;
    Ok(())
}

fn install_stdin(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let stdin = lua.create_table().map_err(RunError::Init)?;

    // read() → rest of stdin ("" at EOF); read(n) → up to n bytes, nil at EOF.
    let read = lua
        .create_function(|lua, n: Value| {
            let n: Option<usize> = argcheck::arg(lua, n, "lur.stdin.read", 1, "number")?;
            let stdin = std::io::stdin();
            let mut buf = Vec::new();
            match n {
                None => {
                    stdin
                        .lock()
                        .read_to_end(&mut buf)
                        .map_err(|e| Error::runtime(e.to_string()))?;
                    Ok(Value::String(lua.create_string(&buf)?))
                }
                Some(n) => {
                    buf.resize(n, 0);
                    let read = read_up_to(&mut stdin.lock(), &mut buf)
                        .map_err(|e| Error::runtime(e.to_string()))?;
                    if read == 0 && n > 0 {
                        return Ok(Value::Nil);
                    }
                    Ok(Value::String(lua.create_string(&buf[..read])?))
                }
            }
        })
        .map_err(RunError::Init)?;
    stdin.set("read", read).map_err(RunError::Init)?;

    // lines() → iterator of lines with `\n` / `\r\n` stripped.
    let lines = lua
        .create_function(|lua, ()| {
            let iter = lua.create_function(|lua, ()| {
                let mut buf = Vec::new();
                let n = std::io::stdin()
                    .lock()
                    .read_until(b'\n', &mut buf)
                    .map_err(|e| Error::runtime(e.to_string()))?;
                if n == 0 {
                    return Ok(Value::Nil);
                }
                if buf.last() == Some(&b'\n') {
                    buf.pop();
                    if buf.last() == Some(&b'\r') {
                        buf.pop();
                    }
                }
                Ok(Value::String(lua.create_string(&buf)?))
            })?;
            Ok(iter)
        })
        .map_err(RunError::Init)?;
    stdin.set("lines", lines).map_err(RunError::Init)?;

    lur.set("stdin", stdin).map_err(RunError::Init)?;
    Ok(())
}

/// Fill `buf` across short reads until full or EOF; returns bytes read.
fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn install_stdout(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let stdout = lua.create_table().map_err(RunError::Init)?;

    let write = lua
        .create_function(|lua, data: Value| {
            let data: mlua::LuaString = argcheck::arg(lua, data, "lur.stdout.write", 1, "string")?;
            std::io::stdout()
                .lock()
                .write_all(&data.as_bytes())
                .map_err(|e| Error::runtime(e.to_string()))
        })
        .map_err(RunError::Init)?;
    stdout.set("write", write).map_err(RunError::Init)?;

    let flush = lua
        .create_function(|_, ()| {
            std::io::stdout()
                .lock()
                .flush()
                .map_err(|e| Error::runtime(e.to_string()))
        })
        .map_err(RunError::Init)?;
    stdout.set("flush", flush).map_err(RunError::Init)?;

    lur.set("stdout", stdout).map_err(RunError::Init)?;
    Ok(())
}
