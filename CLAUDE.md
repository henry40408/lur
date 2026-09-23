# CLAUDE.md

`lur` is a sandboxed Luau runtime in Rust: one binary, one-shot (`lur script.lua`) and server
(`lur serve app.lua`) modes over a shared core. [README.md](README.md) (CLI, sandbox, `lur.*`
API) and [ARCHITECTURE.md](ARCHITECTURE.md) (modules, lifecycles, invariants) are authoritative
— read them before non-trivial work.

## Commands

```sh
cargo nextest run                              # all tests (NOT cargo test)
cargo nextest run --test serve_http            # one integration test file
cargo nextest run -E 'test(routing)'           # tests matching a name
cargo clippy --all-targets -- -D warnings      # lint gate
cargo fmt --all                                # required before committing
cargo bench --bench runtime                    # before/after for perf changes
cargo deny check                               # advisories/licenses/bans/sources
docker compose up -d                           # Postgres for tests/pg.rs (skipped locally if down)
```

CI blocks on fmt, clippy, nextest, and `cargo deny`; coverage and benchmarks are informational.

## Invariants (see ARCHITECTURE.md for why)

Violating these compiles but breaks the sandbox or the pool.

- **`build_lua` order** (`src/runtime.rs`): strip `require`/`getfenv`/`setfenv`/`loadstring`
  → `capabilities::install` → `sandbox(true)` → deadline interrupt → memory cap.
- **One flat `lur` table**, filled in fixed order by `capabilities::install`
  (`src/capabilities/mod.rs`); each `src/capabilities/<name>.rs` owns its slice. `fs`/`http`/
  `env` take `Arc<Policy>`; `serve`'s `Registry` is `None` in one-shot.
- **Two-layer timeout**: deadline interrupt (CPU-bound) + `tokio::time::timeout` (async I/O),
  in `Runtime::guarded` and `call_handler`.
- **Pool isolation** (`src/serve.rs`): each request/cron run executes in a `fresh_env` whose
  writes are discarded. Never add shared mutable global state on a pooled VM.
- **Security**: `lur.fs` canonicalizes before the allowlist check; `lur.http` checks every
  redirect hop and blocks private IPs unless `--allow-private`; `lur.env` returns `nil` for
  denied and unset alike. Default profile is `strict`.
- **Dynamic SQL** is wrapped in `sqlx::AssertSqlSafe` in `src/capabilities/storage/{sqlite,postgres}.rs`;
  user values go in bind params (`?` / `$n`), never into the SQL string.

## Conventions

- Edition 2024; toolchain pinned in `rust-toolchain.toml`. No `rust-version` is declared —
  don't add one on a toolchain bump.
- Integration tests: one file per surface under `tests/`; unit tests inline.
- "(spec §N)" → `docs/superpowers/specs/2026-06-26-lur-lua-runtime-design.md`.
