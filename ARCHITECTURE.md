# Architecture

This document is for developers working *on* `lur`. For how to *use* it, see the
[README](README.md). Section references like "(spec §3)" point at
[`docs/superpowers/specs/2026-06-26-lur-lua-runtime-design.md`](docs/superpowers/specs/2026-06-26-lur-lua-runtime-design.md).

## The shape of it

One binary, two execution modes, one shared core:

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

The crate root ([`src/lib.rs`](src/lib.rs)) exposes the library; `main.rs` is a thin
CLI on top. The public modules are `capabilities`, `config`, `policy`, `runtime`,
`serve`, and `units`.

## Module map

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI (clap), config/policy resolution, mode dispatch, exit codes. Not part of the library. |
| `src/runtime.rs` | The shared core: `build_lua`, the `Runtime` (one-shot), `RuntimeConfig`, `RunError`, the deadline/timeout machinery. |
| `src/serve.rs` | Server mode: the VM `Pool`, `Router`, request dispatch, cron schedulers, graceful shutdown. |
| `src/policy.rs` | `Policy` — the capability allow/deny model (`strict()` / `loose()`) and its checks. |
| `src/config.rs` | TOML config file parsing and the profile/allowlist model. |
| `src/units.rs` | `parse_size` (×1024) and `parse_duration` for CLI value parsers. |
| `src/capabilities/` | One submodule per `lur.*` table; `mod.rs::install` orchestrates them. |

## The shared core: `build_lua`

Both modes build their VM(s) through [`runtime::build_lua`](src/runtime.rs). The order
is load-bearing:

1. **Strip dangerous globals.** `require`, `getfenv`, `setfenv`, and `loadstring` survive
   Luau's `sandbox(true)` but each defeats a runtime guarantee — `require` reads `.luau`
   files off disk (bypassing the `lur.fs` capability), and the other three reach the
   *writable* global environment, which would let a value bleed across requests sharing a
   pooled VM. They are set to `nil` before anything else.
2. **Install capabilities.** `capabilities::install` builds the single `lur` table and
   populates it (see below). This must happen *before* the freeze.
