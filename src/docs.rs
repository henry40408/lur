//! Render the embedded `GUIDE.md` to ANSI-styled text for `lur docs`. A
//! hand-rolled sink over `pulldown-cmark` — color is gated so plain mode is
//! clean text with the Markdown markup stripped, never half-rendered source.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

const BOLD: &str = "\x1b[1m";
const CODE: &str = "\x1b[1;34m";
const RESET: &str = "\x1b[0m";

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
}
