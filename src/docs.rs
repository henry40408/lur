//! Render the embedded `GUIDE.md` for `lur docs`: a hand-rolled
//! `pulldown-cmark` sink whose plain mode strips all markup and escapes.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

const BOLD: &str = "\x1b[1m";
const H1: &str = "\x1b[1;7m"; // reverse video bar
const H3C: &str = "\x1b[1;36m"; // bold cyan (capability headings)
const INLINE: &str = "\x1b[36m"; // inline code (cyan)
const DIM: &str = "\x1b[2m"; // code frame bar
const RESET: &str = "\x1b[0m";

const KW_C: &str = "\x1b[1;36m"; // keyword (bold cyan)
const STR_C: &str = "\x1b[32m"; // string (green)
const COM_C: &str = "\x1b[90m"; // comment (bright black)
const NUM_C: &str = "\x1b[33m"; // number (yellow)

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// A pragmatic Lua highlighter, not a full lexer: comments/strings are scanned
/// before operators, keywords match on word boundaries, and an unterminated
/// string/comment runs to the end.
fn highlight_lua(code: &str, color: bool) -> String {
    if !color {
        return code.to_string();
    }
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len() + 32);
    let mut i = 0;
    let wrap = |out: &mut String, c: &str, s: &str| {
        out.push_str(c);
        out.push_str(s);
        out.push_str(RESET);
    };
    while i < bytes.len() {
        let rest = &code[i..];
        if rest.starts_with("--") {
            let end = if rest.starts_with("--[[") {
                rest.find("]]").map_or(rest.len(), |p| p + 2)
            } else {
                rest.find('\n').unwrap_or(rest.len())
            };
            wrap(&mut out, COM_C, &rest[..end]);
            i += end;
            continue;
        }
        if rest.starts_with("[[") {
            let end = rest.find("]]").map_or(rest.len(), |p| p + 2);
            wrap(&mut out, STR_C, &rest[..end]);
            i += end;
            continue;
        }
        let c0 = bytes[i];
        if c0 == b'"' || c0 == b'\'' {
            let quote = c0;
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == quote {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(bytes.len());
            wrap(&mut out, STR_C, &code[i..end]);
            i = end;
            continue;
        }
        if c0.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'x')
            {
                j += 1;
            }
            wrap(&mut out, NUM_C, &code[i..j]);
            i = j;
            continue;
        }
        if c0.is_ascii_alphabetic() || c0 == b'_' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let word = &code[i..j];
            if LUA_KEYWORDS.contains(&word) {
                wrap(&mut out, KW_C, word);
            } else {
                out.push_str(word);
            }
            i = j;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Printed width of `s`, skipping `ESC [ … m` colour sequences. Text between
/// escapes is measured with `unicode-width`, so emoji and ZWJ sequences count
/// as the terminal draws them.
fn visible_len(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    let mut n = 0;
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        n += UnicodeWidthStr::width(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find('m').map_or(tail.len(), |p| p + 1);
        rest = &tail[end..];
    }
    n + UnicodeWidthStr::width(rest)
}

/// Track the SGR state across `s`: a reset clears it, any other escape appends.
fn track_style(s: &str, active: &mut String) {
    let mut rest = s;
    while let Some(i) = rest.find('\x1b') {
        let tail = &rest[i..];
        let end = tail.find('m').map_or(tail.len(), |p| p + 1);
        if &tail[..end] == RESET {
            active.clear();
        } else {
            active.push_str(&tail[..end]);
        }
        rest = &tail[end..];
    }
}

/// Greedy word wrap of one cell to `width` columns. Colour state is closed at
/// each line end and reopened on the next, so styled spans survive a wrap.
fn wrap_cell(cell: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut curw = 0;
    let mut active = String::new();
    let flush = |lines: &mut Vec<String>, cur: &mut String, curw: &mut usize, active: &str| {
        let mut line = std::mem::replace(cur, active.to_string());
        if !active.is_empty() {
            line.push_str(RESET);
        }
        lines.push(line);
        *curw = 0;
    };
    for word in cell.split_whitespace() {
        let w = visible_len(word);
        if curw > 0 && curw + 1 + w > width {
            flush(&mut lines, &mut cur, &mut curw, &active);
        }
        if w <= width {
            if curw > 0 {
                cur.push(' ');
                curw += 1;
            }
            cur.push_str(word);
            curw += w;
            track_style(word, &mut active);
            continue;
        }
        // Longer than a whole line: break by character.
        if curw > 0 {
            flush(&mut lines, &mut cur, &mut curw, &active);
        }
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                let mut seq = String::from(c);
                for c in chars.by_ref() {
                    seq.push(c);
                    if c == 'm' {
                        break;
                    }
                }
                track_style(&seq, &mut active);
                cur.push_str(&seq);
                continue;
            }
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if curw > 0 && curw + cw > width {
                flush(&mut lines, &mut cur, &mut curw, &active);
            }
            cur.push(c);
            curw += cw;
        }
    }
    if curw > 0 || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Smallest width a column is shrunk to when fitting a table to the terminal.
