# Architecture

For developers working *on* `lur`; for usage see the [README](README.md).
Design rationale and known limitations sit next to what they explain: in the sections below
and in the `//!` docs of the `src/` modules. User-facing limits are in the README.

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

### Why Luau

- **Luau, not PUC Lua 5.4.** Untrusted scripts are in the threat model, and Luau's
  `sandbox(true)` (readonly globals, dangerous stdlib removed) is hardened at Roblox scale;
  a hand-rolled 5.4 sandbox would make every bug a lur CVE. Costs: Lua 5.1 dialect (no
  integer subtype, `goto`, `<close>`), smaller ecosystem. Revisit only if scripts become
  semi-trusted-only *and* 5.4 semantics become a hard requirement.

## Module map

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI, config/policy resolution, mode dispatch, exit codes. Not in the library. |
| `src/runtime.rs` | `build_lua`, one-shot `Runtime`, `RuntimeConfig`, `RunError`, timeout machinery. |
| `src/serve.rs` | Server mode: VM `Pool`, `Router`, dispatch, cron, graceful shutdown. |
| `src/color.rs` | `NO_COLOR`/TTY gate (`color_from_env`, `stderr_color`, `stdout_color`). |
| `src/diagnostics.rs` | `render`: rustc-style error snippet + filtered traceback, `lur: <msg>` fallback. |
| `src/docs.rs` | `render` for `lur docs` (embedded `docs/GUIDE.md`): pulldown-cmark → ANSI or plain text; `render_width` wraps table cells to a width from `terminal_width` (`COLUMNS` → `TIOCGWINSZ` → 80, no wrap when stdout is not a TTY), cell width via `unicode-width`. |
| `src/policy.rs` | `Policy`: allow/deny model (`strict()` / `loose()`) and its checks. |
| `src/config.rs` | TOML config parsing; profile/allowlist model. |
| `src/units.rs` | `parse_size` (binary, ×1024) and `parse_duration` for clap. |
| `src/capabilities/` | One submodule per `lur.*` table; `mod.rs::install` orchestrates. |
| `src/capabilities/storage/` | Backend seam: `Backend`/`Transaction` enums (`Sqlite`/`Postgres`), lazy `Shared` handle, `ExecResult`; `sqlite.rs` and `postgres.rs` own all backend-specific SQL, binding, row mapping, transactions, and kv. |
| `src/capabilities/db.rs` | `lur.db` (`exec`/`query`/`tx`) over `storage::Backend`; hands `storage::Shared` to `kv`. |
| `src/capabilities/validate.rs` | `lur.validate`: schema nodes (opaque userdata), `parse`/`try`, issue collection; pure compute. |
| `src/capabilities/raise.rs` | The one way to raise a *table* as a Lua error (`raising`, or `raising_hot` + `Sentinel` on hot paths), give it a locked `__tostring` metatable (`error_meta`), and recover it after a `lur.async` task flattened it (`recover`). |
| `src/capabilities/kv.rs` | `lur.kv` over the backend's kv methods; owns the `IN_KV_UPDATE` reentrancy guard. |
| `src/capabilities/state/resume.rs` | `coroutine.resume`/`wrap` re-done as Luau C functions that count resume depth for `fetch` single-flight; the crate's only `unsafe`. |

## Shared core: `build_lua`

Both modes build VMs via [`runtime::build_lua`](src/runtime.rs). Order is load-bearing:

1. **Strip `require`, `getfenv`, `setfenv`, `loadstring`.** They survive `sandbox(true)`:
   `require` reads `.luau` files off disk (bypassing `lur.fs`); the other three reach the
   writable global env, letting values bleed across requests on a pooled VM.
