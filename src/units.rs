//! Human-readable size and duration parsing for the CLI (spec §12): `128m`,
//! `16m`, `5s`, `2m`, `500ms`. Sizes are binary (k/m/g = ×1024); durations
//! accept `ms`/`s`/`m`/`h`. A bare number is bytes (size) or seconds (duration).

use std::time::Duration;

/// Split a trimmed `<digits><unit>` string into its numeric and unit parts.
fn split_num_unit(s: &str) -> Result<(u64, &str), String> {
    let s = s.trim();
    let idx = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(idx);
    if num.is_empty() {
        return Err(format!("expected a number in {s:?}"));
    }
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid number in {s:?}"))?;
    Ok((n, unit.trim()))
}

/// Parse a byte size: bare/`b` = bytes, `k`/`kb` = ×1024, `m`/`mb` = ×1024²,
/// `g`/`gb` = ×1024³ (case-insensitive).
pub fn parse_size(s: &str) -> Result<usize, String> {
    let lower = s.to_ascii_lowercase();
    let (n, unit) = split_num_unit(&lower)?;
    let mult: u64 = match unit {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        other => return Err(format!("unknown size unit {other:?} (use b/k/m/g)")),
    };
    n.checked_mul(mult)
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| format!("size too large: {s:?}"))
}

/// Parse a duration: `ms` = milliseconds, bare/`s` = seconds, `m` = minutes,
/// `h` = hours (case-insensitive).
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let lower = s.to_ascii_lowercase();
    let (n, unit) = split_num_unit(&lower)?;
    let overflow = || format!("duration too large: {s:?}");
    let ms: u64 = match unit {
        "ms" => n,
        "" | "s" => n.checked_mul(1_000).ok_or_else(overflow)?,
        "m" => n.checked_mul(60_000).ok_or_else(overflow)?,
        "h" => n.checked_mul(3_600_000).ok_or_else(overflow)?,
        other => return Err(format!("unknown duration unit {other:?} (use ms/s/m/h)")),
    };
    Ok(Duration::from_millis(ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_handles_units_and_bare_bytes() {
        assert_eq!(parse_size("512"), Ok(512));
        assert_eq!(parse_size("512b"), Ok(512));
        assert_eq!(parse_size("4k"), Ok(4 * 1024));
        assert_eq!(parse_size("2m"), Ok(2 * 1024 * 1024));
        assert_eq!(parse_size("128M"), Ok(128 * 1024 * 1024));
        assert_eq!(parse_size("1g"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_size("16mb"), Ok(16 * 1024 * 1024));
    }

    #[test]
    fn parse_size_rejects_garbage() {
        assert!(parse_size("").is_err());
        assert!(parse_size("m").is_err());
        assert!(parse_size("12x").is_err());
        assert!(parse_size("1.5m").is_err());
    }

    #[test]
    fn parse_duration_handles_units_and_bare_seconds() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("5"), Ok(Duration::from_secs(5)));
        assert_eq!(parse_duration("5s"), Ok(Duration::from_secs(5)));
        assert_eq!(parse_duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
    }

    #[test]
    fn parse_duration_rejects_garbage() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("s").is_err());
        assert!(parse_duration("10x").is_err());
    }
}
