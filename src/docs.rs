//! Render the embedded `GUIDE.md` to ANSI-styled text for `lur docs`. A
//! hand-rolled sink over `pulldown-cmark` — color is gated so plain mode is
//! clean text with the Markdown markup stripped, never half-rendered source.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

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

/// Highlight a Lua snippet with 16-color SGR. Returns `code` unchanged when
/// `color` is false. Never panics; unterminated strings/comments consume to the
/// end of input as that token. A pragmatic tokenizer — not a full Lua lexer —
/// but it must not mis-scan: comments/strings are recognized before operators,
/// and keywords match only on word boundaries.
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
        // Line comment (and long comment `--[[ ... ]]`).
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
        // Long-bracket string `[[ ... ]]`.
        if rest.starts_with("[[") {
            let end = rest.find("]]").map_or(rest.len(), |p| p + 2);
            wrap(&mut out, STR_C, &rest[..end]);
            i += end;
            continue;
        }
        // Quoted string (single or double), honoring `\` escapes.
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
        // Number (decimal/float/hex).
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
        // Identifier / keyword (word boundary).
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
        // Anything else: emit one char verbatim.
        let ch = rest.chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Render `markdown` to terminal text, glamour-aligned. When `color` is false,
/// styling strings are empty but the structure (heading `#` prefixes, `│ ` code
/// frame, indentation) is kept, so plain mode is clean, readable text with no
/// escape codes.
pub fn render(markdown: &str, color: bool) -> String {
    let st = |s: &'static str| -> &'static str { if color { s } else { "" } };

    let mut out = String::new();
    let mut list_depth: usize = 0;
    let mut margin: usize = 0; // left indent of the current section's body
    let mut in_code = false;
    let mut heading_level: usize = 0; // 0 = not in a heading

    for ev in Parser::new(markdown) {
        match ev {
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
            Event::Start(Tag::Paragraph) => out.push_str(&" ".repeat(margin)),
            Event::Start(Tag::Strong | Tag::Emphasis) => out.push_str(st(BOLD)),
            Event::End(TagEnd::Strong | TagEnd::Emphasis) => out.push_str(st(RESET)),
            Event::Start(Tag::List(_)) => list_depth += 1,
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
                    // Frame each code line with `│ ` at the section margin, and
                    // syntax-highlight the line content.
                    for line in text.split_inclusive('\n') {
                        let nl = line.ends_with('\n');
                        let body = line.strip_suffix('\n').unwrap_or(line);
                        out.push('\n');
                        out.push_str(&" ".repeat(margin));
                        out.push_str(st(DIM));
                        out.push('\u{2502}');
                        out.push_str(st(RESET));
                        out.push(' ');
                        out.push_str(&highlight_lua(body, color));
                        let _ = nl;
                    }
                } else {
                    out.push_str(&text);
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Item | TagEnd::BlockQuote(_))
            | Event::SoftBreak
            | Event::HardBreak => out.push('\n'),
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
        // Heading prefixes kept for H2+; H1 has no '#'.
        assert!(out.contains("## Section"), "h2 prefix: {out:?}");
        assert!(out.contains("### lur.json"), "h3 prefix: {out:?}");
        // Code frame present; code text verbatim.
        assert!(out.contains("\u{2502} local x = 1"), "code frame: {out:?}");
        // Inline code and heading text survive as plain text.
        assert!(out.contains("Guide") && out.contains("lur.json"), "{out:?}");
    }

    #[test]
    fn color_on_styles_headings_and_highlights_code() {
        let out = render(MD, true);
        assert!(out.contains("\x1b[1;7m"), "h1 reverse bar: {out:?}");
        assert!(out.contains("\x1b[1;36m"), "h3 bold cyan: {out:?}");
        // The code block body is highlighted (the `local` keyword is colored).
        assert!(
            out.contains("\x1b[1;36mlocal\x1b[0m"),
            "code highlighted: {out:?}"
        );
        // The code frame bar is present.
        assert!(out.contains('\u{2502}'), "code frame: {out:?}");
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
