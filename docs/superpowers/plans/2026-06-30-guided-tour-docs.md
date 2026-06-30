# Guided Tour (`lur docs`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an embedded `lur docs` cookbook whose Lua examples are run as tests, so the guide can never drift from the runtime.

**Architecture:** A single `docs/GUIDE.md` is `include_str!`'d into the binary. `lur docs` renders it to stdout via a hand-rolled ANSI sink over `pulldown-cmark` (`src/docs.rs`), gated by a shared `src/color.rs` (NO_COLOR + TTY). A `tests/guide.rs` harness scans the raw Markdown for ` ```lua ` fences, runs the runnable ones under a permissive sandbox, and asserts every capability is documented.

**Tech Stack:** Rust (edition 2024), `pulldown-cmark` (parser only), `mlua`/Luau via the existing `lur::runtime::Runtime`, `tempfile` (dev-dep) for the test harness.

## Global Constraints

- Edition 2024; do **not** bump `rust-version` (MSRV).
- Tests run with `cargo nextest run` (NOT `cargo test`).
- `cargo fmt --all` before every commit; `cargo clippy --all-targets -- -D warnings` must pass.
- New dependency `pulldown-cmark = { version = "0.13.4", default-features = false }` — version is ≥7 days old (published 2026-05-20), MIT. No other new runtime deps.
- Stage files explicitly by name (never `git add -A`/`.`). Commits are GPG-signed (default).
- Canonical user-facing capability list (drift guard): `json base64 crypto cookie time log args state io fs env http db kv async serve`.
- Color codes reuse the diagnostics palette: bold-blue `\x1b[1;34m`, bold `\x1b[1m`, reset `\x1b[0m`.

---

### Task 1: Extract `src/color.rs` shared color gate

Move the NO_COLOR/TTY decision out of `diagnostics.rs` into a cross-cutting module so both stderr (diagnostics) and stdout (`lur docs`) share one rule.

**Files:**
- Create: `src/color.rs`
- Modify: `src/lib.rs` (add `pub mod color;`)
- Modify: `src/diagnostics.rs` (remove `color_from_env` + `stderr_color`, and their `use std::ffi::OsStr; use std::io::IsTerminal;` + the `color_from_env_truth_table` test)
- Modify: `src/main.rs:330-337` (`lur::diagnostics::stderr_color()` → `lur::color::stderr_color()`)
- Modify: `src/serve.rs` (both `crate::diagnostics::stderr_color()` → `crate::color::stderr_color()`)

**Interfaces:**
- Produces: `lur::color::color_from_env(no_color: Option<&std::ffi::OsStr>, stream_is_tty: bool) -> bool`; `lur::color::stderr_color() -> bool`; `lur::color::stdout_color() -> bool`.

- [ ] **Step 1: Write `src/color.rs` with the failing tests first**

```rust
//! Shared decision for whether to emit ANSI color, honoring `NO_COLOR` and TTY
//! state. Used by both the diagnostics renderer (stderr) and `lur docs` (stdout).

use std::ffi::OsStr;
use std::io::IsTerminal;

/// Color is on only when the target stream is a TTY and `NO_COLOR` is unset or
/// empty (the de-facto standard: a non-empty `NO_COLOR` disables color
/// regardless of its value).
pub fn color_from_env(no_color: Option<&OsStr>, stream_is_tty: bool) -> bool {
    stream_is_tty && no_color.is_none_or(|v| v.is_empty())
}

/// Whether diagnostics written to stderr should be colorized.
pub fn stderr_color() -> bool {
    color_from_env(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stderr().is_terminal(),
    )
}

/// Whether `lur docs` output written to stdout should be colorized.
pub fn stdout_color() -> bool {
    color_from_env(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stdout().is_terminal(),
    )
}

#[cfg(test)]
mod tests {
    use super::color_from_env;
    use std::ffi::OsStr;

    #[test]
    fn color_from_env_truth_table() {
        assert!(color_from_env(None, true));
        assert!(!color_from_env(Some(OsStr::new("1")), true));
        assert!(color_from_env(Some(OsStr::new("")), true));
        assert!(!color_from_env(None, false));
        assert!(!color_from_env(Some(OsStr::new("1")), false));
    }
}
```

