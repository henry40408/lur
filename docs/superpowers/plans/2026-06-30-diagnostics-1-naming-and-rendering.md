# Diagnostics (1/2): chunk naming + rustc-style rendering — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make script errors point at the user's file and line (not lur's internals) and render them rustc-style with a source snippet, in both one-shot and server mode.

**Architecture:** Three steps. (1) Name every `lua.load(source)` chunk from the CLI path so error positions read `app.lua:2:`. (2) A new pure `src/diagnostics.rs` renderer that turns mlua's error `Display` string into a rustc-style report (header + `-->` + source snippet + caret + filtered traceback), with a safe fallback. (3) Wire the renderer into one-shot and server error output.

**Tech Stack:** Rust (edition 2024), mlua 0.11 (Luau). No new dependencies.

This is plan 1 of 2 for the diagnostics spec (`docs/superpowers/specs/2026-06-30-diagnostics-design.md`); it covers spec components 1–3. Component 4 (per-capability argument messages) is a separate later plan.

## Global Constraints

- **Chunk name = the CLI path as the user typed it** (`cli.script` / `cli.app`), not canonicalized and not reduced to a basename. Set via Lua's file convention (a leading `@`) so it renders as `app.lua:2:`. A nameless `Runtime` (the `Runtime::new()` test helper and any other nameless caller) falls back to the generic name `script` — **never** the Rust call-site location.
- **Exit codes are unchanged** (usage `2`, timeout `124`, OOM `137`, other errors `1`). This plan does not touch exit-code mapping.
- **Renderer is fail-safe:** when no location can be parsed (or the line is out of range), it falls back to a plain `lur: <message>` rather than guessing, and never panics.
- **Traceback default: kept, filtered.** Drop only the contentless frame line `[C]: in ?`; keep every other frame.
- No new dependencies.
- CI gates: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run`. Run `cargo fmt --all` before each commit.
- Commits GPG-signed (`git commit -S`). Stage files explicitly — never `git add -A`/`.`.

---

## File Structure

- **Modify** `src/runtime.rs` — add `chunk_name: Option<String>` to `RuntimeConfig` (+ `Default`); `Runtime` stores a resolved `chunk_name: String`; the three `run*` methods name the chunk.
- **Modify** `src/serve.rs` — `Server::load` names the chunk from `config.chunk_name`; `Server` stores `source`/`chunk_name` for handler/cron error rendering (Task 3).
- **Create** `src/diagnostics.rs` — the pure renderer.
- **Modify** `src/lib.rs` (or wherever modules are declared) — `pub mod diagnostics;`.
- **Modify** `src/main.rs` — set `chunk_name` from `cli.script`/`cli.app`; render the one-shot `Script` error via `diagnostics::render`.
- **Modify** `tests/` — chunk-naming integration tests; renderer unit tests live inline in `src/diagnostics.rs`.

---

### Task 1: Chunk naming (one-shot + server)

**Files:**
- Modify: `src/runtime.rs` (`RuntimeConfig` + `Default`, `Runtime` struct, `with_config`, `run`/`run_with_timeout`/`run_to_exit_code`)
- Modify: `src/serve.rs` (`Server::load` chunk naming)
- Modify: `src/main.rs` (`run_one_shot`, `run_server` set `chunk_name`)
- Test: `tests/diagnostics.rs` (new)

**Interfaces:**
- Consumes: existing `build_lua`, `Runtime`, `Server::load`.
- Produces: `RuntimeConfig.chunk_name: Option<String>`; named chunks everywhere. Tasks 2–3 rely on the chunk name reaching the error string as `<path>:<line>:`.

- [ ] **Step 1: Write the failing chunk-naming tests**

Create `tests/diagnostics.rs`:

```rust
use lur::runtime::{Runtime, RuntimeConfig};

