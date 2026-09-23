# Architecture

For developers working *on* `lur`; for usage see the [README](README.md). "(spec §N)" refers
to [`docs/superpowers/specs/2026-06-26-lur-lua-runtime-design.md`](docs/superpowers/specs/2026-06-26-lur-lua-runtime-design.md).

## Overview

One binary, two modes, one shared core:

```
                 ┌──────────────────────── src/main.rs ────────────────────────┐
                 │  CLI parse (clap) · config + policy resolution · mode select │
                 └───────────────┬─────────────────────────┬────────────────────┘
                       one-shot   │                         │  serve
                                  ▼                         ▼
                       runtime::Runtime            serve::Server
                       (1 VM, current-thread rt)   (VM pool, multi-thread rt,
                                  │                  router, cron schedulers)
                                  └───────────┬───────────┘
                                              ▼
                        runtime::build_lua  — the shared core
              sandboxed Luau VM · lur.* capabilities · deadline interrupt · memory cap
```

`src/lib.rs` is the library; `main.rs` is a thin CLI on top.

## Module map

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI, config/policy resolution, mode dispatch, exit codes. Not in the library. |
| `src/runtime.rs` | `build_lua`, one-shot `Runtime`, `RuntimeConfig`, `RunError`, timeout machinery. |
| `src/serve.rs` | Server mode: VM `Pool`, `Router`, dispatch, cron, graceful shutdown. |
| `src/color.rs` | `NO_COLOR`/TTY gate (`color_from_env`, `stderr_color`, `stdout_color`). |
| `src/diagnostics.rs` | `render`: rustc-style error snippet + filtered traceback, `lur: <msg>` fallback. |
| `src/docs.rs` | `render` for `lur docs` (embedded `docs/GUIDE.md`): pulldown-cmark → ANSI or plain text. |
| `src/policy.rs` | `Policy`: allow/deny model (`strict()` / `loose()`) and its checks. |
| `src/config.rs` | TOML config parsing; profile/allowlist model. |
| `src/units.rs` | `parse_size` (binary, ×1024) and `parse_duration` for clap. |
| `src/capabilities/` | One submodule per `lur.*` table; `mod.rs::install` orchestrates. |
| `src/capabilities/storage/` | Backend seam: `Backend`/`Transaction` enums (`Sqlite`/`Postgres`), lazy `Shared` handle, `ExecResult`; `sqlite.rs` and `postgres.rs` own all backend-specific SQL, binding, row mapping, transactions, and kv. |
| `src/capabilities/db.rs` | `lur.db` (`exec`/`query`/`tx`) over `storage::Backend`; hands `storage::Shared` to `kv`. |
| `src/capabilities/kv.rs` | `lur.kv` over the backend's kv methods; owns the `IN_KV_UPDATE` reentrancy guard. |

## Shared core: `build_lua`

Both modes build VMs via [`runtime::build_lua`](src/runtime.rs). Order is load-bearing:

1. **Strip `require`, `getfenv`, `setfenv`, `loadstring`.** They survive `sandbox(true)`:
   `require` reads `.luau` files off disk (bypassing `lur.fs`); the other three reach the
   writable global env, letting values bleed across requests on a pooled VM.