- [ ] **Step 2: Add the module and remove the old copy**

In `src/lib.rs` add `pub mod color;` (keep modules alphabetical: before `pub mod config;`).

In `src/diagnostics.rs`, delete the `use std::ffi::OsStr;` and `use std::io::IsTerminal;` lines, the `color_from_env` fn, the `stderr_color` fn, and the `color_from_env_truth_table` test plus its `use super::{color_from_env, render};`/`use std::ffi::OsStr;` (the remaining tests only need `use super::render;`).

- [ ] **Step 3: Update the call sites**

`src/main.rs` — change the diagnostics render call's last argument to `lur::color::stderr_color()`.
`src/serve.rs` — change both render calls' last argument to `crate::color::stderr_color()`.

- [ ] **Step 4: Run fmt, clippy, tests**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run`
Expected: PASS — `color::tests::color_from_env_truth_table` passes; all 186 prior tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/color.rs src/lib.rs src/diagnostics.rs src/main.rs src/serve.rs
git commit -m "refactor(color): extract shared NO_COLOR/TTY gate into src/color.rs"
```

---

### Task 2: `pulldown-cmark` dependency + `src/docs.rs` renderer

**Files:**
- Modify: `Cargo.toml` (`[dependencies]`)
- Create: `src/docs.rs`
- Modify: `src/lib.rs` (add `pub mod docs;`)

**Interfaces:**
- Consumes: `lur::color::stdout_color`.
- Produces: `lur::docs::render(markdown: &str, color: bool) -> String`.

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` `[dependencies]`, alphabetically (after `mlua`):
```toml
pulldown-cmark = { version = "0.13.4", default-features = false }
```

- [ ] **Step 2: Write the failing renderer tests**

Create `src/docs.rs`:
```rust
//! Render the embedded `GUIDE.md` to ANSI-styled text for `lur docs`. A
//! hand-rolled sink over `pulldown-cmark` — color is gated so plain mode is
//! clean text with the Markdown markup stripped, never half-rendered source.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(docs)'`
Expected: FAIL to compile — `render` is not defined.

- [ ] **Step 4: Implement `render`**

Add above the tests in `src/docs.rs`:
```rust
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
                out.push_str("\n");
                out.push_str(bold);
            }
            Event::End(TagEnd::Heading(_)) => {
                out.push_str(reset);
                out.push('\n');
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => out.push('\n'),
            Event::Start(Tag::Strong) | Event::Start(Tag::Emphasis) => out.push_str(bold),
            Event::End(TagEnd::Strong) | Event::End(TagEnd::Emphasis) => out.push_str(reset),
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
            Event::End(TagEnd::Item) => out.push('\n'),
            Event::Start(Tag::BlockQuote(_)) => out.push_str("\u{2502} "),
            Event::End(TagEnd::BlockQuote(_)) => out.push('\n'),
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
            Event::Start(Tag::Link { dest_url, .. }) => {
                // Emitted as `text (url)`; capture url for the end tag.
                out.push_str(""); // text comes via Event::Text
                let _ = dest_url; // url appended below on link text via simple form
            }
            Event::Code(text) => {
                out.push_str(code);
                out.push_str(&text);
                out.push_str(reset);
            }
            Event::Text(text) => {
                if in_code_block {
                    // Indent each code line by two spaces.
                    for (i, line) in text.split_inclusive('\n').enumerate() {
                        if i == 0 || !line.is_empty() {
                            out.push_str("  ");
                        }
                        out.push_str(line);
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

    // Collapse the leading newline and trailing whitespace.
    format!("{}\n", out.trim())
}
```

Note on links: the simple form above renders the link **text** inline (URL omitted) to keep the sink small; if you want `text (url)`, buffer the url on `Start(Tag::Link)` into a `Vec<String>` stack and append ` (url)` on `End(TagEnd::Link)`. The plain-text test does not require the URL, so the minimal form is acceptable; add the url form only if a later review asks.

