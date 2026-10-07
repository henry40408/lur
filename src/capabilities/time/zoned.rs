//! Timezone-aware formatting and lenient parsing for `lur.time`.

use std::str::FromStr;

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz as Iana;
use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// A timezone: UTC, a fixed offset (`+08:00`), or an IANA name (`Asia/Taipei`).
#[derive(Clone, Copy)]
enum Zone {
    Fixed(FixedOffset),
    Iana(Iana),
}

impl Zone {
    const UTC: Self = Self::Fixed(FixedOffset::east_opt(0).unwrap());

    fn parse(text: &str) -> Result<Self, String> {
        if text.eq_ignore_ascii_case("utc") || text == "Z" {
            return Ok(Self::UTC);
        }
        if let Some(off) = parse_offset(text) {
            return Ok(Self::Fixed(off));
        }
        Iana::from_str(text)
            .map(Self::Iana)
            .map_err(|_e| format!("unknown timezone '{text}' (use an IANA name or +HH:MM)"))
    }

    fn at(self, ms: i64) -> Result<DateTime<FixedOffset>, String> {
        let utc = DateTime::<Utc>::from_timestamp_millis(ms)
            .ok_or_else(|| "timestamp out of range".to_owned())?;
        Ok(match self {
            Self::Fixed(off) => utc.with_timezone(&off),
            Self::Iana(tz) => utc.with_timezone(&tz).fixed_offset(),
        })
    }

    /// Interpret a wall-clock time in this zone; the earlier instant wins in a
    /// DST overlap, and a nonexistent (gap) time is an error.
    fn resolve(self, naive: NaiveDateTime) -> Result<i64, String> {
        let ms = match self {
            Self::Fixed(off) => off
                .from_local_datetime(&naive)
                .earliest()
                .map(|d| d.timestamp_millis()),
            Self::Iana(tz) => tz
                .from_local_datetime(&naive)
                .earliest()
                .map(|d| d.timestamp_millis()),
        };
        ms.ok_or_else(|| format!("local time '{naive}' does not exist in this timezone"))
    }
}

/// `+08:00`, `-0500`, `+08`.
fn parse_offset(text: &str) -> Option<FixedOffset> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m) = match digits.len() {
        2 => (digits.parse::<i32>().ok()?, 0),
        4 => (
            digits[..2].parse::<i32>().ok()?,
            digits[2..].parse::<i32>().ok()?,
        ),
        _ => return None,
    };
    if m >= 60 {
        return None;
    }
    FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

fn str_arg(lua: &Lua, value: Value, fname: &str, n: usize) -> mlua::Result<String> {
    let s: mlua::LuaString = argcheck::arg(lua, value, fname, n, "string")?;
    s.to_str()
        .map(|s| s.to_string())
        .map_err(|e| Error::runtime(format!("{fname}: {e}")))
}

/// Optional timezone argument; `nil` means UTC.
fn zone_arg(lua: &Lua, value: Value, fname: &str, n: usize) -> mlua::Result<Zone> {
    if matches!(value, Value::Nil) {
        return Ok(Zone::UTC);
    }
    let s = str_arg(lua, value, fname, n)?;
    Zone::parse(&s).map_err(|e| Error::runtime(format!("{fname}: {e}")))
}

fn ms_arg(value: Value, fname: &str) -> mlua::Result<i64> {
    argcheck::integer_arg(value, fname, 1)?
        .ok_or_else(|| Error::runtime(format!("{fname}: argument #1 must be integer, got nil")))
}

/// Wall-clock layouts tried by `parse` without a format, in order; results are
/// interpreted in the caller's zone.
const NAIVE_DATETIME: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M",
    "%Y/%m/%d %H:%M:%S",
    "%Y/%m/%d %H:%M",
];
const NAIVE_DATE: &[&str] = &["%Y-%m-%d", "%Y/%m/%d"];

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).expect("midnight is valid")
}

/// chrono panics when displaying an invalid strftime string, so reject it first.
fn check_format(fmt: &str) -> Result<(), String> {
    if StrftimeItems::new(fmt).any(|i| matches!(i, Item::Error)) {
        return Err(format!("invalid format '{fmt}'"));
    }
    Ok(())
}

fn parse_with_format(text: &str, fmt: &str, zone: Zone) -> Result<i64, String> {
    check_format(fmt)?;
    if let Ok(dt) = DateTime::parse_from_str(text, fmt) {
        return Ok(dt.timestamp_millis());
    }
    if let Ok(n) = NaiveDateTime::parse_from_str(text, fmt) {
        return zone.resolve(n);
    }
    if let Ok(d) = NaiveDate::parse_from_str(text, fmt) {
        return zone.resolve(midnight(d));
    }
    Err(format!("'{text}' does not match format '{fmt}'"))
}