const MIN_COL: usize = 6;

/// Column-aligned table: bold header, dim rule, two-space gutters, last column
/// unpadded. Every line starts with `prefix` (indent and/or blockquote bar,
/// `prefix_width` columns wide). With `max_width`, cells wrap so the table fits.
fn render_table(
    rows: &[Vec<String>],
    prefix: &str,
    prefix_width: usize,
    max_width: Option<usize>,
    color: bool,
) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(visible_len(cell));
        }
    }
    let gutters = 2 * cols.saturating_sub(1);
    if let Some(max) = max_width {
        let avail = max.saturating_sub(prefix_width);
        while widths.iter().sum::<usize>() + gutters > avail {
            let (i, &w) = widths
                .iter()
                .enumerate()
                .max_by_key(|&(i, &w)| (w, std::cmp::Reverse(i)))
                .unwrap();
            if w <= MIN_COL {
                break;
            }
            widths[i] -= 1;
        }
    }
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        let wrapped: Vec<Vec<String>> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                if max_width.is_some() && visible_len(cell) > widths[i] {
                    wrap_cell(cell, widths[i])
                } else {
                    vec![cell.clone()]
                }
            })
            .collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for l in 0..height {
            let start = out.len();
            out.push_str(prefix);
            let prefix_end = out.len();
            for (i, lines) in wrapped.iter().enumerate() {
                let cell = lines.get(l).map_or("", String::as_str);
                if r == 0 && color && !cell.is_empty() {
                    out.push_str(BOLD);
                    out.push_str(cell);
                    out.push_str(RESET);
                } else {
                    out.push_str(cell);
                }
                if i + 1 < wrapped.len() {
                    let fill = (widths[i] + 2).saturating_sub(visible_len(cell));
                    out.push_str(&" ".repeat(fill));
                }
            }
            let trimmed = out.trim_end_matches(' ').len().max(prefix_end);
            out.truncate(trimmed);
            if out.len() == prefix_end {
                out.truncate(start + prefix.trim_end_matches(' ').len());
            }
            out.push('\n');
        }
        if r == 0 {
            let total = widths.iter().sum::<usize>() + gutters;
            out.push_str(prefix);
            if color {
                out.push_str(DIM);
            }
            out.push_str(&"\u{2500}".repeat(total));
            if color {
                out.push_str(RESET);
            }
            out.push('\n');
        }
    }
    out
}

/// Terminal width for `lur docs`: `None` (no wrapping) unless stdout is a TTY,
/// then `COLUMNS` if set to a positive number, else the `TIOCGWINSZ` width
/// (unix), else 80.
pub fn terminal_width() -> Option<usize> {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return None;
    }
    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0);
    #[cfg(unix)]
    let cols = cols.or_else(|| {
        rustix::termios::tcgetwinsize(std::io::stdout())
            .ok()
            .map(|ws| usize::from(ws.ws_col))
            .filter(|&n| n > 0)
    });
    Some(cols.unwrap_or(80))
}

/// Glamour-style terminal rendering. Without `color`, the structure (heading
/// `#` prefixes, `│ ` code frame, indentation) stays but no escapes are emitted.
pub fn render(markdown: &str, color: bool) -> String {
    render_width(markdown, color, None)
}

