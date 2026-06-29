//! Human-readable, rustc-style rendering of script errors against their source.

/// Render `displayed` (an mlua error's `Display`) against `source`, rustc-style.
/// `chunk_name` is the bare path used as the chunk name (no `@`). Falls back to
/// `lur: <message>` when no in-range location can be parsed. Never panics.
pub fn render(source: &str, chunk_name: &str, displayed: &str) -> String {
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
        return format!("lur: {message}");
    };

    let mut out = String::new();
    out.push_str(&format!("error: {message}\n"));
    let pos = match col {
        Some(c) => format!("{chunk_name}:{line}:{c}"),
        None => format!("{chunk_name}:{line}"),
    };
    out.push_str(&format!(" --> {pos}\n"));

    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    out.push_str(&format!("{pad} |\n"));
    out.push_str(&format!("{gutter} | {src_line}\n"));
    // Caret: under `col` when known, else under the first non-whitespace char.
    let caret_col = col.unwrap_or_else(|| src_line.len() - src_line.trim_start().len() + 1);
    let caret_pad = " ".repeat(caret_col.saturating_sub(1));
    out.push_str(&format!("{pad} | {caret_pad}^\n"));

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
        let out = render(SRC, "app.lua", displayed);
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
        let out = render(SRC, "app.lua", displayed);
        assert_eq!(out, "lur: something with no location");
    }

    #[test]
    fn out_of_range_line_falls_back_to_plain() {
        let displayed = "runtime error: app.lua:99: mystery";
        let out = render(SRC, "app.lua", displayed);
        assert_eq!(out, "lur: mystery");
    }

    #[test]
    fn line_zero_falls_back_to_plain() {
        // A zero line number must not underflow; fall back to the plain form.
        let out = render(SRC, "app.lua", "runtime error: app.lua:0: mystery");
        assert_eq!(out, "lur: app.lua:0: mystery");
    }

    #[test]
    fn renders_syntax_error_snippet() {
        // Syntax errors carry no traceback; the "syntax error: " prefix is stripped.
        let out = render(
            SRC,
            "app.lua",
            "syntax error: app.lua:1: Expected identifier",
        );
        assert!(out.contains("error: Expected identifier"), "{out}");
        assert!(out.contains("--> app.lua:1"), "{out}");
        assert!(out.contains("1 | local x = nil"), "{out}");
    }

    #[test]
    fn caret_points_at_the_column_when_present() {
        // With a column, the position line shows it and the caret is indented to
        // it (col 7 -> 6 leading spaces before '^').
        let out = render(SRC, "app.lua", "syntax error: app.lua:2:7: bad token");
        assert!(out.contains("--> app.lua:2:7"), "{out}");
        assert!(
            out.contains("      ^"),
            "caret indented to the column: {out}"
        );
    }
}