fn parse_lenient(text: &str, zone: Zone) -> Result<i64, String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Ok(dt.timestamp_millis());
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(text) {
        return Ok(dt.timestamp_millis());
    }
    if let Ok(t) = httpdate::parse_http_date(text)
        && let Ok(d) = t.duration_since(std::time::UNIX_EPOCH)
    {
        return Ok(d.as_millis() as i64);
    }
    for fmt in NAIVE_DATETIME {
        if let Ok(n) = NaiveDateTime::parse_from_str(text, fmt) {
            return zone.resolve(n);
        }
    }
    for fmt in NAIVE_DATE {
        if let Ok(d) = NaiveDate::parse_from_str(text, fmt) {
            return zone.resolve(midnight(d));
        }
    }
    Err(format!("unrecognized date '{text}'; pass a format"))
}

pub fn install(lua: &Lua, time: &Table) -> Result<(), RunError> {
    let format_rfc3339 = lua
        .create_function(|lua, (ms, tz): (Value, Value)| {
            let f = "lur.time.format_rfc3339";
            let zone = zone_arg(lua, tz, f, 2)?;
            let dt = zone
                .at(ms_arg(ms, f)?)
                .map_err(|e| Error::runtime(format!("{f}: {e}")))?;
            Ok(dt.to_rfc3339_opts(SecondsFormat::Millis, true))
        })
        .map_err(RunError::Init)?;
    time.set("format_rfc3339", format_rfc3339)
        .map_err(RunError::Init)?;

    let format_rfc2822 = lua
        .create_function(|lua, (ms, tz): (Value, Value)| {
            let f = "lur.time.format_rfc2822";
            let zone = zone_arg(lua, tz, f, 2)?;
            let dt = zone
                .at(ms_arg(ms, f)?)
                .map_err(|e| Error::runtime(format!("{f}: {e}")))?;
            Ok(dt.to_rfc2822())
        })
        .map_err(RunError::Init)?;
    time.set("format_rfc2822", format_rfc2822)
        .map_err(RunError::Init)?;

    // `format(ms, fmt, tz?)` — strftime.
    let format = lua
        .create_function(|lua, (ms, fmt, tz): (Value, Value, Value)| {
            let f = "lur.time.format";
            let fmt = str_arg(lua, fmt, f, 2)?;
            let zone = zone_arg(lua, tz, f, 3)?;
            check_format(&fmt).map_err(|e| Error::runtime(format!("{f}: {e}")))?;
            let dt = zone
                .at(ms_arg(ms, f)?)
                .map_err(|e| Error::runtime(format!("{f}: {e}")))?;
            Ok(dt.format(&fmt).to_string())
        })
        .map_err(RunError::Init)?;
    time.set("format", format).map_err(RunError::Init)?;

    let parse_rfc2822 = lua
        .create_function(|lua, text: Value| {
            let f = "lur.time.parse_rfc2822";
            let text = str_arg(lua, text, f, 1)?;
            DateTime::parse_from_rfc2822(text.trim())
                .map(|d| d.timestamp_millis())
                .map_err(|e| Error::runtime(format!("{f}: {e}")))
        })
        .map_err(RunError::Init)?;
    time.set("parse_rfc2822", parse_rfc2822)
        .map_err(RunError::Init)?;

    // `parse(text, fmt?, tz?)` — with `fmt` it is strptime; without, the common
    // machine formats then a few scraped-site layouts. A wall-clock time with no
    // offset is read in `tz` (default UTC).
    let parse = lua
        .create_function(|lua, (text, fmt, tz): (Value, Value, Value)| {
            let f = "lur.time.parse";
            let text = str_arg(lua, text, f, 1)?;
            let text = text.trim();
            let zone = zone_arg(lua, tz, f, 3)?;
            let result = match fmt {
                Value::Nil => parse_lenient(text, zone),
                other => parse_with_format(text, &str_arg(lua, other, f, 2)?, zone),
            };
            result.map_err(|e| Error::runtime(format!("{f}: {e}")))
        })
        .map_err(RunError::Init)?;
    time.set("parse", parse).map_err(RunError::Init)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_offset;

    #[test]
    fn offsets_parse_with_and_without_colon() {
        assert_eq!(parse_offset("+08:00").unwrap().local_minus_utc(), 8 * 3600);
        assert_eq!(
            parse_offset("-0530").unwrap().local_minus_utc(),
            -(5 * 3600 + 1800)
        );
        assert_eq!(parse_offset("+08").unwrap().local_minus_utc(), 8 * 3600);
        assert!(parse_offset("+8").is_none());
        assert!(parse_offset("+08:60").is_none());
        assert!(parse_offset("Asia/Taipei").is_none());
    }
}