2. **`capabilities::install`** builds the `lur` table — must precede the freeze — and, as its
   last step, marks every capability table read-only (nested ones, e.g. `lur.crypto.hex`, are
   frozen by their own capability). `luaL_sandbox` only freezes one level, so without this
   `lur.json.encode = ...` would succeed and persist on a pooled VM. Capabilities must not
   keep mutable state in a lur-visible table; per-request results are created later and stay
   mutable. `every_lur_table_is_deeply_frozen` walks the tree so a new unfrozen table fails CI.
   `lur.state`'s install also replaces `coroutine.resume` and `coroutine.wrap` (see
   [`lur.state`](#lurstate)), which only works before the freeze.
3. **`sandbox(true)`** freezes globals (`rawset` can't reopen them). Luau already lacks
   `os.execute`, `io`, `loadfile`/`dofile`, `package`.
4. **Deadline interrupt.** Reads a shared `Deadline = Arc<Mutex<Option<Instant>>>`; past the
   deadline it raises on *every* interrupt, so a `pcall` loop can't swallow it.
5. **Memory cap last**, so construction allocations don't count against it.

### Sandbox rationale

- **Only `require` is a real escape** among the four stripped globals (it loads `.luau`
  off disk). `loadstring`/`getfenv`/`setfenv` can't reach `io`/`os`; they're stripped for
  least surface and to stop global bleed on pooled VMs. A policy-gated `require` via
  mlua's `create_require_function` is the natural future replacement for dropping it.
- **Every table under `lur` is frozen, not just `lur`.** `luaL_sandbox` only makes the first
  level read-only, so `lur.json.encode = f` used to succeed and persist on a pooled VM,
  letting one request tamper with later ones (e.g. forge `lur.validate.try`).
  `capabilities::install` now freezes each capability table, and the nested ones
  (`lur.crypto.hex`, `lur.args.flags`/`positional`) freeze themselves. A generic recursive
  walk was tried first (mlua `pairs` Rust walk: +9% `vm_cold_start`, from value-converting iteration, since every
  function value becomes a registry ref; Luau walk: +24%, chunk compile). The explicit
  freeze is ~+2%. A raw `lua_next` walk would cost <1% but needs `unsafe`, which the crate
  denies. A one-shot test walks the tree to catch omitted nested tables; tables returned by
  functions are covered only by the review rule in ARCHITECTURE. Nothing had to move out of lur-visible
  tables: no capability wrote into one after install (state lives in Rust or closures).
  Behaviour change: scripts can no longer monkeypatch or extend `lur.*`; use a local table.

### Two-layer timeout

- The **deadline interrupt** aborts CPU-bound Lua.
- **`tokio::time::timeout`** kills code parked on async I/O, where the interrupt never fires.

Applied in `Runtime::guarded` (one-shot) and `call_handler` (server), both through
`runtime::with_timeout` (which the app.lua top level also goes through, without a timeout,
for its end-of-run cleanup). Errors are classified as out-of-memory → `RunError::OutOfMemory`,
past-deadline → `Timeout`, else `Script`.

Cancelling a Lua call must run its host futures' drop guards *now* (a `fetch` leader hands off
in its `Drop`; pooled VMs are reused, not dropped). mlua runs every async host call from a
small Lua poll loop (`=__mlua_async_poll`); if that loop raises, the Lua thread dies with the
pending futures still on its stack and only a GC frees them. Three rules keep that from
happening:

- The interrupt never raises inside the poll loop (`in_async_poll`); past the deadline it
  raises in the user code the loop returns to, and parked code is left to the wall clock.
- `with_timeout` clears the deadline before dropping the call (on expiry or when its own
  future is dropped): mlua cancels a parked call by resuming it once with a terminate
  signal, which the interrupt would otherwise abort.
- After a run that timed out or ran out of memory, `runtime::reclaim` does a full GC as a
  backstop (the memory cap can also fail inside the poll loop). Only those endings pay for it.

#### Rationale

- **The deadline interrupt must keep raising.** Its error is an ordinary catchable Lua
  error; a fire-once abort would be swallowed by `while true do pcall(...) end`. One
  exception: it never raises inside mlua's own async poll loop (see "The deadline interrupt
  never raises inside mlua's async poll loop" below). It still raises in every user frame.

- **The deadline interrupt never raises inside mlua's async poll loop.** Found while adding
  `fetch` single-flight: a run timed out on a reused VM left its leader's flight in place,
  so later callers of that key waited until their own timeouts. mlua drives each async host
  call from a small Lua loop (`=__mlua_async_poll`); the tokio timer and the deadline expire
  together, the timer wakes the loop first, the interrupt raised in it, and the thread died
  with the pending host futures on its stack: nothing dropped them until a GC, which an idle
  pooled VM may not run for a long time. The interrupt now skips that frame (CPU-bound code
  is still stopped in its own frames; parked code by the wall clock), `with_timeout` clears
  the deadline before dropping the call so mlua's terminate resume is not aborted, and a
  timed-out or OOM run ends with a full GC as a backstop. This also makes every other
  drop guard on a timed-out run (e.g. a cancelled `db.tx`) run promptly. A unit test pins
  the chunk name so an mlua bump that renames it fails loudly. A serve test pins the skip
  itself: a timed-out handler's nested `lur.async` task holding a `db.tx` must release the
  write lock at once. Without the skip that test fails, and so do none of the others. The
  other two pieces are belt-and-braces and no test can catch their removal. With the skip,
  mlua's terminate resume already starts in the poll frame. The memory cap failing inside
  that frame needs an allocation at exactly that point, which cannot be forced. Both are
  kept because they cost nothing on a normal run. Rejected: a GC after every
  run (cost on the hot path); a GC loop alone (one collection only frees one level of
  nested `lur.async` tasks).

## Capability layer

[`capabilities::install`](src/capabilities/mod.rs) fills the flat `lur` table in fixed order:

```
null · log · json · base64 · crypto · cookie · time · url · charset · html · xml · feed · validate · io · fs · http · env · db · kv · async · args · serve · state · text
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

- **`lur.fs`** canonicalizes to pick the granting root (defeating `..` and symlink escapes),
  then opens the file beneath that root's `cap-std` directory handle, so a symlink swapped in
  after the check cannot lead out of the root.
- **`lur.http`** checks every request and redirect hop against the net allowlist, uses a DNS
  resolver rejecting loopback/private/link-local IPs unless `--allow-private` (SSRF guard),
  caps redirects (10) and the buffered body (`--max-http-body`), and always verifies TLS.
  IP literals bypass the resolver, so `url_allowed` is their sole private-IP check; system
  proxy env vars (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) are ignored on purpose: through a
  proxy the name is resolved remotely and bypasses the resolver.
- **`lur.env`** returns `nil` for both denied and unset, so it isn't an existence oracle.

## One-shot mode

[`Runtime`](src/runtime.rs) owns one VM and a current-thread tokio runtime.
`main.rs::run_one_shot` builds a `RuntimeConfig` and calls `run_to_exit_code`, which maps the
chunk's top-level `return` to an exit code: number → that code, `nil`/`false` → 1,
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

**Rationale.**

- **`app.lua` top level runs once per pooled VM**, so it must be idempotent
  (`CREATE TABLE IF NOT EXISTS`, etc.).

### Routing

[`Router`](src/serve.rs) parses paths into `Static` / `Param` (`:name`, or trailing `:name?`)
segments. `resolve` picks the most specific match regardless of registration order: static
beats param at the same position; a concrete method beats `ANY` as tiebreak. Two routes of
the same method and the same static/param shape (counting every length an optional param
allows) are rejected at load; `/u/:id` and `/u/me` coexist, `/u/:id?` and `/u` do not.
Params are percent-decoded to raw bytes as `req.params`.

### Request lifecycle

`handle` (hyper adapter) → `dispatch_async`:

1. Body over `--max-body` → **413** before routing; the VM never sees it. `handle` reads the
   body through `http_body_util::Limited`, so reading stops at the cap instead of buffering
   an oversized (or chunked, length-less) body first.
2. No route → **404**.
3. `checkout()`, `build_req` (`method`, `path`, `params`, `query`/`query_all`, `headers`,
   `cookies`, `body`, streaming `read`, `json()`), then `call_handler` under the two-layer
   timeout.
4. Returned table → `response_from` (`status` default 200, must be 100–599; `headers`
   expanded to validated pairs, CR/LF and framing headers rejected; `body` default
   empty); timeout → **503**; uncaught `lur.serve.abort` → its status with a JSON body;
   other Lua error or bad return → logged, **500**. Handler errors never bring the server
   down.

Chunks are named from the CLI path (`script` if unnamed), so errors read `app.lua:2:`.
Handler and cron errors go through the same `diagnostics::render` as one-shot.

The body is a one-shot cursor (`BodyStream`): once `req.read(n)` streams it, `req.body` and
`req.json()` / `req.form()` / `req.form_all()` / `req.valid_body` raise instead of
returning a partial body.

### Per-call isolation

`fresh_env` makes a throwaway table whose `__index` is the frozen globals and sets it as the
handler/cron env: reads fall through, writes are discarded after the call. Together with
stripping `getfenv`/`setfenv`/`loadstring`, this prevents cross-request global bleed.

**Rationale.**

- **Upvalues persist across requests on a pooled VM** and can't be prevented (inherent to
  closures). `fresh_env` only isolates globals. Rule: never stash request data in an
  upvalue; use `lur.state`/`lur.kv`.

### Cron

Each job runs a `cron_loop`: compute the next fire from the 6-field spec, sleep (or stop on
shutdown), run on a pooled VM. Single-flight by default (an `AtomicBool` skips a tick while the
previous run is in flight; `overlap = true` allows concurrency). Missed ticks are never
replayed. A per-job `timeout` overrides the per-event timeout. Errors and timeouts are logged
with the job name, never propagated.

**Known limitations.**

- Cron: UTC only (no timezone setting), in-memory schedule, no missed-run replay, no
  `@every` interval syntax.

### Graceful shutdown

`run_with_shutdown` fans one shutdown future (SIGTERM/SIGINT, or any future in tests) out to
the accept loop and cron loops via a `watch` channel. Connections are wrapped by hyper-util's
`GracefulShutdown`, which tells idle keep-alive ones to close and lets in-flight requests
finish; cron runs are tracked by a tokio-util `TaskTracker`. After accept stops, both are
awaited, bounded by `--shutdown-grace`. Stragglers are aborted when the runtime drops.

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
- **Expiry.** Every `lur_kv` row has a nullable `expires_at` (epoch ms; `NULL` = never). A row
  with `expires_at <= now` is absent for every op, and `now` is read in Rust (`now_ms`), never
  from the database clock, so SQLite and Postgres agree. Expired rows are deleted when the pool
  opens and, at most hourly (`PurgeClock`), after a `set`/`add`/`incr`; reads don't delete.
  `open` adds the column to older tables (SQLite: `pragma table_info` check, tolerating a lost
  race; Postgres: `ADD COLUMN IF NOT EXISTS`). `incr` is one upsert that restarts an expired
  row and sets the expiry only if the row has none (or `renew_ttl`).
- **`lur.http` cache** is an in-memory `HttpCache` (`http.rs`) held in `RuntimeConfig` and
  shared by every pooled VM like `lur.state`, so it works without `--db`. This is the one
  Rust-side shared state besides `StateStore`; it is never exposed to VMs except through
  `opts.cache` and `lur.http.cache_clear()`. Policy check → build request → key → lookup; the
  key is a SHA-256 of method, final URL and all request headers. Entries are `Arc`s with an
  `Instant` expiry; expired ones are dropped on lookup and swept at most once a minute on
  insert. Size is bounded by `--http-cache-max-entries` / `--http-cache-max-bytes` (`0` =
  unlimited): a `BTreeMap<tick, key>` recency index gives LRU order, a hit re-stamps the
  entry, and an insert over either cap drops expired entries and then pops the oldest; a
  single entry larger than the byte cap is not stored. Sizes count body + headers + key.
- **`lur.text.replacer`** (`capabilities/text.rs`): an immutable byte trie (`Arc<Matcher>`),
  one per `replacer` call. `match` only changes which candidate on the walk wins (lowest
  pair index vs deepest), so both modes share one structure. `ReplacerStore` (in
  `RuntimeConfig`, shared by the pool) keeps `Weak` handles by `opts.key`; the matcher
  lives while some VM holds a userdata. Each matcher charges its estimated size to a
  counter (per VM in app data, or the store's for keyed) and releases it on drop; over
  `--replacer-max-bytes` it runs a GC and, if still over, raises. No shared mutable state
  is reachable from a script besides this keyed registry.
- **Invariants:** kv counters are integers; `kv.get` returns bytes; `db.tx`/`kv.update` are
  write transactions; integer steps (`kv.incr`/`decr`, `state.incr`/`decr`) reject fractions.

### SQLite (`storage/sqlite.rs`)

- `SqliteBackend` owns a lazily opened `sqlx` pool (WAL, file auto-created) and
  `lur_kv(key TEXT PRIMARY KEY, value BLOB, expires_at INTEGER)` — counters are stored as
  SQLite integers.
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
  bytes BYTEA, num BIGINT, expires_at BIGINT)` — `kind = 0` opaque bytes, `kind = 1` integer
  counter in `num`.
- **Isolation:** single statements run at `READ COMMITTED` (atomic, no retry). `db.tx` and
  `kv.update` use `SERIALIZABLE` on a pinned connection; conflicts abort with SQLSTATE
  `40001`, surfaced with a stable, locale-independent message (`map_pg_error`) rather than
  retried, since the body may have had side effects. The abort hits an in-transaction
  statement about as often as `COMMIT` (~50/50) — don't assume commit-only. No
  `retry_busy`/`busy_timeout` layer.
- **Trusted URL:** the `--db` URL comes from the operator, not scripts, so it's exempt from
  the net allowlist and SSRF guard (as the SQLite path is exempt from `lur.fs`).

### Cancellation-safe transactions

`db.tx`/`kv.update` open a `sqlx::Transaction` via `Pool::begin_with` (`BEGIN IMMEDIATE` on
SQLite, `BEGIN ISOLATION LEVEL SERIALIZABLE` on Postgres). If the wall-clock timeout drops the
future mid-body, dropping the `Transaction` queues a ROLLBACK on the connection before it
returns to the pool, so it is never reused mid-transaction (on Postgres it would sit
idle-in-transaction holding locks). Explicit commit/rollback consumes the transaction.

**Rationale.**

- **Cancellation cleanup relies on sqlx's rollback-on-drop**, using `Pool::begin_with` so the
  `BEGIN` statement stays ours (`BEGIN IMMEDIATE`, `BEGIN ISOLATION LEVEL SERIALIZABLE`).
  This replaced a hand-rolled guard (`PinnedTx` + detached ROLLBACK task) once sqlx 0.9
  gained `begin_with`; the cancel tests (single-connection pool) pass on both backends.
  Rejected: Postgres server-side timeouts (Postgres-only, and would kill legitimately slow
  transforms). `db.tx` closures hold `Weak` refs so cancellation drops the transaction
  immediately instead of waiting for Luau GC.

### `lur.state`

[`capabilities/state.rs`](src/capabilities/state.rs): a process-wide host-side `StateStore`
shared by all pooled VMs (via `RuntimeConfig::state`), **primitives only**. Each write takes
a fresh version from one store-wide counter that only goes up, so a key deleted and recreated
never reuses a version a reader might hold (prevents ABA) and deleted keys leave no entry
behind (an absent key is version 0). `update` is an optimistic CAS loop whose user function
runs with no host lock held; conflicts retry.

Entries carry an optional expiry (`Instant`, lazily filtered plus a once-a-minute sweep on
write) and an `evictable` flag. Only evictable entries are in the recency index
(`order: tick → key`), so `--state-max-entries`/`--state-max-bytes` can never drop a counter
or a lock: a write that does not fit after dropping expired and then least-recently-used
evictable entries raises and changes nothing. An entry past `expires` but inside `stale_until`
is *stale*: it reads as absent (version 0) everywhere except `fetch`, which falls back to it
when its loader raises. `fetch` is the one async function here; like `update`, its function
runs with no lock held, but unlike `update` it is never retried (it has side effects).

`fetch` is single-flight per key across the pool. The hit path is unchanged (one `lookup`);
on a miss, `StateStore::join` re-checks for a value and, under the same lock, either creates
a `Flight` (`flights: key → { id, watch::Sender, wait edges }`) and returns a `LeadGuard`, or
subscribes to the existing flight's `watch` channel. Lifecycle:

1. The leader runs the loader with no lock held, inside a task-local `LEADING` link
   (store, key, flight id, parent link).
2. Outcome (value, raised error, or a non-primitive rejection): `LeadGuard::finish`, under
   one lock, stores the value (CAS on version 0; one that does not fit stays uncached),
   publishes `Arc<Shared>` and removes the flight. Waiters turn it into their result:
   value → `"hit"`; error → the stale fallback, else raise.
3. No outcome (the leader's own deadline fired in the loader, or its future was dropped):
   the `LeadGuard` drop removes the flight, which drops the sender and closes the channel.
   Each waiter sees the close and calls `join` again: the first one in becomes the new
   leader, the rest subscribe to it.

Invariants:

- **A flight is removed on every exit** of its leader — finish, error, deadline, drop,
  panic — by its `LeadGuard` (matching the flight id, so a stale guard cannot remove a
  successor's flight). This relies on cancellation dropping host futures promptly; see
  [Two-layer timeout](#two-layer-timeout).
- **No flight outlives its run.** A leader can also be parked in a coroutine the script
  suspended and kept (e.g. in an app.lua upvalue), where nothing polls or drops it. Each
  flight records its VM (`VmLeads`, a per-VM count), and `with_timeout` calls
  `state::end_run` after every run on every ending, removing whatever that VM still leads.
  A VM runs one request or cron job at a time, so anything left is orphaned. With no flights
  led (the usual case) this is one atomic load. `end_run` is the backstop: the next rule
  removes almost every parked leader long before.
- **No Lua value crosses VMs.** Outcomes are `Prim` or `mlua::Error` (its text, as an
  error raised in another VM would be); each waiter builds its own Lua values.
- **No lock is held while waiting or while the loader runs**; waiters await the channel
  and stay under their own two-layer timeout.
- **No deadlock on re-entry.** A waiter inside a loader records a wait edge (its leading
  key → the key it waits on); before waiting, `join` follows the edges from the target and,
  if they lead back to one of the caller's own leading keys (same key, or a cycle across
  keys or VMs), returns `Inline`: the loader runs without single-flight, like the pre-
  single-flight `fetch`. `WaitGuard` removes the edges on every exit.
- **No run waits on a leader it parked itself.** `state::install` replaces
  `coroutine.resume` and `coroutine.wrap` (before `sandbox(true)` freezes `coroutine`) with
  C functions (`state/resume.rs`) identical to Luau's (bar debugger breakpoints) except
  that they count the Lua-level resumes running on the VM (`VmLeads::depth`). A flight
  records that depth when it starts.
  Resumes nest on the host stack (a coroutine awaiting a host call yields mlua's pending
  sentinel to whoever resumed it), so when the resume at depth `d` returns, however, every
  flight the VM started at depth `>= d` is in a coroutine nobody runs: it is removed,
  handing off. A leader is therefore waited on only while it can still run (a sibling task resuming
  another task's own thread is an mlua hazard that hangs with or without `lur.state`); a leader
  at depth 0 is the run's top level or a `lur.async` task, which the run is polling. The
  cost is one atomic load per resume while the VM leads nothing inside a coroutine. The
  one leader left is on a thread an error killed inside mlua's poll loop (a memory error or
  the deadline) or an `AsyncThread` dropped during GC (garbage, its guard dropped at the
  next GC, or `end_run`); a caller waiting on a leader of its own VM collects
  garbage every second (`wait_same_vm`, at most once a second per VM) until it hands off.

#### Why `lur.state` is shaped this way

- **`lur.state` grows TTL, an opt-in eviction cap, and `fetch`; there is no `lur.cache`.**
  A second in-memory store next to `state` and `kv` makes users pick between three; the
  honest axis is persistence (`kv` needs `--db`, `state` does not), so `state` is "`kv` in
  memory" with the same `ttl_ms` rules. Eviction is the hazard: a counter or an `add`-as-lock
  silently dropped is a correctness bug. So only entries written with `evictable = true` (and
  everything `fetch` stores) are ever dropped; a write that cannot fit raises
  (`state is full`) and changes nothing. Caps default to unlimited so existing scripts are
  unaffected. `fetch` (Rails' name, not `get_or_set`: the rest of the API is short Redis-style
  verbs) is separate from `update` because its function has side effects and so cannot be
  retried. `stale_ms` keeps an expired value only for `fetch`'s loader-failed fallback; every
  other operation sees it as absent. Rejected: a separate `lur.cache`, named cache
  instances (handle lifetime across the VM pool), a global LRU over every entry, negative
  caching of loader errors, and `kv.fetch` for now (needs `--db`, which a cache should not).
  Single-flight came later (next entry). The LRU is a separate small index in `state.rs` rather than a shared core with
  `HttpCache`: the two differ (non-evictable entries, stale windows, versions), and sharing
  would have changed a hot path for no behavioral gain. Counters (`hits`/`misses`/`stale`/
  `evictions`) are kept for a future `/metrics`.

- **`fetch` is single-flight per key, pool-wide, with no new API.** Without it, N
  concurrent misses ran the loader N times (a stampede on exactly the slow upstream the
  cache protects). One caller leads and runs the loader unlocked; the others await a
  `tokio::sync::watch` channel in the host-side flight table, each under its own timeout.
  How a leader's run ends decides what waiters get:

  | Leader ends with | Waiters get | Loader runs again? |
  | --- | --- | --- |
  | a value (incl. `nil`, or one too big to cache) | the value, status `"hit"` | no |
  | an error the loader raised (incl. its own `lur.http` timing out, OOM) | the stale value as `"stale"`, else the same error | no, and the failure is not remembered |
  | no outcome: its own deadline/timeout, or its future dropped (client gone, lost a `race`, shutdown, cron skip) | nothing yet: one waiter becomes the new leader | yes, once |

  Choices: waiters report `"hit"` (meaning "your loader did not run"; a new `"shared"`
  status would break scripts that branch on the three existing ones). `nil` is shared but,
  as before, not cached, so the next miss loads again. A non-primitive return is raised to
  every waiter (it is the loader's verdict, so no stale fallback). Error identity is not
  kept across VMs: a waiter gets the same `mlua::Error`, i.e. its text, never a Lua value
  from another VM, so an error table from `lur.validate` or `lur.serve.abort` in a loader
  arrives as a plain error (a 500, not the abort's status) — a known limitation. OOM inside
  the loader counts as the loader failing (shared), since it is not the waiter's own limit.
  The handoff signal is structural: only a `LeadGuard` removes a flight; finishing sends an
  outcome first, dropping it unfinished closes the channel. So a cancellation path nobody
  thought of still hands off, and the flight id stops a late guard from removing its
  successor. Re-entry: a `fetch` inside a loader that would wait on itself — the same key,
  or a cycle of keys (`a` → `b` → `a`, also across VMs) found by walking wait edges — runs
  its loader inline without single-flight, exactly as `fetch` behaved before; raising would
  have broken loaders that worked. Edges and the walk only exist for callers inside a
  loader, and the hit path is untouched (one lookup, as before). A leader can also be left
  parked in a coroutine the script suspended and kept reachable (an app.lua upvalue); its
  guard would never drop, blocking the key pool-wide. Flights therefore record their VM and
  are removed when that VM's run ends (`state::end_run`, from `with_timeout`; one atomic
  load when the VM leads nothing). A run can also fetch a key whose leader it parked itself,
  earlier in the same run. Waiting there would wait on itself: forever without a timeout,
  and in serve also for every later caller of the key, since `end_run` never comes. So
  `coroutine.resume` and `coroutine.wrap` are replaced (before the sandbox freezes
  `coroutine`) by versions that count the Lua-level resumes running on the VM; a flight
  records the count when it starts, and when the resume at depth `d` returns, for any
  reason, the VM's flights at depth `>= d` are removed (hand-off). Resumes nest strictly on
  the host stack (a coroutine that awaits a host call yields mlua's pending sentinel to
  whoever resumed it), so a leader still at depth `> 0` after its resume returned can only
  be parked; one at depth 0 is the run's top level or a `lur.async` task, both being polled.
  No dependence on mlua's hidden `Lua::poll_pending()`: any return of the resume counts.
  Consequence: a script-level scheduler that resumes a coroutine, gets the pending sentinel
  and forwards it with its own `coroutine.yield` (so the host call completes later) loses
  single-flight for that key: the leader is removed at the resume, a concurrent caller
  loads too (a duplicate load, never a deadlock; the leader's late `finish` finds no flight
  and only stores, CAS on version 0). Forwarding the sentinel is undocumented mlua
  behaviour, so this is accepted. The replacements are Luau's own `lcorolib.cpp` functions
  (`coresumey`/`auxresume`/`auxwrapy`) redone over the public C API in `state/resume.rs`,
  the crate's only `unsafe` (it is `deny`ed elsewhere): error messages and values,
  `luaL_where` prefixes, `debug.info` names and tracebacks match the builtins
  (`tests/coroutine.rs` compares both). Rejected: a Lua wrapper around the builtins (resume
  micro-benchmarks +112-127%, VM cold start +21%, an extra frame in tracebacks and
  `debug.info`); an mlua Rust function (errors become `mlua::Error` userdata, so error
  values and identity change); per-task tracking (the first design: each `lur.async` task
  got per-VM numbers pushed on a thread-local stack around every poll, and a flight
  recorded the stack it started under; it cost +3-6% on combinator micro-benchmarks, and
  with no timeout still waited forever on a leader parked inside a still-running sibling
  task or whose coroutine moved between sibling tasks). Cost of the replacement,
  interleaved medians of 6 rounds vs the builtins (main), two independent runs:
  `coroutine_resume_10k` +1.5 to +4.8%, `coroutine_wrap_generator_10k` +4.4 to +6.1%,
  `vm_cold_start` -0.6 to +2.7%, `async_*` -2.8 to +0.8%, `state_fetch_*` -0.2/+2.6%
  (noise: the hit path did not change). Gap: a memory error
  raised inside mlua's poll loop kills a leader's thread with its guard still on the stack:
  garbage, but only dropped at a GC, which a VM that only waits may never start. A caller
  waiting on a leader of its own VM therefore runs a full GC every second (at most once a
  second per VM) until it hands off (`wait_same_vm`). That also runs while a sibling waits
  on a slow live leader, accepted as same-VM waits are rare. A unit test leaks a guard into
  garbage, since that memory error cannot be raised on demand. A caller on another VM
  waits for that VM's GC, its `end_run`, or its own timeout.
  The first loader to finish stores (normally the leader; an inline caller can beat it),
  so the leader's `ttl_ms`/`stale_ms` win over its waiters': a waiter's own options never
  reach the store. The stale fallback on a shared failure uses the window stored with the
  old value, as it always has. A leader's OOM is shared as plain text so waiters are not
  themselves classified as OOM.
  Rejected: a per-key `Mutex` held across the loader (holds a lock across an `await`, and a
  waiter could not tell success from a leader that died); retrying on failure (the loader
  has side effects, and N waiters would retry N times: the stampede again); remembering
  failures (negative caching, rejected for `fetch` before); a chain of `oneshot`s per waiter
  (`watch` gives late subscribers the outcome and a close signal for free); passing Lua
  values between VMs (unsound: a VM's values belong to its own state).

## Validation & structured errors

[`capabilities/validate.rs`](src/capabilities/validate.rs) is installed after `feed` (pure
group, no `Policy`, no registry). A schema is an opaque userdata holding an immutable
`Node` tree built bottom-up (never cyclic; depth capped at 64, so cyclic *input* is safe);
a Lua function is a `Node::Func`. `check` is an async recursive walk (validators may call
async `lur.*`) that builds a fresh result and records `Issue`s (cap 100) instead of
stopping at the first.

Raising a table: a Rust callback's `Err` reaches Lua as userdata, so
[`capabilities/raise.rs`](src/capabilities/raise.rs) splits it: a Rust function returns
`(ok, value | error_table)` and a tiny Lua wrapper (made from one compiled factory chunk)
does `error(tbl, 0)`. The factory is compiled once per process and loaded once per VM by
`capabilities::install`, which hands it to every capability that raises tables.
`pcall` therefore sees the real table, whose metatable
(`error_meta`) is read-only with `__metatable = false`, so scripts cannot rewrite it for a
pooled VM. Uncaught, the error's `__tostring` (its `message`) is what the CLI prints.
`lur.serve.abort` reuses `raising`/`error_meta` (below).

### Validation rationale

- **`lur.validate` is a toolkit of functions, not a route DSL.** Small composable
  schemas called *inside* handler code (`parse`/`try`), in the Nitro/h3 style: no
  declarative route schema, so it works the same in one-shot scripts, cron jobs and
  handlers, and needs no server hooks. `req.valid_*` helpers and `lur.serve.abort` build
  on it (next entry).
  Coercion is deliberately narrow because inputs are mostly query-string text: `int` takes
  a number with an integral value or a string that is strictly `-?%d+` (`1.5`, `1e3`,
  `0x10`, `+1` and whitespace are rejected, not "helpfully" parsed); `number` takes
  finite decimal floats; `bool` takes only `"true"`/`"false"`; `str` never coerces from a
  number. `""` counts as *missing* for `int`/`number`/`bool` so `?limit=` takes the default
  (a form field left blank is not an input), but stays a valid string for `str`. Luau
  numbers are f64, so integers beyond 2^53 are rejected rather than silently rounded; keep
  larger ids as strings. Unknown object keys are dropped by default (mass-assignment safe,
  and the result is always a fresh table) with `strict = true` to make them an error.
  There are no `date`/`uuid`/`email`/`url` builtins: each has many defensible definitions
  and a plain function validator (`function(v) ... return v end`, raising to reject)
  plus `lur.time`/`lur.url` covers them without freezing one in the API. `pattern` is a
  Lua pattern evaluated by Luau's own `string.find`, not a regex: no new dependency, the
  same matcher scripts already know, and its limits (no alternation, not anchored
  implicitly) are documented. All issues are collected (`issues = { { path, message } }`,
  paths `a.b[2].c`, root `""`, capped at 100) so a client can fix a form in one round
  trip. The error is a table (`kind = "validation"`) raised via Lua's `error(tbl, 0)`
  through a small Lua wrapper — see ARCHITECTURE.md "Validation & structured errors" — so
  `pcall` hands scripts a real table; it has `__tostring`, which is also what the CLI
  prints, and a locked metatable (`__metatable = false`, read-only) so a script cannot
  rewrite it for later errors on a pooled VM. *Across `lur.async` tasks*: mlua flattens a
  failed task's error value to text inside `call_async`, but that rendering calls the
  table's `__tostring` — ours — which records the table in a per-VM slot; the combinator
  takes it back right after the task fails and re-raises it, so
  `pcall` around `all`/`race`/`map` sees the table and `settled` returns it in `err`. This
  costs nothing per task on success; per combinator call it costs one Lua wrapper frame,
  whose inner function returns one value and signals failure by returning a private
  sentinel table that carries the error (`raise::raising_hot`): returning `(true, value)`
  instead measured ~700 extra instructions per `all` call (~+4% on `async_all_small`, vs
  ~+2.5%; `async_race_small` ~+5%, about 0.8 µs per call, since the wrapper frame is a
  larger share of a cheap call). Rejected: an `xpcall` trampoline around every task
  (+24–49% on `vm_cold_start`, `async_map` and `async.all`, budget 5%), and recording the
  table at raise time (wrong after a task catches one error and later re-raises it while
  another task raised in between). Accepted limits: only `error_meta` tables are recovered
  (a script's own `error({...})` in a task still arrives as text, since a plain table has no
  hook); `any` keeps aggregating failures as text; and the match is by text: the error
  must be the table's message, alone or followed by a newline (where mlua appends the
  traceback), so any string starting with message + `"\n"` matches. A string error raised
  in the same run that begins with the text of an error table rendered earlier and not yet
  consumed would come back as that table. That is script-only and cannot forge a table the
  script does not already hold; the slot is reset at the start and end of every run, so
  nothing crosses runs on a pooled VM (same match as the abort slot below).
  `optional` only waives "required" (an inner `default` still fills). `lur.null` is treated
  as absent for object fields and the root (normalized in `check_object` / the entry
  point, so `optional`/`default` see `nil` and a required field says "is required" rather
  than a type error); it is deliberately *not* normalized for list items, where silently
  dropping a `null` would change the list, and function validators keep seeing the raw
  value. This is what makes `req.valid_body` accept the usual JSON clients that send
  `{"x": null}`. `default` values are
  not checked against `min`/`max`/`pattern`; a function validator's returned table is not
  copied, so it may alias its input. Rejected: raising a Rust error (scripts would get an
  opaque userdata), a global `pcall`, and a regex crate. Not done: coercion defaults beyond
  the above, async schemas cached by name, localized messages.

### `lur.serve.abort` and `req.valid_*`

mlua flattens a table error to a string once it crosses `call_async`, so `call_handler`
cannot read `status` off the failing error. Instead `abort` records its response in an
`AbortSlot` (`Arc<Mutex<AbortState>>`, created with the `Registry`, shared by the VM's
`abort` closure and the pooled `Vm`):

- `abort` checks `status` (400–599) and `message`, JSON-encodes `{error, data?}` right away
  (so non-encodable `data` is an ordinary error at the call site), stores
  `PendingAbort { status, message, body }`, and returns the error table to the `error(tbl, 0)`
  wrapper. It refuses unless `in_request` is set.
- `call_handler(.., in_request)` resets the slot (`in_request`, `pending = None`) before every
  run and takes `pending` after it. A `pcall`-caught abort therefore cannot leak into a later
  request or cron run on the same VM, and a handler that swallows one and returns normally
  gets its normal response (`pending` is only consulted on `Err`).
- On `Err`, if `pending.message` equals the error text (`RuntimeError` message, optionally
  followed by a traceback; looked through `CallbackError`/`WithContext`), the result is
  `CallError::Aborted` and `dispatch_async` answers `status` with a
  `Content-Type: application/json` body (logged at `info`, `warn` for 5xx). Anything else —
  including a timeout, which is checked first — is the usual 500/503. The happy path pays two
  uncontended mutex locks per run and nothing else.
- Cron runs and one-shot scripts have `in_request = false` (one-shot installs a stub like
  `lur.serve.http`), so `abort` is an ordinary runtime error there.

`req.valid_query` / `req.valid_params` are produced lazily by the request metatable's
`__index` (like `req.body`), so a request that never validates pays nothing. Each builds an
async Rust function over `lur.validate.try` plus the same `raising` wrapper; on `(false, err)`
it calls the same `abort_table` with `fail_status` and `data = err.issues`. For an object
schema, `validate::list_field_names` tells it which fields to feed from `query_all`.
Errors from `try` itself (bad schema, timeout, OOM) propagate untouched.

`req.form` / `req.form_all` / `req.valid_body` are lazy `__index` products the same way.
They share the query path's pieces: `parse_query` (one decoder for query strings and
urlencoded bodies; the body is first `from_utf8_lossy`'d), `last_table` / `all_table` (the
`query` / `query_all` builders) and `feed_lists` (list fields from the all-values table,
built only when the schema has list fields). The media type is read from the lowered
`req.headers` table at call time. `valid_body` does all synchronous work (stream check,
content-type dispatch, JSON decode or form parse) before awaiting `try`, so the body mutex
is never held across an await. Decode errors map to `abort(400, "invalid JSON body")`
(memory errors propagate); an empty JSON body is handed to `try` as `nil` and, if that
fails, answered with the same 400 rather than the validation message.

#### Rationale: abort and form bodies

- **`lur.serve.abort` and `req.valid_*`** wire `lur.validate` into handlers, still as
  plain calls inside the handler (h3 `createError` style), not route metadata.
  *Naming*: `abort`, not `error`, so a call site reads as "stop here" and a status can
  never be mistaken for a success; only `400..=599` is accepted (`abort(200, …)` is an
  argument error) so it cannot produce a success or redirect. The body is
  `{"error": message, "data": data}` JSON with `data` omitted when `nil`: one fixed,
  machine-readable shape instead of a per-handler convention; handlers that want another
  shape return a normal response. Side effects before the abort are not rolled back: there
  is no transaction around a handler, and pretending otherwise would be worse than saying
  so. `fail_status` (not `status`/`error_status`) names the status used when validation
  fails, default 400, `404` for path params. *Recognising an uncaught abort*: mlua flattens
  a table error to a string across `call_async`, so the server cannot read `status` from
  the error. `abort` stashes the encoded response in a per-VM slot that `call_handler`
  clears before every run and consults only on `Err`, matching it to the failing error by
  message (the error text is the message, alone or followed by a newline and traceback, so
  `error("xy")` does not match an abort `"x"`); so a `pcall`-caught abort cannot leak into a later request on the pooled VM, a
  caught-then-returned handler is a normal response, and the happy path pays two
  uncontended locks per run. Rejected: an `xpcall` trampoline per handler/task (+24%
  async cost, see above) and encoding the status into the error string (spoofable by any
  `error("...")`, visible in logs). Since error tables now survive `lur.async` (above), a
  `pcall` around a combinator also sees an abort's `.status`; the slot stays the source of
  truth for the uncaught response. The message match means a script that catches an abort
  and re-raises a string with the identical message is still treated as that abort; the
  status is the one the script chose earlier, but the body is the earlier abort's, so a
  later plain `error()` whose text equals a swallowed abort's message in the same run
  returns that abort's `data` (script-only: only the script itself can cause it). An abort raised inside a `lur.async` task is honored when
  uncaught, nested combinators included. A bare `lur.validate.parse` that fails
  uncaught in a handler stays a **500** on purpose: it signals a bug (or an unchecked
  assumption) in the script, while client input goes through `req.valid_*`. Data that
  is not JSON-encodable fails at the `abort` call (ordinary, catchable error), not later
  while writing the response. Not done: custom error renderers.
- **Form bodies and `req.valid_body`.** `req.form()` / `req.form_all()` raise on a
  non-urlencoded Content-Type instead of returning `{}`: the caller is deciding how to read
  the body, and silently yielding nothing for a JSON post hides the bug. `valid_body` is the
  place that dispatches (JSON / urlencoded / 415), because that is where a 4xx rather than a
  500 is the right answer. It reuses the query decoder and list feeding so `valid_query` and
  `valid_body` cannot drift (`+` is a space, bad `%` stays literal, invalid UTF-8 in the
  body is replaced). Malformed JSON is always 400 `invalid JSON body`, independent of
  `fail_status` (that knob is for schema issues, e.g. 422). An empty JSON body is invalid
  unless the schema accepts `nil` (decided by running `try` on `nil`, not by sniffing the
  schema), so `v.optional(...)` bodies work and everything else gets a 400 instead of a
  confusing "expected object, got nil" issue list. Multipart stays unsupported (415 from
  `valid_body`). Content-Type is read from `req.headers` at call time, so a script that
  rewrites that table changes the dispatch (harmless: it can only affect its own request).

### Error tables across `lur.async` tasks

`lur.async` runs each task in its own thread via `call_async`. When the thread fails, mlua
renders the error value with `luaL_tolstring` plus a traceback and keeps only that text
(`Error::RuntimeError`), so the combinator never sees the table. The rendering does call the
table's `__tostring`, and for error tables that is `error_meta`'s Rust function: besides
returning `message`, it records `(table, message)` in the VM's `raise::Flattened` slot
(`lua` app data). Right after a task fails (synchronously, in `drive`'s `on_result`, before
any other Lua runs) the combinator calls `raise::recover`, which *takes* the slot and returns
the table if the error text is that `message`, alone or followed by a newline (where mlua
appends the traceback): any string starting with `message` + `"\n"` matches. Only the script
can produce such a string, and a match only hands it back a table it already held, so this
cannot forge an error.
`all`/`race`/`map` are `raising_hot` wrappers (the inner function returns one value, or a
private per-VM sentinel table carrying the error, which the wrapper empties before raising;
cheaper than `raising`'s `(ok, value)` pair), so a recovered table is re-raised with
`error(tbl, 0)` and `pcall` around the combinator gets the same table (`.status`, `.issues`);
`settled` puts it in `err`; `any` still aggregates failures as text. Other errors, including
plain script tables without `error_meta`, pass through as before. Nesting works because the
re-raise is flattened (and recorded) again by the outer task. Nothing runs while tasks
succeed; the cost is one Lua wrapper frame per `all`/`race`/`map` call and a slot reset at the
start and end of every run (`Runtime::guarded`, `call_handler`), so a table never outlives its
run, nor stays alive on an idle pooled VM.
The `AbortSlot` match above still decides the HTTP response for an uncaught abort.

## Async core

[`capabilities/async_ops.rs`](src/capabilities/async_ops.rs): `lur.async.sleep` and
combinators `all`/`race`/`any`/`settled` over arrays of zero-arg functions. `--max-concurrency`
is an `Arc<Semaphore>`; each task takes an owned permit before running. Lua still runs one step
at a time — tasks interleave only at I/O awaits; `race`/`any` drop (cancel) the remaining
futures on settlement. Error tables raised in a task keep their identity (see "Error tables
across `lur.async` tasks").

Every combinator goes through one `drive` helper: a `FuturesUnordered` holding at most a
*window* of futures (`--max-concurrency`, and for `map` also `opts.limit`), refilled as items
finish, with results placed by index. The first error (or `race`/`any` winner) returns at once
and drops the rest. Bounding the window keeps a long list from parking thousands of futures
on the semaphore, which was quadratic in list length. `map(list, fn, opts?)` runs
`fn(item, index)` per element; nested combinators under a tight `--max-concurrency` can
still starve.

**Rationale.**

- **Sliding window for every `lur.async` combinator.** `all`/`settled`/`race`/`any`/`map`
  share one `drive` helper over a `FuturesUnordered` that holds at most a window of futures
  and creates the next only when one finishes. Two reasons: `try_join_all` silently becomes
  `FuturesOrdered` above 30 futures, so an early error waited behind earlier items and
  queued items kept starting; and with every future created up front, thousands of them
  parked on the semaphore and each wake-up rescheduled the lot (profiled: 80k items under
  `limit` took ~500 ms, 20x the unbounded run; windowed it is linear). The window is
  `--max-concurrency`, further narrowed by `map`'s `opts.limit` (a per-call bound, so
  there is no per-call semaphore any more; only the global permit is still acquired inside
  each task, since it is shared across calls). Nested combinators (a `map` inside
  `map`/`all`/`any`) under a tight `--max-concurrency` can still starve, since a parent
  task holds a global permit while its children wait for one. `limit` must be an integer
  \>= 1 (`math.huge`, floats and strings raise naming `opts.limit`); values above the item
  count are harmless. The first error returns immediately and the rest are cancelled. `drop_errors` compacts the
  result in input order (failed items vanish, later indexes shift) and discards error
  messages; use `settled` when they matter. A `nil` return still leaves a hole, as in
  `all`. The list must be a pure array (non-sequence keys raise) so a typo'd dict is not
  silently ignored.

## Diagnostics & docs rendering

- **Diagnostics**: chunk name is the CLI path as typed (not canonicalized or
  basenamed). Exit codes deliberately not split per error kind.
- **`lur docs`**: hand-rolled ANSI renderer over `pulldown-cmark` (no default features);
  `termimad` rejected for its dependency weight. 16-color SGR only.
  Table width comes from `COLUMNS` (positive) → `TIOCGWINSZ` via `rustix::termios` (unix,
  no `unsafe`) → 80; no wrapping when stdout is not a TTY, so pipes get full-width tables.
  Cell width uses the `unicode-width` crate rather than a hand range table: the hand table
  got emoji (✅) and ZWJ sequences (👨‍👩‍👧) wrong. Nested quotes / lists inside a quote
  remain unwrapped (known limitation).

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