3. **`sandbox(true)`.** Freezes the global table (so `rawset` and friends can't reopen it)
   and disables untrusted hooks. Standard Luau already lacks `os.execute`, `io`,
   `loadfile`/`dofile`, and `package`.
4. **Install the deadline interrupt.** A shared `Deadline = Arc<Mutex<Option<Instant>>>`
   is read by an `mlua` interrupt hook. Past the deadline it raises *on every interrupt*,
   so a CPU-bound `pcall` loop cannot swallow the error and outlive its budget.
5. **Apply the memory cap last**, after construction/injection have done their own
   allocations.

### The two-layer timeout

A single deadline can't cover both failure modes, so there are two layers (spec §5):

- The **deadline interrupt** aborts CPU-bound Lua (tight loops, busy work).
- A **`tokio::time::timeout`** wraps the driving future to kill code parked on async I/O,
  where the interrupt hook never fires.

One-shot applies both in `Runtime::guarded`; server mode applies both in
`call_handler`. After the await, the error is classified: out-of-memory →
`RunError::OutOfMemory`; past-deadline → `Timeout`; otherwise `Script`.

## Capability layer

[`capabilities::install`](src/capabilities/mod.rs) creates the one flat `lur` table and
hands it to each submodule's `install` in a fixed order:

```
null · log · json · base64 · crypto · cookie · io · fs · http · env · db · async · args · serve · state
```

Each submodule sets its own slice (`lur.fs`, `lur.http`, …). Policy-gated modules
(`fs`, `http`, `env`) receive an `Arc<Policy>`; storage modules receive the `--db` path;
`async` receives the concurrency cap; `serve` receives a `Registry` only under
`lur serve` (it is `None` in one-shot, which makes `lur.serve.*` raise a clear error).
Everything is wired before `sandbox(true)` freezes it.

### Policy enforcement

[`Policy`](src/policy.rs) is the deny-by-default model shared into host callbacks behind
an `Arc`. `strict()` grants nothing; `loose()` grants everything. Enforcement lives at
each capability's boundary:

- **`lur.fs`** canonicalizes the path *before* the allowlist check, so `..` and symlink
  escapes resolve to their real target first.
- **`lur.http`** checks every request and every redirect hop against the network
  allowlist, runs a DNS resolver that rejects loopback/private/link-local addresses
  unless `--allow-private` (SSRF guard), caps redirects, caps the buffered body
  (`--max-http-body`), and always verifies TLS.
- **`lur.env`** returns `nil` for both "denied" and "unset", so it can't be used as an
  oracle for which variables exist.

## One-shot mode

[`Runtime`](src/runtime.rs) owns a single VM and a **current-thread** tokio runtime that
drives async host calls. `main.rs::run_one_shot` reads the script, builds a
`RuntimeConfig`, and calls `run_to_exit_code`, which evaluates the chunk and maps its
top-level `return` to a process exit code (spec §8): a number → that code, `nil`/`false`
→ 1, anything else (or no return) → 0.

## Server mode

[`Server::load`](src/serve.rs) builds a **multi-threaded** runtime and a pool of
`pool_size` pre-warmed VMs. Each VM runs `app.lua` once — not to serve traffic, but to
*collect its registrations*: `lur.serve.http`/`lur.serve.cron` push into a per-VM
`Registry`. The handler closures live inside each VM, indexed by a host-assigned id; the
host keeps only the route/cron *metadata*. Because every VM runs the same script, they
must register the same routes and jobs in the same order — `load` checks this and rejects
an app whose registrations diverge across VMs (the ids would not line up).

### The VM pool

```
Pool { available: Mutex<Vec<Vm>>, permits: Semaphore }
```

A request `checkout()`s a VM: acquire a permit (parking if all VMs are busy), then pop a
VM. The returned `CheckedOut` guard pushes the VM back into `available` on `Drop`, *then*
releases the permit — so a waiter woken by the permit is guaranteed to find a VM waiting.
Exclusive ownership for the whole call is what makes the per-call environment swap safe;
it replaces what would otherwise be a single-VM serialize lock. The pool size therefore
caps concurrent in-flight handlers.

### Routing

[`Router`](src/serve.rs) compiles `(method, path)` registrations into routes. A path is
parsed into segments where `:name` becomes a `Param` and everything else is `Static`.
`resolve` walks all routes and picks the **most specific** match independent of
registration order: a static segment beats a param at the same position, and a concrete
method beats `ANY` as a tiebreak. Duplicate `(method, signature)` pairs are rejected at
load time. Matched params are percent-decoded to raw bytes and exposed as `req.params`.

### Request lifecycle

`handle` (the hyper adapter) → `dispatch_async`:

1. Reject an oversize body with **413** at the host edge, before routing — the VM never
   allocates it.
2. `router.resolve` → **404** if nothing matches.
3. `checkout()` a VM, `build_req` (sets `method`, `path`, `params`, `query`/`query_all`,
   `headers`, `cookies`, `body`, the streaming `read`, and `json()`), then `call_handler` under the
   two-layer timeout.
4. Map the result: a returned table → `response_from` (reads `status`, default 200, and
   `body`, default empty); timeout → **503**; a Lua error → logged and **500**. A handler
   error never brings the server down (spec §8).

The request body is a one-shot cursor (`BodyStream`): `req.read(n)` streams it in chunks,
and once streaming starts `req.body`/`req.json()` refuse to serve a now-partial body.

### Per-call isolation

`fresh_env` builds a throwaway table whose metatable `__index` points at the frozen
globals. Each handler/cron run is given this as its environment, so reads fall through to
the real globals while writes land in the throwaway and are discarded when the call ends.
This — together with stripping `getfenv`/`setfenv`/`loadstring` in `build_lua` — closes
the cross-request global-bleed vector on a pooled VM (spec §3, §5.1).

### Cron

Each job gets its own `cron_loop` task that computes the next future fire from a 6-field
schedule, sleeps until then (or until shutdown), and runs the handler on a pooled VM.
It is **single-flight** by default: an `AtomicBool` skips a tick whose predecessor is
still running (set `overlap = true` to allow concurrency). Missed ticks are **never
replayed** (fire-forward). Errors and timeouts are logged with the job name, never
propagated.

### Graceful shutdown

`run_with_shutdown` fans a single shutdown future (SIGTERM/SIGINT, or any future for
tests) out to the accept loop and every cron scheduler through a `tokio::sync::watch`
channel. An `Arc<()>` liveness token is cloned into every in-flight connection and cron
run; on shutdown the accept loop stops, then the drain loop waits until only the original
`Arc` handle remains — bounded by `--shutdown-grace`. Anything still running past the
grace period is aborted when the runtime drops.

## State & storage

- **`lur.db` / `lur.kv`** ([`capabilities/db.rs`](src/capabilities/db.rs)) sit on a
  lazily-opened `sqlx` SQLite pool (WAL mode, file auto-created). The pool opens on first
  use and is shared by both modules; `kv` is a thin layer over an internal
  `lur_kv(key, value)` table. Dynamic SQL is wrapped in `sqlx::AssertSqlSafe` at the four
  call sites that build statements from user input.
- **`lur.state`** ([`capabilities/state.rs`](src/capabilities/state.rs)) is a host-side,
  process-wide `StateStore` shared by every VM in the pool (via `RuntimeConfig::state`),
  holding **primitives only**. Every key is version-stamped (bumped on every write,
  including deletes, to avoid ABA). `update` is an optimistic CAS loop whose user function
  runs with **no host lock held**, so it scales across the pool; a conflicting write
  triggers a retry.

## Async core

[`capabilities/async_ops.rs`](src/capabilities/async_ops.rs) exposes `lur.async.sleep`
and the combinators (`all`, `race`, `any`, `settled`) over arrays of zero-arg Lua
functions. An optional `Arc<Semaphore>` (`--max-concurrency`) caps in-flight tasks: each
task acquires an owned permit before it runs. Lua itself still executes one step at a
time; tasks interleave only at I/O await points, and early settlement (`race`/`any`)
drops — and thereby cancels — the remaining futures.

## Configuration resolution

CLI flags are parsed by clap with `units::parse_size`/`parse_duration` value parsers.
`main.rs::load_config` finds the TOML config (`--config`, else
`$XDG_CONFIG_HOME/lur/config` → `~/.config/lur/config`, unless `--no-config`), and
`build_policy` merges it with the flags: the **profile** is last-wins (a flag overrides
`default_profile`) while **allowlists** are additive (config grants unioned with per-run
flags). Mode selection in `main` is a literal peek at `argv[1] == "serve"`.

## Tests & CI

Integration tests live under `tests/` (one file per surface: `cli.rs`, `serve.rs`,
`serve_http.rs`, `runtime.rs`, …) alongside unit tests inside each module. Run them with
`cargo nextest run`. Benchmarks are in `benches/` (`cargo bench --bench runtime`). CI
([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs fmt + clippy (`-D
warnings`), nextest, coverage (`cargo-llvm-cov` → Codecov), and an informational
benchmark report; every action is pinned to a commit SHA.
