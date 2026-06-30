//! Human-readable, rustc-style rendering of script errors against their source.

use std::ffi::OsStr;
use std::io::IsTerminal;

/// ANSI styles for the rendered diagnostic. All fields are empty strings when
/// color is off, so the colored and plain code paths are identical save for the
/// (then-empty) escape sequences.
struct Palette {
    /// Bold red — the `error:` label and the caret.
    err: &'static str,
    /// Bold blue — the gutter (`|`, line numbers) and the `-->` arrow.
    gutter: &'static str,
    /// Reset back to the terminal default.
    reset: &'static str,
}

impl Palette {
    fn new(color: bool) -> Self {
        if color {
            Palette {
                err: "\x1b[1;31m",
                gutter: "\x1b[1;34m",
                reset: "\x1b[0m",
            }
        } else {
            Palette {
                err: "",
                gutter: "",
                reset: "",
            }
        }
    }
}

/// Decide whether to emit ANSI color from the `NO_COLOR` env value (if any) and
/// whether the target stream is a TTY. Color is on only when the stream is a TTY
/// and `NO_COLOR` is unset or empty (the de-facto standard: a non-empty
/// `NO_COLOR` disables color regardless of its value).
fn color_from_env(no_color: Option<&OsStr>, stream_is_tty: bool) -> bool {
    stream_is_tty && no_color.is_none_or(|v| v.is_empty())
}

/// Whether diagnostics written to stderr should be colorized, honoring
/// `NO_COLOR` and a non-TTY stderr (pipe/redirect).
pub fn stderr_color() -> bool {
    color_from_env(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stderr().is_terminal(),
    )
}

/// Render `displayed` (an mlua error's `Display`) against `source`, rustc-style.
/// `chunk_name` is the bare path used as the chunk name (no `@`). `color` enables
/// ANSI styling (callers pass [`stderr_color`]). Falls back to `lur: <body>` (the
/// label-stripped message) whenever a source snippet can't be rendered — whether
/// the location is unparsable or out of range. Never panics.
pub fn render(source: &str, chunk_name: &str, displayed: &str, color: bool) -> String {
    // Split off the traceback (runtime errors append one; syntax errors don't).
    let (head, traceback) = match displayed.split_once("\nstack traceback:") {
        Some((h, t)) => (h, Some(t)),
        None => (displayed, None),
    };

    // Strip a leading "<kind> error: " label if present.
    let body = head
        .strip_prefix("runtime error: ")
        .or_else(|| head.strip_prefix("syntax error: "))
        .unwrap_or(head);

    let parsed = parse_location(body, chunk_name);
    let Some((line, col, message)) = parsed else {
        return format!("lur: {body}");
    };
    let Some(src_line) = source.lines().nth(line - 1) else {
        // Location parsed but points past the source: fall back to the same
        // `lur: {body}` form as the unparsable case, keeping the location text.
        return format!("lur: {body}");
    };

    let p = Palette::new(color);
    let (g, e, r) = (p.gutter, p.err, p.reset);
    let mut out = String::new();
    out.push_str(&format!("{e}error:{r} {message}\n"));
    let pos = match col {
        Some(c) => format!("{chunk_name}:{line}:{c}"),
        None => format!("{chunk_name}:{line}"),
    };
    out.push_str(&format!(" {g}-->{r} {pos}\n"));

    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    out.push_str(&format!("{g}{pad} |{r}\n"));
    out.push_str(&format!("{g}{gutter} |{r} {src_line}\n"));
    // Caret: under `col` when known, else under the first non-whitespace char.
    let caret_col = col.unwrap_or_else(|| src_line.len() - src_line.trim_start().len() + 1);
    let caret_pad = " ".repeat(caret_col.saturating_sub(1));
    out.push_str(&format!("{g}{pad} |{r} {caret_pad}{e}^{r}\n"));

    if let Some(tb) = traceback {
        let kept: Vec<&str> = tb
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| l.trim() != "[C]: in ?")
            .collect();
        if !kept.is_empty() {
            out.push_str("stack traceback:\n");
            for l in kept {
                out.push_str(l);
                out.push('\n');
            }
        }
    }

    out.trim_end().to_string()
}