2. **`capabilities::install`** builds the `lur` table — must precede the freeze.
3. **`sandbox(true)`** freezes globals (`rawset` can't reopen them). Luau already lacks
   `os.execute`, `io`, `loadfile`/`dofile`, `package`.
4. **Deadline interrupt.** Reads a shared `Deadline = Arc<Mutex<Option<Instant>>>`; past the
   deadline it raises on *every* interrupt, so a `pcall` loop can't swallow it.
5. **Memory cap last**, so construction allocations don't count against it.

### Two-layer timeout (spec §5)

- The **deadline interrupt** aborts CPU-bound Lua.
- **`tokio::time::timeout`** kills code parked on async I/O, where the interrupt never fires.

Applied in `Runtime::guarded` (one-shot) and `call_handler` (server). Errors are classified as
out-of-memory → `RunError::OutOfMemory`, past-deadline → `Timeout`, else `Script`.

## Capability layer

[`capabilities::install`](src/capabilities/mod.rs) fills the flat `lur` table in fixed order:

```
null · log · json · base64 · crypto · cookie · time · io · fs · http · env · db · kv · async · args · serve · state
```

`fs`/`http`/`env` get an `Arc<Policy>`; `db` gets the `--db` target and passes the shared
handle to `kv`; `async` gets the concurrency cap; `serve` gets a `Registry` only under
`lur serve` (`None` in one-shot, so `lur.serve.*` raises).

Scalar arguments go through `argcheck::arg` (keeps mlua coercion, raises
`lur.<cap>.<fn>: argument #<n> must be <type>, got <type>`) and `argcheck::integer_arg`
(rejects fractional numbers). Table/closure-taking APIs (`http`, `serve`, `db`, `async`)
validate their own arguments.

### Policy enforcement

[`Policy`](src/policy.rs) is deny-by-default, shared into callbacks via `Arc`. `strict()`
grants nothing; `loose()` grants everything. Enforced at each capability:

- **`lur.fs`** canonicalizes before the allowlist check, defeating `..` and symlink escapes.
- **`lur.http`** checks every request and redirect hop against the net allowlist, uses a DNS
  resolver rejecting loopback/private/link-local IPs unless `--allow-private` (SSRF guard),
  caps redirects (10) and the buffered body (`--max-http-body`), and always verifies TLS.
- **`lur.env`** returns `nil` for both denied and unset, so it isn't an existence oracle.

## One-shot mode

[`Runtime`](src/runtime.rs) owns one VM and a current-thread tokio runtime.
`main.rs::run_one_shot` builds a `RuntimeConfig` and calls `run_to_exit_code`, which maps the
chunk's top-level `return` to an exit code (spec §8): number → that code, `nil`/`false` → 1,
anything else or no return → 0.

## Server mode

[`Server::load`](src/serve.rs) builds a multi-thread runtime and `pool_size` pre-warmed VMs.
Each VM runs `app.lua` once to *collect registrations* (`lur.serve.http`/`cron` push into a
per-VM `Registry`). Handler closures stay in each VM, indexed by id; the host keeps only
metadata. All VMs must register identical routes and jobs in the same order, or `load` rejects
the app (ids wouldn't line up).

### VM pool

```
Pool { available: Mutex<Vec<Vm>>, permits: Semaphore }
```

`checkout()` acquires a permit, then pops a VM. The `CheckedOut` guard pushes the VM back on
`Drop` *before* releasing the permit, so a woken waiter always finds a VM. Exclusive ownership
per call is what makes the per-call env swap safe; pool size caps concurrent handlers.

### Routing

[`Router`](src/serve.rs) parses paths into `Static` / `Param` (`:name`) segments. `resolve`
picks the most specific match regardless of registration order: static beats param at the
same position; a concrete method beats `ANY` as tiebreak. Duplicate `(method, signature)` is
rejected at load. Params are percent-decoded to raw bytes as `req.params`.

### Request lifecycle

`handle` (hyper adapter) → `dispatch_async`:

1. Body over `--max-body` → **413** before routing; the VM never sees it.
2. No route → **404**.
3. `checkout()`, `build_req` (`method`, `path`, `params`, `query`/`query_all`, `headers`,
   `cookies`, `body`, streaming `read`, `json()`), then `call_handler` under the two-layer
   timeout.
4. Returned table → `response_from` (`status` default 200, must be 100–599; `headers`
   expanded to validated pairs, CR/LF and framing headers rejected; `body` default
   empty); timeout → **503**; Lua error or bad return → logged, **500**. Handler errors never
   bring the server down (spec §8).

Chunks are named from the CLI path (`script` if unnamed), so errors read `app.lua:2:`.
Handler and cron errors go through the same `diagnostics::render` as one-shot.

The body is a one-shot cursor (`BodyStream`): once `req.read(n)` streams it, `req.body` and
`req.json()` raise instead of returning a partial body.

### Per-call isolation

`fresh_env` makes a throwaway table whose `__index` is the frozen globals and sets it as the
handler/cron env: reads fall through, writes are discarded after the call. Together with
stripping `getfenv`/`setfenv`/`loadstring`, this prevents cross-request global bleed
(spec §3, §5.1).

### Cron

Each job runs a `cron_loop`: compute the next fire from the 6-field spec, sleep (or stop on
shutdown), run on a pooled VM. Single-flight by default (an `AtomicBool` skips a tick while the
previous run is in flight; `overlap = true` allows concurrency). Missed ticks are never
replayed. A per-job `timeout` overrides the per-event timeout. Errors and timeouts are logged
with the job name, never propagated.

### Graceful shutdown

`run_with_shutdown` fans one shutdown future (SIGTERM/SIGINT, or any future in tests) out to
the accept loop and cron loops via a `watch` channel. Each in-flight connection and cron run
holds a clone of an `Arc<()>` token; after accept stops, draining waits until only the
original remains, bounded by `--shutdown-grace`. Stragglers are aborted when the runtime drops.

## State & storage

- **Seam.** `db.rs`/`kv.rs` call the backend-neutral `Backend` enum;
  `StorageTarget::resolve` picks the backend from `--db` (`postgres://`/`postgresql://` →
  Postgres, else a SQLite path) and `Shared::ensure` opens it on first use. The kv value
  model (opaque bytes vs. integer counter) is defined at the seam.
- **SQL safety.** Dynamic SQL is wrapped in `sqlx::AssertSqlSafe` at the statement-building
  sites in `storage/sqlite.rs` and `storage/postgres.rs`; user values stay in bind params.
- **`lur.kv`.** `add`/`cas`/`incr`/`decr` are single statements; `update` (read-modify-write)
  uses the backend's `kv_update` transaction. `get` always returns bytes (counters as
  decimal strings).
- **Invariants:** kv counters are integers; `kv.get` returns bytes; `db.tx`/`kv.update` are
  write transactions; integer steps (`kv.incr`/`decr`, `state.incr`/`decr`) reject fractions.

### SQLite (`storage/sqlite.rs`)

- `SqliteBackend` owns a lazily opened `sqlx` pool (WAL, file auto-created) and
  `lur_kv(key TEXT PRIMARY KEY, value BLOB)` — counters are stored as SQLite integers.
- Write transactions (`db.tx`, `kv.update`) use `BEGIN IMMEDIATE`.
- Lock contention has two complementary layers: a 5 s `busy_timeout` waits out ordinary
  write-lock contention; `retry_busy` (5 attempts, full-jitter backoff) covers locks the busy
  handler can't wait on — the WAL pragma on a fresh connection and fail-fast lock upgrades.
  It wraps single-statement writes (`db.exec`, `kv.add`/`cas`/`incr`/`decr`), `begin`, and
  `open_pool` (connect + `lur_kv` DDL), i.e. only where no user code has run, so a retry never
  duplicates a side effect.

### Postgres (`storage/postgres.rs`)

- `PgBackend` owns the `PgPool`, `$n` binding, row→Lua mapping (core scalar types only; other
  columns raise a cast-to-text error), and `lur_kv(key TEXT PRIMARY KEY, kind SMALLINT,
  bytes BYTEA, num BIGINT)` — `kind = 0` opaque bytes, `kind = 1` integer counter in `num`.
- **Isolation:** single statements run at `READ COMMITTED` (atomic, no retry). `db.tx` and
  `kv.update` use `SERIALIZABLE` on a pinned connection; conflicts abort with SQLSTATE
  `40001`, surfaced with a stable, locale-independent message (`map_pg_error`) rather than
  retried, since the body may have had side effects. The abort hits an in-transaction
  statement about as often as `COMMIT` (~50/50) — don't assume commit-only. No
  `retry_busy`/`busy_timeout` layer.
- **Trusted URL:** the `--db` URL comes from the operator, not scripts, so it's exempt from
  the net allowlist and SSRF guard (as the SQLite path is exempt from `lur.fs`).

### Cancellation-safe transactions

`db.tx`/`kv.update` run user code inside a manually opened transaction, which `sqlx` doesn't
auto-roll-back. If the wall-clock timeout drops the future mid-body, the guard
(`SqliteTransaction`/`PgTransaction`, or `PinnedTx` in `kv_update`) rolls back on `Drop` via a
detached task, so the connection never returns to the pool mid-transaction (on Postgres it
would sit idle-in-transaction holding locks). Explicit COMMIT/ROLLBACK disarms the guard.

### `lur.state`

[`capabilities/state.rs`](src/capabilities/state.rs): a process-wide host-side `StateStore`
shared by all pooled VMs (via `RuntimeConfig::state`), **primitives only**. Each key carries a
version bumped on every write, including deletes (prevents ABA). `update` is an optimistic
CAS loop whose user function runs with no host lock held; conflicts retry.

## Async core

[`capabilities/async_ops.rs`](src/capabilities/async_ops.rs): `lur.async.sleep` and
combinators `all`/`race`/`any`/`settled` over arrays of zero-arg functions. `--max-concurrency`
is an `Arc<Semaphore>`; each task takes an owned permit before running. Lua still runs one step
at a time — tasks interleave only at I/O awaits; `race`/`any` drop (cancel) the remaining
futures on settlement.

## Configuration resolution

`main` peeks `argv[1]`: `serve` → server mode, `docs` → print the guide, else one-shot.
`load_config` finds the TOML config (`--config`, else `$XDG_CONFIG_HOME/lur/config` →
`~/.config/lur/config`; skipped with `--no-config`). `build_policy` picks the profile by
precedence `-A`/`--loose` > `--strict` > config `default_profile` > strict; under strict,
allowlists are the union of config and flag grants.

## Tests & CI

Integration tests: one file per surface in `tests/`; unit tests inline. `tests/pg.rs` needs
Postgres (`LUR_TEST_PG_URL`, default matches `docker compose up -d`): skipped locally if
unreachable, a hard failure when `CI` is set. Benchmarks: `benches/runtime.rs`.

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)): fmt, clippy (`-D warnings`),
nextest (with a Postgres service), `cargo deny check`, plus informational coverage
(`cargo-llvm-cov` → Codecov) and benchmark report. All actions are pinned to commit SHAs.
