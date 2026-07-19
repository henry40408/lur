//! Human-readable, rustc-style rendering of script errors against their source.

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
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "{e}error:{r} {message}");
    let pos = match col {
        Some(c) => format!("{chunk_name}:{line}:{c}"),
        None => format!("{chunk_name}:{line}"),
    };
    let _ = writeln!(out, " {g}-->{r} {pos}");

    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    let _ = writeln!(out, "{g}{pad} |{r}");
    let _ = writeln!(out, "{g}{gutter} |{r} {src_line}");
    // Caret: under `col` when known, else under the first non-whitespace char.
    // The pad mirrors the source prefix character-for-character — tabs stay tabs
    // so the caret lands at the same terminal tab stop; everything else becomes a
    // space (char-counted, so multibyte prefixes don't shift it).
    let prefix_end = match col {
        Some(c) => src_line
            .char_indices()
            .nth(c.saturating_sub(1))
            .map_or(src_line.len(), |(i, _)| i),
        None => src_line.len() - src_line.trim_start().len(),
    };
    let caret_pad: String = src_line[..prefix_end]
        .chars()
        .map(|ch| if ch == '\t' { '\t' } else { ' ' })
        .collect();
    let _ = writeln!(out, "{g}{pad} |{r} {caret_pad}{e}^{r}");

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
    use super::render;

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
    fn caret_mirrors_tab_indentation() {
        // Luau emits no column, so the caret falls back to the first
        // non-whitespace char. The caret line must reproduce the source line's
        // leading tabs (not collapse them to single spaces), so `^` lands under
        // the statement at the terminal's tab stops instead of drifting left.
        let src = "local x = nil\n\t\tprint(x.y)\n";
        let displayed = "runtime error: tab.lua:2: attempt to index nil with 'y'";
        let out = render(src, "tab.lua", displayed, false);
        let caret_line = out.lines().last().expect("a caret line");
        assert!(
            caret_line.ends_with("\t\t^"),
            "caret pad should mirror the two leading tabs: {caret_line:?}"
        );
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
