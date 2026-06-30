//! Render the embedded `GUIDE.md` to ANSI-styled text for `lur docs`. A
//! hand-rolled sink over `pulldown-cmark` — color is gated so plain mode is
//! clean text with the Markdown markup stripped, never half-rendered source.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

const BOLD: &str = "\x1b[1m";
const CODE: &str = "\x1b[1;34m";
const RESET: &str = "\x1b[0m";

#[allow(dead_code)]
const KW_C: &str = "\x1b[1;36m"; // keyword (bold cyan)
#[allow(dead_code)]
const STR_C: &str = "\x1b[32m"; // string (green)
#[allow(dead_code)]
const COM_C: &str = "\x1b[90m"; // comment (bright black)
#[allow(dead_code)]
const NUM_C: &str = "\x1b[33m"; // number (yellow)

#[allow(dead_code)]
const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Highlight a Lua snippet with 16-color SGR. Returns `code` unchanged when
/// `color` is false. Never panics; unterminated strings/comments consume to the
/// end of input as that token. A pragmatic tokenizer — not a full Lua lexer —
/// but it must not mis-scan: comments/strings are recognized before operators,
/// and keywords match only on word boundaries.
#[allow(dead_code)]
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

/// Render `markdown` to terminal text. When `color` is false, every style is an
/// empty string, so the output is the document text (markup stripped) with no
/// escape codes.
pub fn render(markdown: &str, color: bool) -> String {
    let (bold, code, reset) = if color {
        (BOLD, CODE, RESET)
    } else {
        ("", "", "")
    };

    let mut out = String::new();
    let mut list_depth: usize = 0;
    let mut in_code_block = false;

    for ev in Parser::new(markdown) {
        match ev {
            Event::Start(Tag::Heading { .. }) => {
                out.push('\n');
                out.push_str(bold);
            }
            Event::End(TagEnd::Heading(_)) => {
                out.push_str(reset);
                out.push('\n');
            }
            Event::End(TagEnd::Paragraph | TagEnd::Item | TagEnd::BlockQuote(_))
            | Event::SoftBreak
            | Event::HardBreak => out.push('\n'),
            Event::Start(Tag::Strong | Tag::Emphasis) => out.push_str(bold),
            Event::End(TagEnd::Strong | TagEnd::Emphasis) => out.push_str(reset),
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                if list_depth == 0 {
                    out.push('\n');
                }
            }
            Event::Start(Tag::Item) => {
                out.push_str(&"  ".repeat(list_depth.saturating_sub(1)));
                out.push_str("- ");
            }
            Event::Start(Tag::BlockQuote(_)) => out.push_str("\u{2502} "),
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
                out.push('\n');
                out.push_str(code);
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                out.push_str(reset);
                out.push('\n');
            }
            Event::Code(text) => {
                out.push_str(code);
                out.push_str(&text);
                out.push_str(reset);
            }
            Event::Text(text) => {
                if in_code_block {
                    // Indent each code line by two spaces; blank lines are emitted as-is.
                    for line in text.split_inclusive('\n') {
                        if !line.trim().is_empty() {
                            out.push_str("  ");
                        }
                        out.push_str(line);
                    }
                } else {
                    out.push_str(&text);
                }
            }
            Event::Rule => out.push_str("\n---\n"),
            _ => {}
        }
    }

    // Collapse the leading newline and trailing whitespace.
    format!("{}\n", out.trim())
}

#[cfg(test)]
mod tests {
    use super::highlight_lua;
    use super::render;

    const MD: &str = "# Title\n\nSome **bold** and `code`.\n\n```lua\nlocal x = 1\n```\n";

    #[test]
    fn color_off_strips_markup_and_has_no_escapes() {
        let out = render(MD, false);
        assert!(!out.contains('\x1b'), "no ANSI codes: {out:?}");
        // Markdown markup characters are stripped by the parser.
        assert!(!out.contains("**"), "no bold markers: {out:?}");
        assert!(!out.contains('`'), "no code ticks: {out:?}");
        assert!(!out.contains('#'), "no heading markers: {out:?}");
        // The text content survives.
        assert!(out.contains("Title"), "{out:?}");
        assert!(out.contains("bold"), "{out:?}");
        assert!(out.contains("local x = 1"), "{out:?}");
    }

    #[test]
    fn color_on_emits_escape_codes() {
        let out = render(MD, true);
        assert!(out.contains('\x1b'), "expected ANSI codes: {out:?}");
        assert!(out.contains("Title"), "{out:?}");
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
