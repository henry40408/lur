//! `lur.time` — millisecond clocks and timestamp parsing. Pure-compute
//! capability, no policy gate. Fills the gaps Luau's `os.*` cannot: sub-second
//! and monotonic timing, and parsing RFC 3339 / HTTP-date strings into numbers.
//! Formatting stays with `os.date` (which already emits both). All values are
//! integer milliseconds.

use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// Process-fixed reference for `monotonic_ms`, captured on first use. Only the
/// difference between two readings is meaningful.
static MONOTONIC_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Install the flat `lur.time` table.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let time = lua.create_table().map_err(RunError::Init)?;

    install_clocks(lua, &time)?;
    install_parsers(lua, &time)?;

    lur.set("time", time).map_err(RunError::Init)?;
    Ok(())
}

/// `lur.time.now_ms` / `lur.time.monotonic_ms`.
fn install_clocks(lua: &Lua, time: &Table) -> Result<(), RunError> {
    let now_ms = lua
        .create_function(|_, ()| {
            let dur = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_e| {
                Error::runtime("lur.time.now_ms: system clock is before the unix epoch")
            })?;
            Ok(dur.as_millis() as i64)
        })
        .map_err(RunError::Init)?;
    time.set("now_ms", now_ms).map_err(RunError::Init)?;

    let monotonic_ms = lua
        .create_function(|_, ()| Ok(MONOTONIC_START.elapsed().as_millis() as i64))
        .map_err(RunError::Init)?;
    time.set("monotonic_ms", monotonic_ms)
        .map_err(RunError::Init)?;

    Ok(())
}

/// `lur.time.parse_rfc3339` / `lur.time.parse_http_date`.
fn install_parsers(lua: &Lua, time: &Table) -> Result<(), RunError> {
    let parse_rfc3339 = lua
        .create_function(|lua, s: Value| {
            let s: mlua::String = argcheck::arg(lua, s, "lur.time.parse_rfc3339", 1, "string")?;
            let s = s
                .to_str()
                .map_err(|e| Error::runtime(format!("lur.time.parse_rfc3339: {e}")))?;
            let dt = chrono::DateTime::parse_from_rfc3339(&s)
                .map_err(|e| Error::runtime(format!("lur.time.parse_rfc3339: {e}")))?;
            Ok(dt.timestamp_millis())
        })
        .map_err(RunError::Init)?;
    time.set("parse_rfc3339", parse_rfc3339)
        .map_err(RunError::Init)?;

    let parse_http_date = lua
        .create_function(|lua, s: Value| {
            let s: mlua::String = argcheck::arg(lua, s, "lur.time.parse_http_date", 1, "string")?;
            let s = s
                .to_str()
                .map_err(|e| Error::runtime(format!("lur.time.parse_http_date: {e}")))?;
            let t = httpdate::parse_http_date(&s)
                .map_err(|e| Error::runtime(format!("lur.time.parse_http_date: {e}")))?;
            let dur = t.duration_since(UNIX_EPOCH).map_err(|_e| {
                Error::runtime("lur.time.parse_http_date: date is before the unix epoch")
            })?;
            Ok(dur.as_millis() as i64)
        })
        .map_err(RunError::Init)?;
    time.set("parse_http_date", parse_http_date)
        .map_err(RunError::Init)?;

    Ok(())
}