In `src/lib.rs` add `pub mod docs;` (alphabetical: after `pub mod diagnostics;`).

- [ ] **Step 5: Run fmt, clippy, tests**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run -E 'test(docs)'`
Expected: PASS — both docs tests green.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/docs.rs src/lib.rs
git commit -m "feat(docs): markdown renderer for lur docs over pulldown-cmark"
```

---

### Task 3: `docs/GUIDE.md` skeleton + `lur docs` subcommand

Create the guide with the intro and every capability heading present (so `include_str!` compiles and the drift guard added in Task 4 passes), then wire the subcommand.

**Files:**
- Create: `docs/GUIDE.md`
- Modify: `src/main.rs` (argv peek in `main()`; clap `about` mention)
- Modify: `README.md` (CLI section — one line)
- Test: `tests/docs_cmd.rs`

**Interfaces:**
- Consumes: `lur::docs::render`, `lur::color::stdout_color`.

- [ ] **Step 1: Create the skeleton `docs/GUIDE.md`**

Every capability heading must contain its `lur.<name>` token verbatim. Use this skeleton (bodies are filled in Tasks 5–8):
```markdown
# lur guide

`lur` runs Luau in a sandbox. Two modes share one core: one-shot
`lur script.lua [args]` runs a script to completion; `lur serve app.lua` serves
it as a long-running HTTP server. Capabilities live under the `lur.*` global;
each is gated by a policy (default profile is `strict` — deny-all). See the
[README](../README.md) for the full flag set and the sandbox model.

Every example below is run as part of the test suite, so it stays correct.

## Data & I/O

### lur.json
### lur.base64
### lur.crypto
### lur.cookie
### lur.time
### lur.log
### lur.io

## State & arguments

### lur.args
### lur.state

## Capabilities (policy-gated)

### lur.fs
### lur.env
### lur.http

## Storage

### lur.db
### lur.kv

## Concurrency

### lur.async

## Server mode

### lur.serve
```

- [ ] **Step 2: Write the failing subcommand test**

Create `tests/docs_cmd.rs`:
```rust
use assert_cmd::Command;

#[test]
fn docs_prints_the_guide() {
    Command::cargo_bin("lur")
        .unwrap()
        .arg("docs")
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicates::str::contains("lur.json"));
}

#[test]
fn docs_honors_no_color() {
    let out = Command::cargo_bin("lur")
        .unwrap()
        .arg("docs")
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!out.contains(&0x1b), "no ANSI escape with NO_COLOR: {out:?}");
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo nextest run --test docs_cmd`
Expected: FAIL — `lur docs` is unknown (clap error / non-zero exit).

- [ ] **Step 4: Wire the subcommand in `main()`**

In `src/main.rs`, in `fn main()` immediately after the `serve` peek block and before `run_one_shot(Cli::parse())`:
```rust
    if argv.get(1).map(String::as_str) == Some("docs") {
        const GUIDE: &str = include_str!("../docs/GUIDE.md");
        print!("{}", lur::docs::render(GUIDE, lur::color::stdout_color()));
        return ExitCode::SUCCESS;
    }
```
Add a one-line hint to the clap `#[command(... about)]` or the `about` text so `lur --help` mentions `lur docs` prints the guide. Add one line to the README CLI/usage section: `` `lur docs` prints the embedded usage guide. ``

- [ ] **Step 5: Run to verify it passes**

Run: `cargo nextest run --test docs_cmd`
Expected: PASS — both tests green.

- [ ] **Step 6: Commit**

```bash
git add docs/GUIDE.md tests/docs_cmd.rs src/main.rs README.md
git commit -m "feat(docs): add lur docs subcommand and guide skeleton"
```

---

### Task 4: `tests/guide.rs` harness — fence scanner, runner, drift guard

**Files:**
- Create: `tests/guide.rs`

**Interfaces:**
- Consumes: `lur::runtime::{Runtime, RuntimeConfig}`, `lur::policy::Policy`, `tempfile`.

- [ ] **Step 1: Write the harness**

