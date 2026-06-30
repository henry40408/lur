# `lur docs` Rendering Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `lur docs` output scannable — glamour-aligned heading hierarchy, indentation, and syntax-highlighted Lua code blocks.

**Architecture:** Two halves of one responsibility in `src/docs.rs`: a hand-rolled `highlight_lua` tokenizer (keyword/string/comment/number → 16-color SGR), and an upgraded `render` that styles headings (H1 reverse bar; H2+ keep `#` prefixes + bold/indent), frames code blocks with `│ `, and calls `highlight_lua` on their bodies. Color is gated by the existing `bool` so plain mode keeps structure but drops escapes.

**Tech Stack:** Rust (edition 2024), `pulldown-cmark` (already a dep), no new dependencies.

## Global Constraints

- No new dependencies — `highlight_lua` is hand-rolled.
- 16-color ANSI SGR only; no truecolor/256-color.
- `highlight_lua(code, false)` MUST be byte-identical to `code`; the tokenizer MUST NOT panic on unterminated strings/comments.
- Plain mode (`color = false`) MUST contain no `\x1b`, but MUST keep the `## `/`### ` heading prefixes, the `│ ` code frame, and indentation.
- Run tests with `cargo nextest run` (NOT `cargo test`); always `cd /Users/henry/Develop/claude/lur` first.
- `cargo fmt --all` + `cargo clippy --all-targets -- -D warnings` clean before every commit. Stage files by name (never `git add -A`). Commits GPG-signed by default.
- Color palette (exact codes): keyword `\x1b[1;36m`, string `\x1b[32m`, comment `\x1b[90m`, number `\x1b[33m`, reset `\x1b[0m`; H1 bar `\x1b[1;7m`, H3 `\x1b[1;36m`, H2/H4–6 bold `\x1b[1m`, inline code `\x1b[36m`, dim frame `\x1b[2m`.
- The test harness `tests/guide.rs` is NOT touched — it scans raw Markdown.

---

### Task 1: Hand-rolled Lua syntax highlighter

**Files:**
- Modify: `src/docs.rs` (add `highlight_lua` + tests; place the fn above the existing `#[cfg(test)]` module)