/// Parse `<chunk_name>:<line>[:<col>]: <message>` out of `body`. Anchors on the
/// known `chunk_name` prefix so paths containing `:` are safe. Returns
/// `(line, col, message)`.
fn parse_location(body: &str, chunk_name: &str) -> Option<(usize, Option<usize>, String)> {
    let needle = format!("{chunk_name}:");
    let idx = body.find(&needle)?;
    let rest = &body[idx + needle.len()..];

    // line digits
    let line_end = rest.find(|c: char| !c.is_ascii_digit())?;
    if line_end == 0 {
        return None;
    }
    let line: usize = rest[..line_end].parse().ok()?;
    if line == 0 {
        return None;
    }
    let after_line = &rest[line_end..];

    // optional :col
    let (col, after) = if let Some(tail) = after_line.strip_prefix(':') {
        let col_end = tail
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(tail.len());
        if col_end > 0 {
            (Some(tail[..col_end].parse().ok()?), &tail[col_end..])
        } else {
            (None, after_line)
        }
    } else {
        (None, after_line)
    };

    let message = after
        .strip_prefix(": ")
        .or_else(|| after.strip_prefix(':'))?;
    Some((line, col, message.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::{color_from_env, render};
    use std::ffi::OsStr;

    const SRC: &str = "local x = nil\nprint(x.y)\n";

    #[test]
    fn renders_runtime_error_with_snippet() {
        let displayed = "runtime error: app.lua:2: attempt to index nil with 'y'\n\
                         stack traceback:\n\tapp.lua:2: in main chunk\n\t[C]: in ?";
        let out = render(SRC, "app.lua", displayed, false);
        assert!(
            out.contains("error: attempt to index nil with 'y'"),
            "{out}"
        );
        assert!(out.contains("--> app.lua:2"), "{out}");
        assert!(out.contains("2 | print(x.y)"), "{out}");
        // noise frame filtered, real frame kept
        assert!(!out.contains("[C]: in ?"), "{out}");
        assert!(out.contains("in main chunk"), "{out}");
    }

    #[test]
    fn falls_back_to_plain_when_location_unparsable() {
        let displayed = "runtime error: something with no location";
        let out = render(SRC, "app.lua", displayed, false);
        assert_eq!(out, "lur: something with no location");
    }

    #[test]
    fn out_of_range_line_falls_back_to_plain() {
        // Uniform with the unparsable/line-zero cases: fall back to `lur: {body}`,
        // preserving the location text rather than dropping it.
        let displayed = "runtime error: app.lua:99: mystery";
        let out = render(SRC, "app.lua", displayed, false);
        assert_eq!(out, "lur: app.lua:99: mystery");
    }

    #[test]
    fn line_zero_falls_back_to_plain() {
        // A zero line number must not underflow; fall back to the plain form.
        let out = render(SRC, "app.lua", "runtime error: app.lua:0: mystery", false);
        assert_eq!(out, "lur: app.lua:0: mystery");
    }

    #[test]
    fn renders_syntax_error_snippet() {
        // Syntax errors carry no traceback; the "syntax error: " prefix is stripped.
        let out = render(
            SRC,
            "app.lua",
            "syntax error: app.lua:1: Expected identifier",
            false,
        );
        assert!(out.contains("error: Expected identifier"), "{out}");
        assert!(out.contains("--> app.lua:1"), "{out}");
        assert!(out.contains("1 | local x = nil"), "{out}");
    }

    #[test]
    fn caret_points_at_the_column_when_present() {
        // With a column, the position line shows it and the caret is indented to
        // it (col 7 -> 6 leading spaces before '^').
        let out = render(
            SRC,
            "app.lua",
            "syntax error: app.lua:2:7: bad token",
            false,
        );
        assert!(out.contains("--> app.lua:2:7"), "{out}");
        assert!(
            out.contains("      ^"),
            "caret indented to the column: {out}"
        );
    }

    #[test]
    fn color_from_env_truth_table() {
        // On a TTY with NO_COLOR unset → colorize.
        assert!(color_from_env(None, true));
        // NO_COLOR present and non-empty disables, even on a TTY.
        assert!(!color_from_env(Some(OsStr::new("1")), true));
        // An empty NO_COLOR does not disable (de-facto standard).
        assert!(color_from_env(Some(OsStr::new("")), true));
        // Non-TTY (pipe/redirect) never colorizes, regardless of NO_COLOR.
        assert!(!color_from_env(None, false));
        assert!(!color_from_env(Some(OsStr::new("1")), false));
    }

    #[test]
    fn colorizes_snippet_when_enabled() {
        let displayed = "syntax error: app.lua:2:7: bad token";
        let out = render(SRC, "app.lua", displayed, true);
        // Bold-red for the `error:` label and the caret, bold-blue for the gutter,
        // and a reset somewhere.
        assert!(
            out.contains("\x1b[1;31m"),
            "expected bold-red codes: {out:?}"
        );
        assert!(
            out.contains("\x1b[1;34m"),
            "expected bold-blue gutter: {out:?}"
        );
        assert!(out.contains("\x1b[0m"), "expected a reset: {out:?}");
        // The plain text content survives; the location is colored separately from
        // the `-->` arrow, so assert the bare location string.
        assert!(out.contains("error:"), "{out:?}");
        assert!(out.contains("app.lua:2:7"), "{out:?}");
        // Bold-red specifically wraps the caret.
        assert!(out.contains("\x1b[1;31m^\x1b[0m"), "caret colored: {out:?}");
    }

    #[test]
    fn no_escape_codes_when_disabled() {
        let displayed = "runtime error: app.lua:2: attempt to index nil with 'y'\n\
                         stack traceback:\n\tapp.lua:2: in main chunk\n\t[C]: in ?";
        let out = render(SRC, "app.lua", displayed, false);
        assert!(!out.contains('\x1b'), "no ANSI codes expected: {out:?}");
    }
}
