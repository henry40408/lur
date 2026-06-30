# `lur docs` rendering overhaul: glamour-aligned layout + Lua syntax highlight

**Status:** approved design, ready for implementation plan
**Date:** 2026-06-30
**Builds on:** [2026-06-30-guided-tour-docs-design.md](2026-06-30-guided-tour-docs-design.md) (the `lur docs` cookbook). This refines the renderer (`src/docs.rs`) on the same branch, before PR #52 merges.

## Problem

The first `lur docs` renderer (`src/docs.rs`) renders all heading levels
identically (bold only), has flat spacing, and shows code blocks as a single
bold-blue wash with no syntax highlighting. The layout is hard to scan and code
is hard to read.

## Goals

- A scannable heading hierarchy where each level (H1/H2/H3) is visually distinct.
- Syntax-highlighted Lua code blocks.
- Stay within the project's minimalism: **no new dependencies**, 16-color ANSI
  only (no truecolor assumption), and a clean `NO_COLOR`/non-TTY plain mode.

## Prior art

Aligned to [`glamour`](https://github.com/charmbracelet/glamour) (the renderer
behind `glow` and the GitHub `gh` CLI), default dark style:

- **H1** is a reverse/colored block bar (bold, fg-on-bg).
- **H2–H6 keep their `##`/`###` prefix** plus bold; level is signalled by the
  prefix and increasing left indent, not invented glyphs.
- **Code blocks** carry per-token highlight (keyword cyan, function green,
  string tan, comment gray).
- **Block quotes** use a `│ ` prefix; **inline code** gets a distinct fg/bg.

We adopt these conventions, mapped to portable 16-color SGR, and **keep the `#`
prefixes** (user decision).

## Non-goals (YAGNI)

- No truecolor / 256-color; 16-color SGR only.
- No highlighting for languages other than Lua — the guide's code fences are all
  `lua`. A non-`lua` fence (should one ever appear) renders un-highlighted.
- No configurable themes.
- The test harness (`tests/guide.rs`) is unchanged — it scans the **raw**
  Markdown, so styling never affects which examples run.

## Component 1: layout in `src/docs.rs::render`

A 16-color SGR palette, all empty strings when `color` is false:

| Element | Styling (color on) | Plain mode (color off) |
| --- | --- | --- |
| H1 | `\x1b[1;7m` (bold + reverse) around ` <text> `, reset | ` <text> ` (no code), still its own line |
| H2 | `\x1b[1m` bold; literal `## ` prefix kept | `## <text>` |
| H3 | `\x1b[1;36m` bold cyan; literal `### ` prefix kept | `### <text>` |
| H4–H6 | `\x1b[1m` bold; literal `#### ` … prefix kept | `#### <text>` … |
| body text | default fg | plain |
| inline code | `\x1b[36m` cyan (distinct from code blocks) | verbatim text |
| block quote | dim `\x1b[2m│\x1b[0m ` prefix | `│ ` prefix |
| code block | `│ ` left frame (dim) + highlighted body (Component 2) | `│ ` frame + plain code |

**Indentation.** Track a running left margin from the most recent heading level:
a heading of level `L` is emitted at margin `(L - 1) * 2`, and its following body
(paragraphs, lists, block quotes, code) is indented to the same margin. Code
block lines are `<margin spaces>│ <code line>`. This reproduces glamour's
depth-increasing indent. Headings always have one blank line before them.

The `#` prefix for H2+ is produced by the renderer (pulldown-cmark strips it from
the event stream); reconstruct it from the heading level (`"#".repeat(level)` +
space). H1 is the reverse bar (no `#` prefix).

## Component 2: Lua syntax highlighter (hand-rolled, in `src/docs.rs`)

`fn highlight_lua(code: &str, color: bool) -> String` — a small hand-rolled Lua
tokenizer (no dependency). When `color` is false it returns `code` unchanged.
When true, it wraps tokens in 16-color SGR:

| Token class | Color (16-color SGR) | Notes |
| --- | --- | --- |
| keyword | `\x1b[1;36m` bold cyan | `and break do else elseif end false for function goto if in local nil not or repeat return then true until while` |
| string | `\x1b[32m` green | `"…"`, `'…'` (with `\` escapes), and long brackets `[[ … ]]` / `[=*[ … ]=*]` |
| comment | `\x1b[90m` bright black | `-- …` to EOL, and long comments `--[[ … ]]` |
| number | `\x1b[33m` yellow | decimal, float, `0x…` hex |
| other | default | identifiers, operators, punctuation, whitespace |

Tokenizer requirements (pragmatic, not a full Lua lexer — but must not mis-scan
the guide's examples):

- Comments and long strings are recognized **before** operators, so `--` inside a
  string is not treated as a comment and vice-versa.
- Keywords match only on word boundaries (`endpoint` is not `end`).
- Each token's raw text is preserved exactly (highlighting only adds SGR), so
  `highlight_lua(code, false) == code` for any input.
- Unterminated constructs (e.g. a stray `"` ) degrade gracefully: emit the rest
  as that token rather than panicking. Never panics.

The renderer calls `highlight_lua` on each code block's text (the fence info
string is always `lua`/`lua ignore` in the guide; treat any code block as Lua).

## Data flow

```
GUIDE.md ──Parser events──> render(color)
                              ├─ heading L → bar/##-prefix + margin
                              ├─ paragraph/list/quote → margin + inline styles
                              └─ code block text → highlight_lua(code, color)
                                                     wrapped in `│ ` frame
```

`highlight_lua` is a pure string→string function with no markdown awareness; the
renderer owns layout, the highlighter owns token coloring. Each is testable in
isolation.

## Error handling

- `highlight_lua` never panics: an unterminated string/comment consumes to EOF as
  that token. Unknown bytes pass through as `other`.
- `render` remains infallible (pulldown-cmark parses any input).

## Testing strategy

- **`highlight_lua` unit tests:**
  - `color = false` → output byte-identical to input (round-trip), for a snippet
    containing a keyword, string, comment, and number.
  - `color = true` → keyword/string/comment/number each wrapped in their SGR code;
    a `--` inside a string is **not** colored as a comment; `endpoint` is not
    colored as the `end` keyword.
  - Unterminated string does not panic and emits the remainder green.
- **`render` unit tests (extend existing):**
  - color on: H1 emits `\x1b[1;7m`; an H3 emits `\x1b[1;36m`; a code block body
    contains a keyword SGR (highlight applied) and a `│` frame.
  - color off: no `\x1b`; the `## `/`### ` prefixes are present; the `│ ` code
    frame is present; code text is verbatim (no SGR).
- **Existing suites unchanged:** `tests/guide.rs` still runs the guide examples;
  the two prior `docs::render` tests are updated to the new output shape.
- Gate: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`,
  `cargo nextest run` all green; eyeball `cargo run -- docs`.

## File structure

All changes are in `src/docs.rs` (renderer + highlighter live together — they are
the two halves of one responsibility and share the color gate). If the file grows
past ~250 lines and the tokenizer feels separable, the implementer may split the
highlighter into `src/docs/lua.rs`, but a single focused file is acceptable.