/// Like [`render`], wrapping table cells so tables fit `width` columns.
pub fn render_width(markdown: &str, color: bool, width: Option<usize>) -> String {
    let st = |s: &'static str| -> &'static str { if color { s } else { "" } };

    let mut out = String::new();
    let mut list_depth: usize = 0;
    let mut margin: usize = 0; // left indent of the current section's body
    let mut in_code = false;
    let mut in_blockquote = false;
    let mut heading_level: usize = 0; // 0 = not in a heading

    // Table state: finished rows, the row being built, and the output buffer
    // parked while a cell collects its text.
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut parked = String::new();

    for ev in Parser::new_ext(markdown, Options::ENABLE_TABLES) {
        match ev {
            Event::Start(Tag::Table(_)) => {
                rows.clear();
                if in_blockquote {
                    // The bar was pushed at the quote start; the table repeats
                    // it on every line, so drop the lone one here.
                    let bar = format!("{}{}\u{2502}{} ", " ".repeat(margin), st(DIM), st(RESET));
                    if let Some(stripped) = out.strip_suffix(&bar) {
                        out.truncate(stripped.len());
                    }
                }
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if !in_blockquote && list_depth == 0 && !out.ends_with("\n\n") {
                    out.push('\n');
                }
            }
            Event::Start(Tag::TableCell) => parked = std::mem::take(&mut out),
            Event::End(TagEnd::TableCell) => {
                let cell = std::mem::replace(&mut out, std::mem::take(&mut parked));
                row.push(cell);
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
            Event::End(TagEnd::Table) => {
                let (prefix, prefix_width) = if in_blockquote {
                    let bar = format!("{}{}\u{2502}{} ", " ".repeat(margin), st(DIM), st(RESET));
                    (bar, margin + 2)
                } else {
                    let indent = margin + list_depth * 2;
                    (" ".repeat(indent), indent)
                };
                out.push_str(&render_table(&rows, &prefix, prefix_width, width, color));
                rows.clear();
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = level as usize;
                heading_level = lvl;
                margin = (lvl.saturating_sub(1)) * 2;
                out.push('\n');
                out.push_str(&" ".repeat(margin));
                if lvl == 1 {
                    out.push_str(st(H1));
                    out.push(' '); // bar padding
                } else {
                    out.push_str(st(if lvl == 3 { H3C } else { BOLD }));
                    out.push_str(&"#".repeat(lvl));
                    out.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if heading_level == 1 {
                    out.push(' '); // close the bar
                }
                out.push_str(st(RESET));
                out.push('\n');
                heading_level = 0;
            }
            Event::Start(Tag::Paragraph) => {
                if !in_blockquote && !out.ends_with("- ") {
                    out.push_str(&" ".repeat(margin));
                }
            }
            Event::Start(Tag::Strong | Tag::Emphasis) => out.push_str(st(BOLD)),
            Event::End(TagEnd::Strong | TagEnd::Emphasis) => out.push_str(st(RESET)),
            Event::Start(Tag::List(_)) => {
                if list_depth == 0 && !out.is_empty() && !out.ends_with("\n\n") {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                }
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                if list_depth == 0 {
                    out.push('\n');
                }
            }
            Event::Start(Tag::Item) => {
                out.push_str(&" ".repeat(margin + (list_depth.saturating_sub(1)) * 2));
                out.push_str("- ");
            }
            Event::Start(Tag::BlockQuote(_)) => {
                in_blockquote = true;
                out.push_str(&" ".repeat(margin));
                out.push_str(st(DIM));
                out.push('\u{2502}');
                out.push_str(st(RESET));
                out.push(' ');
            }
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                out.push('\n');
            }
            Event::Code(text) => {
                out.push_str(st(INLINE));
                out.push_str(&text);
                out.push_str(st(RESET));
            }
            Event::Text(text) => {
                if in_code {
                    for line in text.split_inclusive('\n') {
                        let body = line.strip_suffix('\n').unwrap_or(line);
                        out.push('\n');
                        out.push_str(&" ".repeat(margin));
                        out.push_str(st(DIM));
                        out.push('\u{2502}');
                        out.push_str(st(RESET));
                        out.push(' ');
                        out.push_str(&highlight_lua(body, color));
                    }
                } else {
                    out.push_str(&text);
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Item) => out.push('\n'),
            Event::End(TagEnd::BlockQuote(_)) => {
                in_blockquote = false;
                out.push('\n');
            }
            Event::SoftBreak | Event::HardBreak => {
                out.push('\n');
                out.push_str(&" ".repeat(margin + list_depth * 2));
            }
            Event::Rule => out.push_str("\n---\n"),
            _ => {}
        }
    }

    format!("{}\n", out.trim())
}

#[cfg(test)]
mod tests {
    use super::highlight_lua;
    use super::render;

    const MD: &str = "# Guide\n\n## Section\n\n### lur.json\n\nUse `lur.json` here.\n\n```lua\nlocal x = 1\n```\n";

    #[test]
    fn color_off_keeps_structure_without_escapes() {
        let out = render(MD, false);
        assert!(!out.contains('\x1b'), "no ANSI codes: {out:?}");
        assert!(out.contains("## Section"), "h2 prefix: {out:?}");
        assert!(out.contains("### lur.json"), "h3 prefix: {out:?}");
        assert!(out.contains("\u{2502} local x = 1"), "code frame: {out:?}");
        // Inline code and heading text survive as plain text.
        assert!(out.contains("Guide") && out.contains("lur.json"), "{out:?}");
    }

    #[test]
    fn tables_render_aligned_without_pipes_or_separator() {
        let md = "| Call | Returns |\n| --- | --- |\n| `a` | one |\n| `longer` | two |\n\nafter\n";
        let out = render(md, false);
        assert!(!out.contains("| ---") && !out.contains('|'), "{out:?}");
        assert!(!out.contains('\x1b'), "{out:?}");
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| l.contains("one") || l.contains("two"))
            .collect();
        assert_eq!(rows.len(), 2, "{out:?}");
        // Second column starts at the same offset on both rows.
        assert_eq!(rows[0].find("one"), rows[1].find("two"), "{out:?}");
        assert!(out.contains("Call") && out.contains("after"), "{out:?}");
    }

    #[test]
    fn table_inside_list_item_keeps_items_clean() {
        let md =
            "- first\n\n  | k | v |\n  | --- | --- |\n  | a | b |\n\n- second\n\n  more text\n";
        let out = render(md, false);
        assert!(!out.contains("| ---"), "{out:?}");
        assert!(
            !out.contains("-       "),
            "no margin glued after the bullet: {out:?}"
        );
        assert!(
            out.contains("- first") && out.contains("- second"),
            "{out:?}"
        );
    }

    #[test]
    fn color_on_styles_headings_and_highlights_code() {
        let out = render(MD, true);
        assert!(out.contains("\x1b[1;7m"), "h1 reverse bar: {out:?}");
        assert!(out.contains("\x1b[1;36m"), "h3 bold cyan: {out:?}");
        assert!(
            out.contains("\x1b[1;36mlocal\x1b[0m"),
            "code highlighted: {out:?}"
        );
        assert!(out.contains('\u{2502}'), "code frame: {out:?}");
    }

    #[test]
    fn wrapped_paragraph_lines_keep_the_margin() {
        let md = "### lur.json\n\nfirst line here\nsecond line here\n";
        let out = render(md, false);
        // Under H3 the body margin is 4; the continuation line must be indented too.
        assert!(out.contains("    first line here"), "{out:?}");
        assert!(out.contains("    second line here"), "{out:?}");
    }

    #[test]
    fn highlight_lua_roundtrips_without_color() {
        let code = "local x = 1 -- a comment\nlocal s = \"hi\"\n";
        assert_eq!(highlight_lua(code, false), code);
    }

    #[test]
    fn highlight_lua_colors_each_token_class() {
        let out = highlight_lua("local n = 42 -- c\nlocal s = \"hi\"\n", true);
        assert!(out.contains("\x1b[1;36mlocal\x1b[0m"), "keyword: {out:?}");
        assert!(out.contains("\x1b[33m42\x1b[0m"), "number: {out:?}");
        assert!(out.contains("\x1b[32m\"hi\"\x1b[0m"), "string: {out:?}");
        assert!(out.contains("\x1b[90m-- c\x1b[0m"), "comment: {out:?}");
    }

    #[test]
    fn highlight_lua_does_not_misread_tokens() {
        // `--` inside a string is not a comment; `endpoint` is not the `end` keyword.
        let out = highlight_lua("local u = \"a--b\"\nlocal endpoint = 1\n", true);
        assert!(
            out.contains("\x1b[32m\"a--b\"\x1b[0m"),
            "string keeps --: {out:?}"
        );
        assert!(
            !out.contains("\x1b[1;36mend\x1b[0m"),
            "no partial keyword: {out:?}"
        );
        assert!(out.contains("endpoint"), "{out:?}");
    }

    #[test]
    fn highlight_lua_handles_unterminated_string() {
        // Must not panic; remainder is colored as the string.
        let out = highlight_lua("local s = \"oops", true);
        assert!(out.contains("\x1b[32m\"oops\x1b[0m"), "{out:?}");
    }
}

#[cfg(test)]
mod table_tests {
    use super::{render, render_width, visible_len};

    const NARROW: &str = "| Call | Returns |\n| --- | --- |\n| `a` | one |\n| `longer` | two |\n";

    #[test]
    fn narrow_ascii_table_is_unchanged_by_width() {
        let plain = render(NARROW, false);
        assert_eq!(render_width(NARROW, false, Some(80)), plain);
        assert_eq!(
            plain,
            "Call    Returns\n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\na       one\nlonger  two\n"
        );
    }

    #[test]
    fn wide_table_wraps_to_width_and_keeps_columns_aligned() {
        let md = "| Name | Description |\n| --- | --- |\n| `fetch` | Fetches a remote resource over HTTP and returns the body as a string value |\n| `x` | short |\n";
        let out = render_width(md, false, Some(40));
        for l in out.lines() {
            assert!(visible_len(l) <= 40, "line too wide: {l:?}");
        }
        let lines: Vec<&str> = out.lines().collect();
        let col = lines[0].find("Description").unwrap();
        assert!(lines.len() > 5, "wrapped into several lines: {out:?}");
        // Continuation lines start in the second column, first column blank.
        assert!(lines[2].starts_with("fetch"), "{out:?}");
        assert!(
            lines[3].starts_with(&" ".repeat(col)) && !lines[3][col..].is_empty(),
            "{out:?}"
        );
        assert_eq!(lines.last().unwrap().find("short"), Some(col), "{out:?}");
        // No wrapping without a width.
        assert!(render(md, false).lines().any(|l| l.len() > 60));
    }

    #[test]
    fn cjk_cells_use_display_width() {
        let md = "| 名稱 | 說明 |\n| --- | --- |\n| `ab` | one |\n| 中文字 | two |\n| ✅ | yes |\n| 👨‍👩‍👧 | fam |\n";
        let out = render(md, false);
        assert_eq!(visible_len("中文字"), 6);
        assert_eq!(visible_len("✅"), 2);
        assert_eq!(visible_len("❌"), 2);
        assert_eq!(visible_len("👨‍👩‍👧"), 2);
        assert_eq!(visible_len("👍🏽"), 2);
        assert_eq!(visible_len("\x1b[1m中\x1b[0m✅"), 4);
        // Second column starts at the same display column on every row.
        let cols: Vec<usize> = out
            .lines()
            .filter(|l| {
                l.ends_with("one")
                    || l.ends_with("two")
                    || l.ends_with("yes")
                    || l.ends_with("fam")
                    || l.ends_with("說明")
            })
            .map(|l| {
                let (first, _) = l.split_once("  ").unwrap();
                let gap = l[first.len()..].len() - l[first.len()..].trim_start().len();
                visible_len(first) + gap
            })
            .collect();
        assert_eq!(cols.len(), 5, "{out:?}");
        assert!(cols.iter().all(|&c| c == cols[0]), "{cols:?} {out:?}");
    }

    #[test]
    fn table_in_blockquote_keeps_bar_on_every_line() {
        let md = "> note\n>\n> | k | v |\n> | --- | --- |\n> | a | bbb |\n> | cc | d |\n";
        let out = render(md, false);
        let table: Vec<&str> = out
            .lines()
            .filter(|l| !l.contains("note"))
            .filter(|l| !l.trim().is_empty())
            .collect();
        assert_eq!(table.len(), 4, "{out:?}");
        for l in &table {
            assert!(l.starts_with("\u{2502} "), "bar missing: {l:?} in {out:?}");
        }
        assert!(!out.contains('|'), "{out:?}");
        assert_eq!(table[2].find("bbb"), table[3].find('d'), "{out:?}");
        // Wrapping accounts for the bar.
        let wide = "> | k | v |\n> | --- | --- |\n> | a | one two three four five six seven eight nine ten |\n";
        for l in render_width(wide, false, Some(30)).lines() {
            assert!(visible_len(l) <= 30, "{l:?}");
        }
    }
}