/// A runtime error reports the configured chunk name, not lur's Rust source.
#[test]
fn named_runtime_reports_script_path_not_internals() {
    let cfg = RuntimeConfig {
        chunk_name: Some("app.lua".to_owned()),
        ..Default::default()
    };
    let rt = Runtime::with_config(cfg).expect("runtime builds");
    let err = rt
        .run("local x = nil\nprint(x.y)\n")
        .expect_err("script raises");
    let msg = err.to_string();
    assert!(msg.contains("app.lua:2"), "names the script line: {msg}");
    assert!(!msg.contains("src/runtime.rs"), "no internal path: {msg}");
}

/// A nameless runtime falls back to "script", never the Rust call site.
#[test]
fn nameless_runtime_falls_back_to_script() {
    let rt = Runtime::new().expect("runtime builds");
    let err = rt.run("error('boom')\n").expect_err("script raises");
    let msg = err.to_string();
    assert!(msg.contains("script:1"), "uses the generic name: {msg}");
    assert!(!msg.contains("src/runtime.rs"), "no internal path: {msg}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --test diagnostics`
Expected: FAIL — `RuntimeConfig` has no `chunk_name` field (compile error), and current output contains `src/runtime.rs`.

- [ ] **Step 3: Add `chunk_name` to `RuntimeConfig` and resolve it on `Runtime`**

In `src/runtime.rs`, add the field to the `RuntimeConfig` struct (near the other fields):

```rust
    /// Chunk name reported in error positions (`app.lua:2:`). Set from the CLI
    /// script/app path. `None` falls back to the generic name `script` — never
    /// the Rust call site. Uses Lua's file convention internally.
    pub chunk_name: Option<String>,
```

Add it to the `Default` impl (`src/runtime.rs:84`):

```rust
            chunk_name: None,
```

Add a stored field to the `Runtime` struct:

```rust
    /// Lua chunk name (already `@`-prefixed) applied to every loaded chunk.
    chunk_name: String,
```

In `with_config`, resolve and store it (replace the `Ok(Self { lua, deadline, rt })` line):

```rust
        let chunk_name = format!("@{}", config.chunk_name.as_deref().unwrap_or("script"));
        Ok(Self {
            lua,
            deadline,
            rt,
            chunk_name,
        })
```

- [ ] **Step 4: Name the chunk in the three `run*` methods**

In `src/runtime.rs`, update each loader to `.set_name(&self.chunk_name)`:

```rust
    pub fn run(&self, source: &str) -> Result<(), RunError> {
        self.guarded(None, self.lua.load(source).set_name(&self.chunk_name).exec_async())
    }

    pub fn run_with_timeout(&self, source: &str, timeout: Duration) -> Result<(), RunError> {
        self.guarded(
            Some(timeout),
            self.lua.load(source).set_name(&self.chunk_name).exec_async(),
        )
    }

    pub fn run_to_exit_code(
        &self,
        source: &str,
        timeout: Option<Duration>,
    ) -> Result<i32, RunError> {
        let values = self.guarded(
            timeout,
            self.lua
                .load(source)
                .set_name(&self.chunk_name)
                .eval_async::<MultiValue>(),
        )?;
        Ok(exit_code_of(values))
    }
```

- [ ] **Step 5: Name the chunk in the server loader**

In `src/serve.rs`, the pool builds each VM and loads `source` at the `lua.load(source).exec_async()` site (~line 229). Name it from `config.chunk_name`. Just before the pool loop builds VMs, compute once:

```rust
        let chunk_name = format!("@{}", config.chunk_name.as_deref().unwrap_or("script"));
```

and change the load to:

```rust
                lua.load(source)
                    .set_name(&chunk_name)
                    .exec_async()
                    .await
                    .map_err(RunError::Script)?;
```

(Use the codebase's surrounding style; `chunk_name` must be in scope where the pool loop runs — hoist its binding above the loop.)

- [ ] **Step 6: Set `chunk_name` from the CLI path in `main.rs`**

In `src/main.rs` `run_one_shot` (after `build_config` returns `config`), set the name before building the runtime:

```rust
    let mut config = config;
    config.chunk_name = Some(cli.script.display().to_string());
```

(If `config` is already bound with `let config = ...`, rebind as `let mut config` or set the field on the existing binding.)

In `run_server`, `config` is already `let mut config` (line ~262); set it before `Server::load`:

```rust
    config.chunk_name = Some(cli.app.display().to_string());
```

- [ ] **Step 7: Run the chunk-naming tests (expect PASS), full suite, lint**

Run: `cargo fmt --all && cargo nextest run --test diagnostics && cargo nextest run && cargo clippy --all-targets -- -D warnings`
Expected: both `diagnostics` tests PASS; full suite green; clippy clean.

Note: if the output shows `[string "app.lua"]:2` instead of `app.lua:2`, the `@` prefix is being double-handled — adjust the `format!("@{}", …)` (drop the `@`) so the rendered short-source is the bare path; the test asserts the rendered form, so let it drive this.

- [ ] **Step 8: Commit**

```bash
git add src/runtime.rs src/serve.rs src/main.rs tests/diagnostics.rs
git commit -S -m "feat(diagnostics): name script chunks from the CLI path"
```

---

### Task 2: rustc-style renderer module + one-shot wiring

**Files:**
- Create: `src/diagnostics.rs`
- Modify: `src/lib.rs` (declare `pub mod diagnostics;`)
- Modify: `src/main.rs` (`run_one_shot` renders the `Script` error)
- Test: inline `#[cfg(test)]` in `src/diagnostics.rs`; one integration assertion in `tests/diagnostics.rs`

**Interfaces:**
- Consumes: the named chunk from Task 1 (error strings read `<path>:<line>:`).
- Produces: `pub fn render(source: &str, chunk_name: &str, displayed: &str) -> String` — a rustc-style report, or a plain `lur: <message>` fallback. `chunk_name` is the bare path (no `@`) the caller knows; `displayed` is the mlua error's `Display`.

- [ ] **Step 1: Write the failing renderer unit tests**

Create `src/diagnostics.rs` with only the tests first (the function follows in Step 3):

```rust
//! Human-readable, rustc-style rendering of script errors against their source.

#[cfg(test)]
mod tests {
    use super::render;

    const SRC: &str = "local x = nil\nprint(x.y)\n";

    #[test]
    fn renders_runtime_error_with_snippet() {
        let displayed = "runtime error: app.lua:2: attempt to index nil with 'y'\n\
                         stack traceback:\n\tapp.lua:2: in main chunk\n\t[C]: in ?";
        let out = render(SRC, "app.lua", displayed);
        assert!(out.contains("error: attempt to index nil with 'y'"), "{out}");
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
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(diagnostics)'`
Expected: FAIL — `render` is not defined.

- [ ] **Step 3: Implement `render`**

Add to `src/diagnostics.rs` (above the test module):

```rust
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
    let caret_col = col.unwrap_or_else(|| {
        src_line.len() - src_line.trim_start().len() + 1
    });
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
    let after_line = &rest[line_end..];

    // optional :col
    let (col, after) = if let Some(tail) = after_line.strip_prefix(':') {
        let col_end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
        if col_end > 0 {
            (Some(tail[..col_end].parse().ok()?), &tail[col_end..])
        } else {
            (None, after_line)
        }
    } else {
        (None, after_line)
    };

    let message = after.strip_prefix(": ").or_else(|| after.strip_prefix(':'))?;
    Some((line, col, message.trim().to_string()))
}
```

- [ ] **Step 4: Run the renderer unit tests (expect PASS)**

Run: `cargo nextest run -E 'test(diagnostics)'`
Expected: the three `render*`/fallback unit tests PASS.

- [ ] **Step 5: Declare the module and wire it into one-shot output**

In `src/lib.rs`, add (with the other `pub mod` declarations):

```rust
pub mod diagnostics;
```

In `src/main.rs` `run_one_shot`, replace the `Script` arm body so it renders. The arm currently is `Err(RunError::Script(e)) => { eprintln!("{e}"); ExitCode::FAILURE }`. Change to:

```rust
        Err(RunError::Script(e)) => {
            let chunk = cli.script.display().to_string();
            eprintln!("{}", lur::diagnostics::render(&source, &chunk, &e.to_string()));
            ExitCode::FAILURE
        }
```

- [ ] **Step 6: Add the one-shot integration assertion**

Append to `tests/diagnostics.rs`:

```rust
/// End to end: the rendered one-shot output carries a snippet and the file line.
#[test]
fn rendered_runtime_error_has_snippet() {
    let cfg = RuntimeConfig {
        chunk_name: Some("app.lua".to_owned()),
        ..Default::default()
    };
    let rt = Runtime::with_config(cfg).expect("runtime builds");
    let err = rt
        .run("local x = nil\nprint(x.y)\n")
        .expect_err("script raises");
    let out = lur::diagnostics::render("local x = nil\nprint(x.y)\n", "app.lua", &err.to_string());
    assert!(out.contains("--> app.lua:2"), "{out}");
    assert!(out.contains("2 | print(x.y)"), "{out}");
}
```

- [ ] **Step 7: Run the full suite + lint**

Run: `cargo fmt --all && cargo nextest run && cargo clippy --all-targets -- -D warnings`
Expected: all green; clippy clean.

- [ ] **Step 8: Commit**

```bash
git add src/diagnostics.rs src/lib.rs src/main.rs tests/diagnostics.rs
git commit -S -m "feat(diagnostics): rustc-style error renderer in one-shot mode"
```

---

### Task 3: Server-side rendering + docs

**Files:**
- Modify: `src/serve.rs` (`Server` stores `source`/`chunk_name`; handler + cron error logs render)
- Modify: `README.md`, `ARCHITECTURE.md`
- Test: `tests/serve.rs` (or `tests/diagnostics.rs`) — handler-error log carries `app.lua:<line>`

**Interfaces:**
- Consumes: `diagnostics::render`; the named chunk from Task 1.
- Produces: server handler/cron errors rendered the same way as one-shot.

- [ ] **Step 1: Write the failing server test**

Append to `tests/serve.rs` (uses the existing `serve(src)` / `dispatch` helpers; the handler error is logged to stderr and also drives a 500 — assert the 500 here, and that dispatch surfaces the error to the caller path). Add:

```rust
#[test]
fn handler_error_is_rendered_against_app_source() {
    // Build a server whose handler raises; the dispatched response is a 500,
    // and the error rendered for the log names the app line.
    let s = serve(
        "lur.serve.http('GET', '/boom', function(req)\n\
         \tlocal x = nil\n\
         \treturn x.y\n\
         end)",
    );
    // The rendered diagnostic is produced from the server's stored source.
    let rendered = s.render_last_load_error_sample();
    // Sanity: the helper exists only to expose rendering in tests; see Step 2.
    let _ = rendered;
}
```

NOTE TO IMPLEMENTER: the above test is a placeholder shape. Prefer asserting the real behavior without adding production-only-for-test methods: instead, assert that a handler error still yields a 500 via the existing dispatch path AND unit-test the rendering separately (Task 2 already covers `render`). If exposing the server's stored `source`/`chunk_name` for a direct render assertion is cleaner, add a `#[cfg(test)]`-gated accessor rather than a public API. Choose the lower-footprint option and record which you chose in your report.

- [ ] **Step 2: Store `source` + `chunk_name` on `Server` and render the logs**

In `src/serve.rs`:

- Add fields to the `Server` struct:

```rust
    /// App source and chunk name, retained for rendering handler/cron errors.
    source: std::sync::Arc<str>,
    chunk_name: String,
```

- In `Server::load`, populate them (the `chunk_name` computed in Task 1 Step 5, and `source` from the `&str` argument): set `source: Arc::from(source)` and `chunk_name: config.chunk_name.clone().unwrap_or_else(|| "script".to_owned())` (store the BARE name here — no `@` — because `diagnostics::render` wants the bare path).

- At the handler-error log (~line 425), change:

```rust
                eprintln!("lur: handler error: {e}");
```

to:

```rust
                eprintln!(
                    "lur: handler error:\n{}",
                    crate::diagnostics::render(&self.source, &self.chunk_name, &e.to_string())
                );
```

- At the cron-error log (~line 484), the cron loop has access to the same stored fields via its `Server`/context; render likewise:

```rust
                eprintln!(
                    "error: cron[{}]:\n{}",
                    job.name,
                    crate::diagnostics::render(&source_for_cron, &chunk_for_cron, &e.to_string())
                );
```

(Use whatever `source`/`chunk_name` handle is in scope in the cron task; if the cron loop does not hold the `Server`, thread the `Arc<str>` source and chunk name into the cron task when it is spawned. Keep the change minimal; if cron does not have access without broader plumbing, render only the handler path in this task and note the cron path as a follow-up in your report.)

- [ ] **Step 3: Run the server tests + full suite + lint**

Run: `cargo fmt --all && cargo nextest run && cargo clippy --all-targets -- -D warnings`
Expected: green; clippy clean.

- [ ] **Step 4: Update the docs**

In `README.md`, add a short "Diagnostics" paragraph near the server/CLI docs:

```markdown
### Diagnostics

Errors are reported against your script's path with the failing line and a
source snippet (rustc-style), followed by a stack traceback. Server handler and
cron errors are rendered the same way to stderr (and still become a `500`).
```

In `ARCHITECTURE.md`, add a note in the error-handling/request-lifecycle area:

```markdown
Every `lua.load` is named from the CLI path (`cli.script`/`cli.app`; a nameless
runtime uses `script`), so error positions read `app.lua:2:` rather than the
Rust call site. `src/diagnostics.rs` renders an mlua error against the source
(rustc-style snippet + filtered traceback), with a plain `lur: <message>`
fallback when no location parses.
```

- [ ] **Step 5: Commit**

```bash
git add src/serve.rs README.md ARCHITECTURE.md tests/serve.rs
git commit -S -m "feat(diagnostics): render server handler/cron errors; document diagnostics"
```

---

## Self-Review

**Spec coverage (components 1–3):**
- Component 1 chunk naming (one-shot + server, CLI path, `script` fallback) → Task 1. ✓
- Component 2 host formatting (`lur:` framing, `[C]: in ?` filter, traceback kept) → folded into the renderer (Task 2 `render`) + applied in main/server (Tasks 2–3). ✓
- Component 3 rustc renderer (`src/diagnostics.rs`, prefix-anchored location parse, optional column, out-of-range/unparsable fallback, no panic) → Task 2. ✓
- Component 4 (per-capability messages) → intentionally NOT here; separate plan. ✓
- Exit codes untouched → no change to the exit-code arms. ✓

**Placeholder scan:** Task 3 Step 1 is explicitly flagged as a shape to refine (with a concrete instruction to prefer a `#[cfg(test)]` accessor or a 500-assertion over a production-only method) — not a silent TODO. All code steps carry complete code.

**Type consistency:** `RuntimeConfig.chunk_name: Option<String>`; `Runtime.chunk_name: String` (`@`-prefixed); `render(source: &str, chunk_name: &str, displayed: &str) -> String` with the BARE chunk name at call sites (main passes `cli.script` path; server stores the bare name). `parse_location` returns `(usize, Option<usize>, String)`, consumed by `render`. ✓

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks.
2. **Inline Execution** — execute in this session with checkpoints.
