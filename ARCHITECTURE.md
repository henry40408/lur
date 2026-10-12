# Architecture

For developers working *on* `lur`; for usage see the [README](README.md).
Design rationale and known limitations: [docs/decisions.md](docs/decisions.md).

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

## Capability layer

[`capabilities::install`](src/capabilities/mod.rs) fills the flat `lur` table in fixed order:

```
null · log · json · base64 · crypto · cookie · time · url · charset · html · xml · feed · validate · io · fs · http · env · db · kv · async · args · serve · state
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
  proxy env vars are ignored. See [docs/decisions.md](docs/decisions.md).
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

### Cron

Each job runs a `cron_loop`: compute the next fire from the 6-field spec, sleep (or stop on
shutdown), run on a pooled VM. Single-flight by default (an `AtomicBool` skips a tick while the
previous run is in flight; `overlap = true` allows concurrency). Missed ticks are never
replayed. A per-job `timeout` overrides the per-event timeout. Errors and timeouts are logged
with the job name, never propagated.

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
