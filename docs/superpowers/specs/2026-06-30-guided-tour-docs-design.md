# Guided tour: embedded `lur docs` cookbook with tested examples

**Status:** approved design, ready for implementation plan
**Date:** 2026-06-30

## Problem

`lur`'s docs are split between [README.md](../../../README.md) (terse: CLI, sandbox
model, `lur.*` API reference) and [ARCHITECTURE.md](../../../ARCHITECTURE.md)
(internal design). There is no single end-to-end "you know Lua, here's how to get
productive with lur" guide that walks each capability with runnable examples.

This is the last planned item in the lur follow-up backlog, intentionally written
after the API surface and diagnostics settled so it is written once.

## Audience and goals

- **Audience:** developers who already write Lua and want to get productive with
  `lur` quickly. Cookbook style, not a beginner tutorial.
- **Always available:** the guide is embedded in the binary and printed by a
  `lur docs` subcommand, so it can be read offline at any time.
- **Enumerated:** every user-facing `lur.*` capability has a section explaining
  each API function's usage.
- **Trustworthy:** the Lua code blocks in the guide are part of the test suite —
  they run and self-verify, so the guide cannot silently drift from the runtime.

## Non-goals (YAGNI)

- No `lur docs <capability>` section filter — `lur docs` prints the whole guide;
  users pipe to a pager. (Possible follow-up.)
- No pager/TTY paging built in.
- No turnkey Markdown-renderer crate (e.g. `termimad`) — its `crossterm` /
  `crossbeam` / `serde` footprint is too heavy for this project. Rendering is
  hand-rolled over a parser (see component 2).
- The guide does not duplicate the full CLI flag reference; it points to README
  for exhaustive flags and the policy/sandbox model.

## Components

### 1. `docs/GUIDE.md` — the single source of truth

A Markdown cookbook. Structure:

1. **Intro** — what `lur` is; the two modes (`lur script.lua` one-shot,
   `lur serve app.lua` server); a one-paragraph sandbox/policy overview that
   points to README for the full flag set and profiles.
2. **Per-capability sections**, one per user-facing `lur.*` table, each with a
   1–2 sentence intro and runnable `assert`-based example(s) covering every API
   function. Capabilities (mirroring the README API grouping):
   `json`, `base64`, `crypto`, `cookie`, `time`, `log`, `args`, `state`, `io`,
   `fs`, `env`, `http`, `db`, `kv`, `async`, `serve`. (The `lur.null` JSON
   sentinel is shown inside the `json` section, not given its own heading.)

Each section heading includes the capability's table name verbatim (e.g. a
heading containing `lur.crypto`) so the drift test (below) can find it.

### 2. `lur docs` subcommand + Markdown renderer

Dispatched the same way `serve` is: `main()` peeks `argv[1]`. When it equals
`docs`, render the embedded guide to stdout and exit `0`:

```rust
// in main(), alongside the existing `serve` peek
if argv.get(1).map(String::as_str) == Some("docs") {
    const GUIDE: &str = include_str!("../docs/GUIDE.md");
    print!("{}", docs::render(GUIDE, docs::stdout_color()));
    return ExitCode::SUCCESS;
}
```

`include_str!` bakes the guide into the binary at compile time (single source,
no runtime file lookup). The one-shot `lur script.lua [args]` grammar is
untouched. Add a one-line mention of `lur docs` to the README CLI section and
the clap `about` text for discoverability (clap does not see the peeked
subcommand otherwise).

**Renderer (`src/docs.rs`, new module).** A hand-rolled ANSI sink over
[`pulldown-cmark`](https://crates.io/crates/pulldown-cmark) (0.13.4, MIT,
published 2026-05-20 — clears the 7-day cooldown). Add it with
`default-features = false` so only the parser is pulled in (non-optional deps:
`bitflags`, `memchr`, `unicase` — all tiny and ubiquitous); the `html` output,
`getopts`, and `serde` features stay off.