Create `tests/guide.rs`:
```rust
//! The Lua examples in docs/GUIDE.md ARE the test suite. Each ```lua block is
//! run under a permissive sandbox; ```lua ignore blocks are shown but skipped.

use std::sync::Arc;

use lur::policy::Policy;
use lur::runtime::{Runtime, RuntimeConfig};

const GUIDE: &str = include_str!("../docs/GUIDE.md");

/// A fenced ```lua block and whether it is marked `ignore`.
struct Block {
    code: String,
    ignore: bool,
}

/// Scan raw Markdown for ```lua fences. The info string after `lua` selects
/// behavior: empty → runnable, `ignore` → skipped.
fn lua_blocks(md: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines = md.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        let info = info.trim();
        if info != "lua" && info != "lua ignore" {
            continue;
        }
        let ignore = info == "lua ignore";
        let mut code = String::new();
        for body in lines.by_ref() {
            if body.trim_start().starts_with("```") {
                break;
            }
            code.push_str(body);
            code.push('\n');
        }
        blocks.push(Block { code, ignore });
    }
    blocks
}

/// A permissive-but-sandboxed config: full fs/env/net (loose), with a temp db.
fn permissive_config(db_path: std::path::PathBuf) -> RuntimeConfig {
    RuntimeConfig {
        policy: Arc::new(Policy::loose().expect("loose policy")),
        db_path: Some(db_path),
        ..Default::default()
    }
}

#[test]
fn every_runnable_example_succeeds() {
    let blocks = lua_blocks(GUIDE);
    assert!(!blocks.is_empty(), "no ```lua blocks found in the guide");
    for (i, block) in blocks.iter().enumerate() {
        if block.ignore {
            continue;
        }
        // Each block gets its own temp dir (cwd for relative fs paths) + db.
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_current_dir(dir.path()).expect("chdir");
        let rt = Runtime::with_config(permissive_config(dir.path().join("guide.db")))
            .expect("runtime builds");
        if let Err(e) = rt.run(&block.code) {
            panic!(
                "guide example #{i} failed: {e}\n--- block ---\n{}",
                block.code
            );
        }
    }
}

#[test]
fn every_capability_is_documented() {
    const CAPS: &[&str] = &[
        "json", "base64", "crypto", "cookie", "time", "log", "args", "state",
        "io", "fs", "env", "http", "db", "kv", "async", "serve",
    ];
    let missing: Vec<&str> = CAPS
        .iter()
        .copied()
        .filter(|c| {
            // `io` is documented as lur.stdin / lur.stdout.
            if *c == "io" {
                return !GUIDE.contains("lur.stdin") && !GUIDE.contains("lur.stdout");
            }
            !GUIDE.contains(&format!("lur.{c}"))
        })
        .collect();
    assert!(missing.is_empty(), "capabilities missing from the guide: {missing:?}");
}
```

- [ ] **Step 2: Run the harness**

Run: `cargo nextest run --test guide`
Expected: `every_capability_is_documented` PASSES (skeleton has every heading). `every_runnable_example_succeeds` either passes trivially (no runnable blocks yet) or, if the skeleton has none, the `assert!(!blocks.is_empty())` fails — if so, add one trivial runnable block under `### lur.json` now: `` ```lua ``\n`assert(lur.json.encode(true) == "true")`\n`` ``` `` and re-run.

- [ ] **Step 3: Commit**

```bash
git add tests/guide.rs docs/GUIDE.md
git commit -m "test(docs): run guide lua examples + assert capability coverage"
```

---

### Task 5: Document the pure-compute capabilities (json, base64, crypto, cookie, time)

Fill these five sections with runnable `assert` examples covering every function. Run the guide harness after writing.

**Files:**
- Modify: `docs/GUIDE.md`

- [ ] **Step 1: Write the sections**

