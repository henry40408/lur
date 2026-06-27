# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

`lur` is a sandboxed Luau runtime in Rust: one binary, two modes (one-shot `lur script.lua`
and server `lur serve app.lua`) over a shared core. The user-facing docs in
[README.md](README.md) (CLI, sandbox model, full `lur.*` Lua API) and the internal design in
[ARCHITECTURE.md](ARCHITECTURE.md) (module map, lifecycles, invariants) are authoritative and
kept current — read them before non-trivial work rather than rediscovering from source.

## Commands

```sh
cargo nextest run                              # all tests (NOT cargo test)
cargo nextest run --test serve_http            # one integration test file (tests/serve_http.rs)
cargo nextest run -E 'test(routing)'           # tests matching a name (nextest filter expr)
cargo clippy --all-targets -- -D warnings      # lint gate
cargo fmt --all                                # required before committing
cargo bench --bench runtime                    # benchmarks (capture before/after for perf changes)
cargo deny check                               # advisories + license/ban/source gate
```

CI hard-gates on fmt, clippy (`-D warnings`), nextest, and `cargo-deny`. Coverage (Codecov) and
the benchmark report are informational and do not block.

## Architecture notes that constrain edits

These are the load-bearing invariants — violating them compiles but breaks the sandbox or the
pool. See ARCHITECTURE.md for the full reasoning.

- **`build_lua` order is fixed** (`src/runtime.rs`): strip dangerous globals (`require`,
  `getfenv`, `setfenv`, `loadstring`) → install capabilities → `sandbox(true)` (freezes globals)
  → install deadline interrupt → apply memory cap. Capabilities must be wired *before* the
  freeze; the memory cap goes last.
- **Capabilities are one flat `lur` table** built in a fixed order by
  `capabilities::install` (`src/capabilities/mod.rs`); each `src/capabilities/<name>.rs` owns its
  slice. Policy-gated modules (`fs`, `http`, `env`) take an `Arc<Policy>`. `serve`'s `Registry`
  is `None` in one-shot, so `lur.serve.*` raises there.
- **Two-layer timeout**: the deadline interrupt kills CPU-bound Lua; a `tokio::time::timeout`
  kills code parked on async I/O. Both are needed — applied in `Runtime::guarded` (one-shot) and
  `call_handler` (server).
- **Server pool + per-call isolation** (`src/serve.rs`): every pooled VM runs `app.lua` once to
  collect identical registrations (divergent registrations across VMs are rejected at load).
  Each request/cron run executes in a `fresh_env` whose writes are discarded — this, plus
  stripping `getfenv`/`setfenv`/`loadstring`, is what prevents cross-request global bleed. Don't
  reintroduce shared mutable global state on a pooled VM.
- **Security boundaries**: `lur.fs` canonicalizes paths before the allowlist check (defeats `..`
  / symlink escape); `lur.http` checks every redirect hop and runs an SSRF guard (private-IP DNS
  rejection unless `--allow-private`); `lur.env` returns `nil` for both denied and unset.
  Default profile is `strict` (deny-all).
- **Dynamic SQL** in `src/capabilities/db.rs` is wrapped in `sqlx::AssertSqlSafe` at the
  statement-building call sites; keep user-supplied values in `?` bind params.

## Conventions

- Edition 2024. MSRV (`rust-version`) and toolchain are managed separately — don't bump MSRV on
  a toolchain change.
- Tests: integration tests are one file per surface under `tests/`; unit tests live inline in
  each module.
- The spec referenced as "(spec §N)" lives at
  `docs/superpowers/specs/2026-06-26-lur-lua-runtime-design.md`.
