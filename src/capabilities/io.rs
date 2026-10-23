//! `lur.stdin` / `lur.stdout` — raw-byte data channels. The script can't
//! pick a file or fd, so these are safe under `strict`.

use std::io::{BufRead, Read, Write};

use mlua::{Error, Function, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    install_stdin(lua, lur)?;
    install_stdout(lua, lur)?;
    Ok(())
}

fn install_stdin(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let stdin = lua.create_table().map_err(RunError::Init)?;
    stdin
        .set("read", stdin_read_fn(lua).map_err(RunError::Init)?)
        .map_err(RunError::Init)?;
    stdin
        .set("lines", stdin_lines_fn(lua).map_err(RunError::Init)?)
        .map_err(RunError::Init)?;
    lur.set("stdin", stdin).map_err(RunError::Init)?;
    Ok(())
}

/// `read()` → rest of stdin ("" at EOF); `read(n)` → up to n bytes, nil at EOF.
fn stdin_read_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(|lua, n: Value| {
        let n: Option<usize> = argcheck::arg(
            lua,
            n,
            argcheck::ArgSpec {
                fname: "lur.stdin.read",
                n: 1,
                expected: "number",
            },
        )?;
        let stdin = std::io::stdin();
        let Some(n) = n else {
            let mut buf = Vec::new();
            stdin
                .lock()
                .read_to_end(&mut buf)
                .map_err(|e| Error::runtime(e.to_string()))?;
            return Ok(Value::String(lua.create_string(&buf)?));
        };
        let buf = read_up_to(&mut stdin.lock(), n).map_err(|e| Error::runtime(e.to_string()))?;
        if buf.is_empty() && n > 0 {
            return Ok(Value::Nil);
        }
        Ok(Value::String(lua.create_string(&buf)?))
    })
}

/// `lines()` → iterator of lines with `\n` / `\r\n` stripped.
fn stdin_lines_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_function(|lua, ()| lua.create_function(|lua, ()| read_stdin_line(lua)))
}

/// The next line of stdin without its line ending, nil at EOF.
fn read_stdin_line(lua: &Lua) -> mlua::Result<Value> {
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
}

/// Read up to `n` bytes, across short reads, until `n` or EOF. The buffer
/// grows with the data actually read, so a huge `n` never pre-allocates.
/// The data lives on the Rust heap, outside the Lua memory limit.
fn read_up_to(r: &mut impl Read, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    r.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn install_stdout(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let stdout = lua.create_table().map_err(RunError::Init)?;

    let write = lua
        .create_function(|lua, data: Value| {
            let data: mlua::LuaString = argcheck::arg(
                lua,
                data,
                argcheck::ArgSpec {
                    fname: "lur.stdout.write",
                    n: 1,
                    expected: "string",
                },
            )?;
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

#[cfg(test)]
mod tests {
    use super::read_up_to;
    use std::io::{self, Cursor, Read};

    /// Yields one byte per `read`, with an `Interrupted` before each.
    struct Trickle<'a>(&'a [u8], bool);
    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.1 = !self.1;
            if self.1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match self.0.split_first() {
                Some((b, rest)) if !out.is_empty() => {
                    out[0] = *b;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn reads_exactly_n_when_available() {
        let mut r = Cursor::new(b"abcdef".to_vec());
        assert_eq!(read_up_to(&mut r, 4).unwrap(), b"abcd");
        assert_eq!(read_up_to(&mut r, 4).unwrap(), b"ef");
        assert_eq!(read_up_to(&mut r, 4).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn zero_reads_nothing() {
        let mut r = Cursor::new(b"abc".to_vec());
        assert_eq!(read_up_to(&mut r, 0).unwrap(), Vec::<u8>::new());
        assert_eq!(read_up_to(&mut r, 3).unwrap(), b"abc");
    }

    #[test]
    fn n_larger_than_input_stops_at_eof() {
        let mut r = Cursor::new(b"abc".to_vec());
        assert_eq!(read_up_to(&mut r, 1 << 20).unwrap(), b"abc");
    }

    #[test]
    fn short_reads_and_interrupts_are_filled() {
        let mut r = Trickle(b"hello", false);
        assert_eq!(read_up_to(&mut r, 3).unwrap(), b"hel");
        assert_eq!(read_up_to(&mut r, 10).unwrap(), b"lo");
    }

    #[test]
    fn huge_n_does_not_preallocate() {
        // Allocating `usize::MAX` bytes up front panics with a capacity
        // overflow, so a regression to pre-allocation fails fast here.
        let mut r = Cursor::new(b"abc".to_vec());
        assert_eq!(read_up_to(&mut r, usize::MAX).unwrap(), b"abc");
        assert_eq!(
            read_up_to(&mut Cursor::new(Vec::new()), usize::MAX).unwrap(),
            b""
        );
    }
}