Replace the five skeleton headings with (each ` ```lua ` block is runnable):

````markdown
### lur.json

Encode/decode JSON. JSON `null` becomes `lur.null` (a sentinel distinct from
`nil`, since a `nil` value means the key is absent); UTF-8 only — base64 binary.

```lua
local s = lur.json.encode({ ok = true, n = 3 })
local v = lur.json.decode(s)
assert(v.ok == true and v.n == 3)
assert(lur.json.decode("null") == lur.null)
```

### lur.base64

```lua
local enc = lur.base64.encode("hi")
assert(enc == "aGk=")
assert(lur.base64.decode(enc) == "hi")
```

### lur.crypto

Pure-compute hashing, HMAC, hex, CSPRNG bytes, and constant-time compare.
Digests are raw bytes — bridge through `hex` or `lur.base64`. `sha1`/`md5` are
legacy-interop only.

```lua
local digest = lur.crypto.sha256("abc")
assert(lur.crypto.hex.encode(digest)
  == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
assert(lur.crypto.hex.decode(lur.crypto.hex.encode(digest)) == digest)

local mac = lur.crypto.hmac_sha256("key", "msg")
assert(lur.crypto.constant_eq(mac, lur.crypto.hmac_sha256("key", "msg")))
assert(not lur.crypto.constant_eq(mac, lur.crypto.hmac_sha256("key", "other")))

assert(#lur.crypto.random_bytes(16) == 16)
assert(#lur.crypto.sha512("x") == 64 and #lur.crypto.sha1("x") == 20)
assert(#lur.crypto.md5("x") == 16)
assert(#lur.crypto.hmac_sha512("k", "m") == 64)
assert(#lur.crypto.hmac_sha1("k", "m") == 20)
```

### lur.cookie

Parse a `Cookie` header into a table; build one `Set-Cookie` value. Values are
raw bytes (base64 arbitrary data).

```lua
local jar = lur.cookie.parse("a=1; b=2")
assert(jar.a == "1" and jar.b == "2")

local set = lur.cookie.serialize("sid", "xyz", { http_only = true, path = "/" })
assert(set:find("sid=xyz", 1, true) == 1)
assert(set:find("HttpOnly", 1, true))
```

### lur.time

Clocks and timestamp parsing that fill the gaps in `os.*`. All values are
integer milliseconds.

```lua
assert(lur.time.now_ms() > 0)

local a = lur.time.monotonic_ms()
local b = lur.time.monotonic_ms()
assert(b >= a)

assert(lur.time.parse_rfc3339("1970-01-01T00:00:01Z") == 1000)
assert(lur.time.parse_http_date("Thu, 01 Jan 1970 00:00:01 GMT") == 1000)
```
````

- [ ] **Step 2: Run the harness**

Run: `cargo nextest run --test guide`
Expected: PASS. If any `assert` fails, the panic names the block and shows the code — fix the example to match the real API (cross-check `src/capabilities/<name>.rs`).

- [ ] **Step 3: Commit**

```bash
git add docs/GUIDE.md
git commit -m "docs(guide): json, base64, crypto, cookie, time sections"
```

---

### Task 6: Document runtime/sandbox capabilities (log, io, args, state, async)

**Files:**
- Modify: `docs/GUIDE.md`

- [ ] **Step 1: Write the sections**

````markdown
### lur.log

`info`/`warn`/`error` write to **stderr** (stdout is the data channel); no
implicit newline.

```lua
lur.log.info("starting\n")
lur.log.warn("careful\n")
lur.log.error("oops\n")
```

### lur.io

`lur.stdout.write(bytes)` / `flush()` is the data channel (raw bytes, no
newline). `lur.stdin.read()` drains all input, `read(n)` reads up to `n` (`nil`
at EOF), and `lines()` iterates newline-stripped lines.

```lua
lur.stdout.write("data\n")
lur.stdout.flush()
```

```lua ignore
-- Reading stdin needs piped input; run as: echo hi | lur read.lua
for line in lur.stdin.lines() do
  lur.stdout.write(line .. "\n")
end
```

### lur.args

`lur.args.positional` is a 1-indexed array; `lur.args.flags` maps
`--name value` / `--name=value` to the string and a bare `--flag` to `true`.

```lua
assert(type(lur.args.positional) == "table")
assert(type(lur.args.flags) == "table")
```

### lur.state

Process-wide shared state across the VM pool (primitives only): `get`/`set`
(`nil` deletes), `incr` (atomic add), `update` (optimistic CAS).

```lua
lur.state.set("hits", 0)
assert(lur.state.incr("hits", 2) == 2)
lur.state.update("hits", function(n) return (n or 0) + 1 end)
assert(lur.state.get("hits") == 3)
lur.state.set("hits", nil)
assert(lur.state.get("hits") == nil)
```

### lur.async

`sleep(ms)` and combinators over arrays of zero-arg functions: `all` (fail-fast),
`race`/`any` (first to settle/succeed), `settled` (never raises). Lua runs one
step at a time; tasks interleave at I/O await points.

```lua
lur.async.sleep(1)
local results = lur.async.all({
  function() return 1 end,
  function() return 2 end,
})
assert(results[1] == 1 and results[2] == 2)

local settled = lur.async.settled({
  function() error("boom") end,
  function() return "ok" end,
})
assert(settled[1].ok == false)
assert(settled[2].ok == true and settled[2].value == "ok")
```
````

- [ ] **Step 2: Run the harness**

Run: `cargo nextest run --test guide`
Expected: PASS. Fix any example whose `assert` trips against the real API (`src/capabilities/{log,io,args,state,async_ops}.rs`).

- [ ] **Step 3: Commit**

```bash
git add docs/GUIDE.md
git commit -m "docs(guide): log, io, args, state, async sections"
```

---

### Task 7: Document policy-gated + storage capabilities (fs, env, db, kv)

Runnable under the harness's loose policy, temp cwd, and temp db. `fs` examples
use **relative** paths (resolved under the per-block temp cwd).

**Files:**
- Modify: `docs/GUIDE.md`

- [ ] **Step 1: Write the sections**

````markdown
### lur.fs

`read(path) → bytes`, `write(path, bytes)`. Paths are canonicalized before the
allowlist check, so `..`/symlink escapes are rejected. Grant access with
`--allow-fs-read`/`--allow-fs-write`/`--allow-fs` (or `--loose`/`-A`).

```lua
lur.fs.write("note.txt", "hello")
assert(lur.fs.read("note.txt") == "hello")
```

### lur.env

`lur.env(name) → string | nil`. Returns `nil` for **both** "denied" and "unset",
so it can't be used as an oracle. Grant names with `--allow-env` (or `-A`).

```lua
assert(lur.env("LUR_GUIDE_DEFINITELY_UNSET") == nil)
```

### lur.db

Requires `--db <path>`. `exec(sql, ...params)` returns
`{ rows_affected, last_insert_id }`; `query(sql, ...params)` returns an array of
row tables keyed by column; `tx(fn)` runs on a pinned connection (commit on
return, rollback on error). Use `?` placeholders.

```lua
lur.db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
local r = lur.db.exec("INSERT INTO t (name) VALUES (?)", "alice")
assert(r.rows_affected == 1)

local rows = lur.db.query("SELECT name FROM t WHERE id = ?", r.last_insert_id)
assert(rows[1].name == "alice")

lur.db.tx(function(tx)
  tx.exec("INSERT INTO t (name) VALUES (?)", "bob")
end)
assert(#lur.db.query("SELECT id FROM t") == 2)
```

### lur.kv

A simple key/value store over the same SQLite pool: `get(key) → bytes | nil`,
`set(key, bytes)`, `delete(key)`.

```lua
lur.kv.set("greeting", "hi")
assert(lur.kv.get("greeting") == "hi")
lur.kv.delete("greeting")
assert(lur.kv.get("greeting") == nil)
```
````

- [ ] **Step 2: Run the harness**

Run: `cargo nextest run --test guide`
Expected: PASS. If `tx`'s inner call shape differs (`tx.exec` vs `tx:exec`), check `src/capabilities/db.rs` and adjust the example to the real form.

- [ ] **Step 3: Commit**

```bash
git add docs/GUIDE.md
git commit -m "docs(guide): fs, env, db, kv sections"
```

---

### Task 8: Document network + server mode (http, serve) and finalize

`http` (network) and `serve` (long-running) examples are illustrative — tag them
` ```lua ignore ` so the harness shows but does not run them.

**Files:**
- Modify: `docs/GUIDE.md`

- [ ] **Step 1: Write the sections**

````markdown
### lur.http

`request(method, url, opts?)` plus `get`/`post`/`put`/`patch`/`delete`/`head`.
`opts` may set `headers`, `query`, `body` **or** `json`, and `timeout` (ms).
Response: `{ status, body, headers, headers_all, json() }`. Every hop is checked
against the network allowlist and the SSRF guard; grant hosts with `--allow-net`.

```lua ignore
local res = lur.http.get("https://example.com", { timeout = 5000 })
assert(res.status == 200)

local posted = lur.http.post("https://api.example.com/items", {
  json = { name = "widget" },
})
local body = posted.json()
```

### lur.serve

Server mode (`lur serve app.lua`). Registration happens once at load.
`serve.http(method, path, handler)` — paths may contain `:name` segments bound
into `req.params`; the handler returns `{ status?, body? }`. `serve.cron(spec,
handler, opts?)` takes a 6-field cron expression and optional `name`/`overlap`/
`timeout`. `req` exposes `method`, `path`, `params`, `query`, `query_all`,
`headers`, `cookies`, `body`, and `json()`.

```lua ignore
lur.serve.http("POST", "/echo", function(req)
  local data = req.json()
  return { status = 200, body = lur.json.encode(data) }
end)

lur.serve.cron("0 */5 * * * *", function()
  lur.log.info("tick\n")
end, { name = "ticker", overlap = false })
```
````

- [ ] **Step 2: Run the full gate**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run`
Expected: PASS — all suites green, including `guide::every_capability_is_documented` (now every capability is present) and `guide::every_runnable_example_succeeds`.

- [ ] **Step 3: Eyeball the rendered output**

Run: `cargo run -q -- docs | head -40`
Expected: readable plain-text guide (piped → no color), markup stripped, sections legible.

- [ ] **Step 4: Update ARCHITECTURE module map**

In `ARCHITECTURE.md`, add rows for the two new modules next to `src/diagnostics.rs`:
- `src/color.rs` — shared `NO_COLOR`/TTY color gate (`color_from_env`, `stderr_color`, `stdout_color`).
- `src/docs.rs` — `render` for `lur docs`: pulldown-cmark → ANSI sink, color via `color::stdout_color`.

- [ ] **Step 5: Commit**

```bash
git add docs/GUIDE.md ARCHITECTURE.md
git commit -m "docs(guide): http, serve sections; document new modules"
```

---

## Self-Review

**Spec coverage:**
- `docs/GUIDE.md` single source → Tasks 3, 5–8. ✓
- `lur docs` subcommand + `include_str!` → Task 3. ✓
- Markdown renderer over pulldown-cmark (`default-features = false`), hand-rolled sink, plain mode strips markup → Task 2. ✓
- `stdout_color`/shared `color_from_env` lift → Task 1. ✓
- `tests/guide.rs`: fence scanner, per-block fresh Runtime + temp dir + temp db (loose policy), `ignore` for http/serve, drift guard → Task 4 (+ ignore tags applied in Task 8). ✓
- Renderer unit tests (color on/off, no markup in plain) → Task 2. ✓
- Capability set incl. `kv`, `async` → drift list in Task 4 + sections in Tasks 6–7. ✓

**Placeholder scan:** No TBD/TODO; every code step shows full code. The link `text (url)` form is explicitly optional with a stated fallback (minimal text-only sink) that the test accepts — not a placeholder.

**Type consistency:** `render(&str, bool) -> String`, `color_from_env(Option<&OsStr>, bool) -> bool`, `stderr_color()/stdout_color() -> bool`, `Policy::loose() -> io::Result<Policy>`, `RuntimeConfig { policy: Arc<Policy>, db_path: Option<PathBuf>, .. }`, `Runtime::with_config(RuntimeConfig) -> Result<_, RunError>`, `Runtime::run(&str)` — consistent across tasks and matches the current source.