- `pub fn render(markdown: &str, color: bool) -> String` walks the
  `pulldown_cmark::Parser` event stream and emits ANSI-styled text:
  - **Headings** → bold (deeper levels dimmer/with leading `#`s preserved for
    hierarchy), a blank line around them.
  - **Inline code** / **fenced code blocks** → a distinct color (e.g. the same
    bold-blue used by diagnostics), code blocks indented.
  - **Emphasis** → italic/underline; **strong** → bold.
  - **Lists** → `- ` bullets with nesting indentation; **block quotes** → a `│ `
    prefix.
  - Links render as `text (url)`.
  When `color` is `false`, every style string is empty, so the output is the
  document's text with structural whitespace but **no escape codes** — never raw
  half-rendered Markdown with stray `**`/`` ` `` noise left to the reader. (The
  parser still strips the Markdown syntax; only the ANSI styling is gated.)
- `pub fn stdout_color() -> bool` mirrors `diagnostics::stderr_color` but checks
  **stdout** (`lur docs` writes there): colorize only when stdout is a TTY and
  `NO_COLOR` is unset/empty. The shared env+TTY predicate
  (`color_from_env(no_color, is_tty)`) is reused — lift it to a small shared
  helper rather than duplicating the rule in two modules.

This keeps full control over the styling, adds a near-zero dependency footprint,
and gives the same `NO_COLOR`/TTY behavior as the diagnostics renderer.

### 3. `tests/guide.rs` — code blocks as tests

An integration test with two responsibilities.

**(a) Run-and-verify the examples.**
- Read the guide via `include_str!("../docs/GUIDE.md")` (compile-time, same
  source the binary ships).
- A small hand-rolled fenced-code scanner (no new dependency) walks the lines,
  recognizes ```` ```lua ```` opening fences, and collects body lines until the
  closing ```` ``` ````. The fence info string after `lua` selects behavior:
  - ```` ```lua ````        → **runnable**; executed and must succeed.
  - ```` ```lua ignore ```` → **shown only**; collected but skipped.
- Each runnable block is run **in its own fresh `lur::runtime::Runtime`**
  (one-shot) with its **own** temp dir and temp db, so blocks never interfere and
  each example stays self-contained. The config is permissive-but-sandboxed so
  real capabilities work:
  - **allow-all policy** (equivalent to `-A`),
  - a fresh **temp working directory** allowlisted for `lur.fs`,
  - a **temp SQLite db** path wired for `lur.db`.
  This means `fs`, `db`, `crypto`, `json`, `base64`, `cookie`, `time`, `log`,
  `args`, `state`, `io` examples are exercised for real. A block that errors
  fails the test, naming the block (by ordinal and a snippet) for diagnosis.
- **`ignore` targets:** `http` (hits the network — flaky/forbidden in tests),
  `serve` (long-running server), and any example that depends on external env.

**(b) Drift guard.**
A second test asserts every user-facing capability table name appears in the
guide text. The list is the canonical set of `lur.*` tables:
`json base64 crypto cookie time log args state io fs env http db kv async serve`
(internal plumbing — `argcheck` — and the `null` sentinel are excluded; `io`
covers `lur.stdin`/`lur.stdout`). Adding a capability without documenting it
fails this test.

## Data flow

```
docs/GUIDE.md ──include_str!──> binary ──docs::render(NO_COLOR/TTY)──> stdout
      │
      └────────include_str!──> tests/guide.rs ──scan fences──> run runnable
                              (raw markdown)                 └─> assert names present
```

The Markdown file is the only source; both the binary and the test read it via
`include_str!`, so they cannot disagree. The renderer is independent of the test
harness: the harness scans the **raw** Markdown for ```` ```lua ```` fences, so
styling never affects which examples run.

## Error handling

- `lur docs`: render + print + exit `0`; no failure path. `pulldown-cmark` parses
  any input infallibly, and the content is compiled in.
- Test harness: a runnable block that raises surfaces as a test failure with the
  block's ordinal and a leading snippet. An `ignore`d block is never executed.
  If the drift test finds a missing capability name, it fails listing the
  missing names.

## Testing strategy

- The examples *are* the regression tests (assert-based self-verification).
- Pure-compute capabilities run end-to-end; network/server examples are `ignore`.
- The drift guard keeps the capability list and the guide's sections in sync.
- **Renderer unit tests** (`src/docs.rs`): `color = true` output contains ANSI
  escape codes; `color = false` output contains **no** escape codes *and* none of
  the Markdown markup characters that the parser strips (no stray `**`, `` ` ``,
  `#` heading markers) — proving plain mode is clean text, not half-rendered
  Markdown. Plus the shared `color_from_env` truth table (TTY × `NO_COLOR`),
  which already exists for diagnostics and is reused here.
- Existing suites (`cargo nextest run`) continue to gate; `tests/guide.rs` joins
  them. Production code changes are limited to the `main()` dispatch arm, the new
  `src/docs.rs` module, and lifting `color_from_env` into a shared helper.

## Implementation note: batching

The bulk of the work is writing `docs/GUIDE.md`. The implementation plan should
land the harness first (so examples are validated as they are written), then add
capability sections in batches, running `cargo nextest run --test guide` after
each batch.
