//! `lur.time` — what `os.*` lacks: millisecond wall/monotonic clocks, RFC 3339 /
//! RFC 2822 / HTTP-date parsing, lenient parsing of scraped dates, and
//! timezone-aware formatting (`os.date` only knows local time and UTC).
//!
//! ## Design notes
//!
//! - Integer milliseconds everywhere. `os.date("!…")` knows only local time and UTC, but feeds
//!   need RFC 2822 with offsets and scraped sites need `Asia/Taipei`-style zones, so
//!   `format*`/`parse` take an optional `tz` (IANA via `chrono-tz`, or a fixed offset).
//! - `parse` without a format accepts a short fixed list on purpose (no guessing `dd/mm` vs
//!   `mm/dd`: ambiguous input needs an explicit `fmt`). A DST gap raises; an overlap picks the
//!   earlier instant.
//! - `try_parse` returns `nil` for unparseable text and for `nil` text (a missing scraped
//!   node), but still raises on a bad `fmt`/`tz`: those are script bugs, not data. A
//!   well-formed `fmt` that can never yield a timestamp (`"%H:%M"`) returns `nil`, where
//!   `parse` would raise.

mod zoned;

use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// `monotonic_ms` epoch; only differences are meaningful.
static MONOTONIC_START: LazyLock<Instant> = LazyLock::new(Instant::now);

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let time = lua.create_table()?;

    install_clocks(lua, &time)?;
    install_parsers(lua, &time)?;
    zoned::install(lua, &time)?;

    lur.set("time", time)?;
    Ok(())
}

/// `lur.time.now_ms` / `lur.time.monotonic_ms`.
fn install_clocks(lua: &Lua, time: &Table) -> Result<(), RunError> {
    let now_ms = lua.create_function(|_, ()| {
        let dur = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_e| {
            Error::runtime("lur.time.now_ms: system clock is before the unix epoch")
        })?;
        Ok(dur.as_millis() as i64)
    })?;
    time.set("now_ms", now_ms)?;

    let monotonic_ms =
        lua.create_function(|_, ()| Ok(MONOTONIC_START.elapsed().as_millis() as i64))?;
    time.set("monotonic_ms", monotonic_ms)?;

    Ok(())
}

/// `lur.time.parse_rfc3339` / `lur.time.parse_http_date`.
fn install_parsers(lua: &Lua, time: &Table) -> Result<(), RunError> {
    let parse_rfc3339 = lua.create_function(|lua, s: Value| {
        let s: mlua::LuaString = argcheck::arg(
            lua,
            s,
            argcheck::ArgSpec {
                fname: "lur.time.parse_rfc3339",
                n: 1,
                expected: "string",
            },
        )?;
        let s = s
            .to_str()
            .map_err(|e| Error::runtime(format!("lur.time.parse_rfc3339: {e}")))?;
        let dt = chrono::DateTime::parse_from_rfc3339(&s)
            .map_err(|e| Error::runtime(format!("lur.time.parse_rfc3339: {e}")))?;
        Ok(dt.timestamp_millis())
    })?;
    time.set("parse_rfc3339", parse_rfc3339)?;

    let parse_http_date = lua.create_function(|lua, s: Value| {
        let s: mlua::LuaString = argcheck::arg(
            lua,
            s,
            argcheck::ArgSpec {
                fname: "lur.time.parse_http_date",
                n: 1,
                expected: "string",
            },
        )?;
        let s = s
            .to_str()
            .map_err(|e| Error::runtime(format!("lur.time.parse_http_date: {e}")))?;
        let t = httpdate::parse_http_date(&s)
            .map_err(|e| Error::runtime(format!("lur.time.parse_http_date: {e}")))?;
        let dur = t.duration_since(UNIX_EPOCH).map_err(|_e| {
            Error::runtime("lur.time.parse_http_date: date is before the unix epoch")
        })?;
        Ok(dur.as_millis() as i64)
    })?;
    time.set("parse_http_date", parse_http_date)?;

    Ok(())
}
