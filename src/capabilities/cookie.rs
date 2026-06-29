//! `lur.cookie` — parse the `Cookie` request header and build `Set-Cookie`
//! values. Pure-compute capability, no policy gate, in the spirit of
//! `lur.base64`/`lur.crypto`: raw bytes in, raw bytes out, no automatic
//! percent-encoding. `serialize` validates its inputs so a malformed cookie
//! fails loudly rather than corrupting the response.

use mlua::{Lua, Table};

use crate::runtime::RunError;

/// Install the flat `lur.cookie` table (`parse` + `serialize`).
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let cookie = lua.create_table().map_err(RunError::Init)?;

    install_parse(lua, &cookie)?;
    install_serialize(lua, &cookie)?;

    lur.set("cookie", cookie).map_err(RunError::Init)?;
    Ok(())
}

/// Trim leading/trailing optional whitespace (space / tab) from a byte slice.
fn trim_ows(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if *first == b' ' || *first == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if *last == b' ' || *last == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// `lur.cookie.parse(header) -> { name = value, ... }`.
fn install_parse(lua: &Lua, cookie: &Table) -> Result<(), RunError> {
    let parse = lua
        .create_function(|lua, header: mlua::String| {
            let out = lua.create_table()?;
            let bytes = header.as_bytes();
            for segment in bytes.split(|&b| b == b';') {
                let segment = trim_ows(segment);
                let Some(eq) = segment.iter().position(|&b| b == b'=') else {
                    continue; // no '=' -> skip
                };
                let name = &segment[..eq];
                if name.is_empty() {
                    continue; // empty name -> skip
                }
                let value = &segment[eq + 1..];
                // Later duplicate overwrites earlier: plain table assignment.
                out.set(lua.create_string(name)?, lua.create_string(value)?)?;
            }
            Ok(out)
        })
        .map_err(RunError::Init)?;
    cookie.set("parse", parse).map_err(RunError::Init)?;
    Ok(())
}

/// `lur.cookie.serialize(name, value, opts?) -> string` (implemented in Task 2).
fn install_serialize(_lua: &Lua, _cookie: &Table) -> Result<(), RunError> {
    Ok(())
}