**Interfaces:**
- Produces: `fn highlight_lua(code: &str, color: bool) -> String` (module-private; `render` in Task 2 calls it).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/docs.rs`:
```rust
    use super::highlight_lua;

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
        assert!(out.contains("\x1b[32m\"a--b\"\x1b[0m"), "string keeps --: {out:?}");
        assert!(!out.contains("\x1b[1;36mend\x1b[0m"), "no partial keyword: {out:?}");
        assert!(out.contains("endpoint"), "{out:?}");
    }

    #[test]
    fn highlight_lua_handles_unterminated_string() {
        // Must not panic; remainder is colored as the string.
        let out = highlight_lua("local s = \"oops", true);
        assert!(out.contains("\x1b[32m\"oops\x1b[0m"), "{out:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(highlight_lua)'`
Expected: FAIL to compile — `highlight_lua` is not defined.

- [ ] **Step 3: Implement `highlight_lua`**

Add to `src/docs.rs` (above the `#[cfg(test)]` module):
```rust
const KW_C: &str = "\x1b[1;36m"; // keyword (bold cyan)
const STR_C: &str = "\x1b[32m"; // string (green)
const COM_C: &str = "\x1b[90m"; // comment (bright black)
const NUM_C: &str = "\x1b[33m"; // number (yellow)

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function",
    "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then",
    "true", "until", "while",
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
                rest.find("]]").map(|p| p + 2).unwrap_or(rest.len())
            } else {
                rest.find('\n').unwrap_or(rest.len())
            };
            wrap(&mut out, COM_C, &rest[..end]);
            i += end;
            continue;
        }
        // Long-bracket string `[[ ... ]]`.
        if rest.starts_with("[[") {
            let end = rest.find("]]").map(|p| p + 2).unwrap_or(rest.len());
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
```
This reuses the existing `RESET` const. Keep `BOLD`/`CODE` consts only if Task 2 still needs them (Task 2 replaces their use).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run -E 'test(highlight_lua)'`
Expected: PASS — all four `highlight_lua` tests green, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/docs.rs
git commit -m "feat(docs): hand-rolled Lua syntax highlighter"
```

---

### Task 2: Glamour-aligned layout in `render`

Rework `render` to style headings by level, indent by section depth, frame code
blocks with `│ ` and highlight their bodies, and color inline code distinctly.

**Files:**
- Modify: `src/docs.rs` (`render` body + the two existing `render` tests; constants)

**Interfaces:**
- Consumes: `highlight_lua` (Task 1).
- Produces: unchanged signature `pub fn render(markdown: &str, color: bool) -> String`.

- [ ] **Step 1: Update the existing render tests to the new output shape**

Replace the two existing tests (`color_off_strips_markup_and_has_no_escapes`,
`color_on_emits_escape_codes`) with:
```rust
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
        assert!(out.contains("\x1b[1;36mlocal\x1b[0m"), "code highlighted: {out:?}");
        // The code frame bar is present.
        assert!(out.contains('\u{2502}'), "code frame: {out:?}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo nextest run -E 'test(color_off_keeps_structure) + test(color_on_styles)'`
Expected: FAIL — current `render` emits neither `\x1b[1;7m`, the `## ` prefixes, nor the `│ ` frame.

- [ ] **Step 3: Rewrite `render`**

Replace the whole `render` function (and adjust the top-of-file consts) in
`src/docs.rs` with:
```rust
const BOLD: &str = "\x1b[1m";
const H1: &str = "\x1b[1;7m"; // reverse video bar
const H3C: &str = "\x1b[1;36m"; // bold cyan (capability headings)
const INLINE: &str = "\x1b[36m"; // inline code (cyan)
const DIM: &str = "\x1b[2m"; // code frame bar
const RESET: &str = "\x1b[0m";

/// Render `markdown` to terminal text, glamour-aligned. When `color` is false,
/// styling strings are empty but the structure (heading `#` prefixes, `│ ` code
/// frame, indentation) is kept, so plain mode is clean, readable text with no
/// escape codes.
pub fn render(markdown: &str, color: bool) -> String {
    let st = |s: &str| if color { s } else { "" };

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
            Event::End(TagEnd::Paragraph) => out.push('\n'),
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
            Event::End(TagEnd::Item) => out.push('\n'),
            Event::Start(Tag::BlockQuote(_)) => {
                out.push_str(&" ".repeat(margin));
                out.push_str(st(DIM));
                out.push('\u{2502}');
                out.push_str(st(RESET));
                out.push(' ');
            }
            Event::End(TagEnd::BlockQuote(_)) => out.push('\n'),
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
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("\n---\n"),
            _ => {}
        }
    }

    format!("{}\n", out.trim())
}
```
Notes for the implementer:
- The `CODE` const is gone; if clippy flags an unused const, remove it.
- pulldown-cmark 0.13 yields `Tag::Heading { level, .. }` where `level` is a
  `HeadingLevel` enum that casts to `usize` via `level as usize` (`H1` → 1 …).
  If the cast does not compile, match the level explicitly (`HeadingLevel::H1 => 1`, …).
- The code-block branch splits the fenced text into lines so each gets its own
  `│ ` frame; an empty trailing segment (from a final `\n`) produces a bare
  frame line — acceptable, matching glamour's framed blocks.

- [ ] **Step 4: Run to verify the render tests pass**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run -E 'test(docs)'`
Expected: PASS — both render tests and all four highlighter tests green.

- [ ] **Step 5: Run the full suite + eyeball**

Run: `cargo nextest run`
Expected: PASS — 192/192 (the guide harness is unaffected).
Run: `cargo run -q -- docs | head -30`
Expected (piped → no color): H1 line, `## ` / `### ` prefixed headings indented by level, code lines framed with `│ `, no escape codes.

- [ ] **Step 6: Commit**

```bash
git add src/docs.rs
git commit -m "feat(docs): glamour-aligned heading hierarchy and framed code blocks"
```

---

## Self-Review

**Spec coverage:**
- Component 1 (layout: H1 bar, H2+ `#` prefixes, indent, `│ ` frame, inline code, block quote) → Task 2. ✓
- Component 2 (`highlight_lua`: keyword/string/comment/number, word-boundary keywords, `--`-in-string safety, no-panic, color-off round-trip) → Task 1. ✓
- 16-color palette exact codes → Global Constraints + both tasks. ✓
- Plain mode keeps structure, drops escapes → Task 2 test `color_off_keeps_structure_without_escapes`. ✓
- Harness untouched → not modified; full-suite run in Task 2 Step 5 confirms 192/192. ✓

**Placeholder scan:** No TBD/TODO; every code step shows full code. The `HeadingLevel as usize` cast carries an explicit fallback. The unused-`CODE`-const note is a concrete clippy instruction, not a placeholder.

**Type consistency:** `highlight_lua(&str, bool) -> String` defined in Task 1, called in Task 2 Step 3 with `(body, color)`. `render(&str, bool) -> String` signature unchanged. Consts (`RESET`, `BOLD`, `H1`, `H3C`, `INLINE`, `DIM`) declared once in Task 2's const block; `KW_C`/`STR_C`/`COM_C`/`NUM_C` in Task 1. `RESET` is shared — Task 1 uses the existing `RESET`; Task 2 keeps it in the const block. No duplicate-const conflict (both reference the same `const RESET`).
