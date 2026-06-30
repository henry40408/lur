# Storage atomic operations: kv / state parity + db busy handling

**Status:** approved design, ready for implementation plan
**Date:** 2026-06-30

## Problem

`lur`'s three storage surfaces have uneven atomic-operation support:

- **`lur.state`** (in-memory, cross-VM): rich — `incr` (atomic add) and `update`
  (optimistic, version-stamped CAS), plus `get`/`set`.
- **`lur.kv`** (SQLite-backed): only `get`/`set`/`delete`. No atomic operations
  at all, despite being the persistent sibling of `state` and despite SQLite
  giving *stronger* atomicity (a single statement is atomic) than `state`'s
  optimistic loop.
- **`lur.db`** (full SQL): `tx` provides transactional atomicity, but under WAL
  with concurrent writers a transaction can fail with `SQLITE_BUSY`, which
  callers must handle themselves.

This project closes the `kv` gap, brings the matching primitives to `state` for
symmetry, and makes `db` transactions wait out write-lock contention instead of
failing fast.

## Goals

- `lur.kv` gains `incr`/`decr`, `add` (set-if-absent), `cas` (compare-and-set),
  and `update` (read-modify-write), with semantics that fit SQLite.
- `lur.state` gains `cas`, `add`, and `decr`, so the two key/value surfaces read
  the same way. `state.incr` becomes integer-semantics to match `kv.incr`.
- `lur.db` transactions and the new SQLite-backed atomic ops wait out
  `SQLITE_BUSY` via `busy_timeout` + `BEGIN IMMEDIATE`, instead of erroring on
  the first lock conflict.
- `lur.kv.get` stops crashing on non-BLOB cells (a latent bug surfaced by
  integer counters).

## Non-goals (YAGNI)

- No new SQL-wrapper surface on `lur.db` (e.g. `upsert`, `exec_many`). `db.tx`
  plus the `kv` atomic ops already cover the use cases; adding SQL helpers would
  duplicate what raw SQL does.
- No configurable `busy_timeout` CLI flag — a fixed, reasonable default
  (5000 ms) is baked in. (Possible follow-up.)
- No floating-point counters on `kv` (and, after this change, on `state`).
  Counters are integers. Arbitrary numeric values still go through `set`.
- No distinct integer primitive type in `state`. Luau numbers are IEEE-754
  doubles, so the exact-integer ceiling is 2^53 regardless; `f64` storage loses
  nothing a Luau script could observe.

## Background: two probed facts that shape the design

1. **sqlx is strictly typed.** `kv.get` currently decodes the value column as
   `Vec<u8>`. Against an `INTEGER` cell (which counters produce) this **errors**:
   `mismatched types; Rust type Vec<u8> (as SQL type BLOB) is not compatible with
   SQL type INTEGER`. So either counters avoid integer affinity, or `get` must
   become type-aware. We choose type-aware `get` (component 1).
2. **SQLite upsert counters store native `INTEGER`** and `'text' + 1` silently
   coerces the text to `0`. So a naive `value = value + ?` on a key holding a
   blob would silently reset it to `n` rather than erroring. `kv.incr` guards
   against a non-integer existing value to match `state.incr`'s safety.

## Components

### 1. Module split: extract `lur.kv` into its own module

`kv` currently lives inside `src/capabilities/db.rs` (`install_kv`). It is about
to grow five operations, so move it to a new **`src/capabilities/kv.rs`**.

- `db.rs` remains the owner of the SQLite pool. `db::install` returns a small
  shared handle so `kv` can reach the same lazily-opened pool:

  ```rust
  /// Shared, lazily-opened SQLite pool + its configured path, handed from
  /// db::install to kv::install so both capabilities use one pool.
  pub(crate) struct SqliteShared {
      cell: Arc<OnceLock<SqlitePool>>,
      path: Arc<Option<PathBuf>>,
  }

  // capabilities::mod::install
  let sqlite = db::install(lua, &lur, config.db_path.clone())?;
  kv::install(lua, &lur, &sqlite)?;
  ```

- Pool machinery stays in `db.rs`, exposed `pub(crate)` to `kv`: `ensure_pool`,
  `open_pool`, `bind_all`, and a new `begin_immediate` helper (component 4). The
  capability install order in `mod.rs` is otherwise unchanged.
- `read_row`/`get` decoding helpers stay in `db.rs`; `kv` gets its own value
  decoder (component 2) since its contract differs (always returns bytes).

### 2. `lur.kv` — type-aware `get` + new atomic operations

**`get` becomes type-aware (bug fix + counter support).** Read the cell by its
runtime type and always return **bytes** (or `nil`):

- `NULL` → `nil`
- `INTEGER` → its decimal-string bytes (e.g. the counter `42` reads back `"42"`)
- `REAL` → its decimal-string bytes
- `TEXT` / `BLOB` → the raw bytes (unchanged from today)

This keeps the `kv` contract "values are raw bytes", never panics, and lets a
counter created by `incr` be read by `get`. The ergonomic numeric path is the
return value of `incr`/`decr`, not `get`.

**New operations** (`value` arguments are byte strings; SQLite atomicity):

| API | Semantics | Returns |
|---|---|---|
| `incr(key, n?)` / `decr(key, n?)` | Atomic integer add/subtract; `n` defaults to `1` and must be an integer. Stored as native `INTEGER`. If the key holds a **non-integer** value, raises `lur.kv.incr: existing value is not an integer`. | new value (Lua integer) |
| `add(key, value)` | Set-if-absent (`INSERT … ON CONFLICT(key) DO NOTHING`). | `true` iff inserted |
| `cas(key, expected, new)` | Byte-equality compare-and-set. `expected = nil` means "expect absent"; `new = nil` means "delete". Covers set-if-absent / update-if-equal / delete-if-equal. | `true` iff applied |
| `update(key, fn)` | Read-modify-write: `fn(current)` receives the current bytes (or `nil`) and returns the new bytes (or `nil` to delete). Runs in a `BEGIN IMMEDIATE` transaction on a pinned connection (like `db.tx`). | new value (bytes \| `nil`) |

Notes:
- `add(key, v)` is the ergonomic spelling of `cas(key, nil, v)`; both are kept
  because the user asked for both and `add` reads clearer at call sites.
- `incr`/`decr` take an integer step (argcheck: number with no fractional part)
  and maintain an integer counter, mirroring `state.incr`.
- **`update` re-entrancy:** the transform `fn` must not call `lur.kv` or
  `lur.db` write operations — they would contend with the pinned connection.
  A re-entry guard (a `thread_local` flag, mirroring `state.update`'s
  `IN_UPDATE`) raises `lur.kv.update cannot be re-entered` rather than
  deadlocking.

### 3. `lur.state` — parity additions + integer `incr`

`state` already has `get`/`set`/`incr`/`update`. Add the matching primitives and
align `incr`:

- **`incr(key, n?)` → integer (changed):** now integer-semantics. `n` defaults
  to `1` and must be an integer; the existing value must be `nil` or a whole
  number, else `lur.state.incr: existing value is not an integer`. Arithmetic is
  exact in `f64` within ±2^53 (Luau's own integer ceiling), so no new primitive
  type is needed. **Breaking change** (previously accepted fractional steps and
  values); called out in the PR and docs.
- **`decr(key, n?)` → integer (new):** `incr(key, -(n or 1))`.
- **`cas(key, expected, new) → bool (new):** value-equality compare-and-set.
  Snapshot `(value, version)`; if the current value equals `expected`, apply via
  the existing version-stamped `compare_and_set`. Strings compare by bytes;
  numbers compare by `f64` equality (documented float caveat). `expected = nil`
  matches an absent key; `new = nil` deletes.
- **`add(key, value) → bool (new):** `cas(key, nil, value)` — set iff absent.

All new ops keep `state`'s existing discipline: the re-entry guard
(`reject_reentry`) applies, and no host lock is held across user code.

### 4. `lur.db` — busy handling (the "busy-retry")

No new SQL surface. Make write contention wait instead of failing, the standard
SQLite way:

- **`busy_timeout`:** set `PRAGMA busy_timeout = 5000` (ms) on pool connections
  via `SqliteConnectOptions::busy_timeout(Duration::from_millis(5000))` in
  `open_pool`. SQLite then blocks-and-retries internally on a locked database up
  to the timeout.
- **`BEGIN IMMEDIATE` for multi-statement / callback transactions:** `db.tx`
  and `kv.update` acquire the write lock at transaction start rather than
  lazily, via a shared `begin_immediate(pool)` helper (runs `BEGIN IMMEDIATE`
  on a pinned connection from the pool). This prevents the "started reading,
  then failed to upgrade to a write" busy/deadlock, and guarantees a user
  callback runs **exactly once** (the lock is already held; no whole-transaction
  retry that could replay side effects). Single-statement ops (`kv.incr`/`decr`,
  `kv.add`, `kv.cas`) are each one atomic statement and need no transaction —
  `busy_timeout` alone absorbs their lock contention.
- **Behavior change:** `db.tx` moves from a deferred `BEGIN` to `BEGIN
  IMMEDIATE`, so a read-only `tx` also takes the write lock — slightly less
  read concurrency, but the correct default for a write-intent transaction API.
  Deliberate; noted in the PR and `db.tx` docs.

The outer `tokio::time::timeout` (one-shot `Runtime::guarded`, server
`call_handler`) still bounds any wait, so a long lock hold cannot hang a
request past its deadline.

## Data flow

```
lur.kv.incr(k, n)   ── INSERT…ON CONFLICT DO UPDATE SET value=value+n ─► SQLite INTEGER cell
lur.kv.get(k)       ── type-aware decode ─► bytes (INTEGER→"42", BLOB→raw, NULL→nil)
lur.kv.add/cas      ── one atomic statement (INSERT OR IGNORE / UPDATE|DELETE … WHERE value=?) ─► bool
lur.kv.update(k,fn) ── begin_immediate(pool) ─► read ─► user fn ─► write ─► commit
lur.db.tx(fn)       ── begin_immediate(pool) ─► fn(tx handle) ─► commit / rollback
                       (busy_timeout absorbs lock contention)

lur.state.incr/decr/cas/add ── StateStore mutex (brief) + version-stamped CAS, no lock across user code
```

`db.rs` owns the pool and the `begin_immediate`/`ensure_pool`/`bind_all`
helpers; `kv.rs` consumes them. `state.rs` is independent (in-memory store).

## Error handling

- Each operation reports in its own voice (`lur.kv.incr: …`, `lur.state.cas: …`),
  reusing `argcheck::arg` for argument-type errors.
- `kv.incr`/`decr` on a non-integer cell, and `state.incr`/`decr` on a
  non-integer value, raise a clear "existing value is not an integer" error
  rather than silently coercing.
- `kv.update` / `state.update` re-entrancy raises a "cannot be re-entered" error
  instead of deadlocking.
- A genuinely stuck write lock surfaces as the SQLite busy error only **after**
  `busy_timeout` elapses (or the outer request timeout fires first).
- `kv.get` never panics: unknown/edge column types fall back to their byte form.

## Testing strategy

- **Unit tests (inline):** `state.rs` — `cas` apply/reject by value, `add`
  idempotence, `incr`/`decr` integer guard (reject fractional). `kv.rs` /
  `db.rs` helpers — type-aware `get` decoding for INTEGER/REAL/BLOB/NULL.
- **Integration tests (`tests/`):** a SQLite-backed suite exercising
  `kv.incr`/`decr` counters, `kv.add` set-if-absent, `kv.cas` (all four
  expected/new combinations), `kv.update` RMW, and the `get`-after-`incr`
  round-trip that currently crashes. A concurrency test that two operations on
  the same key serialize correctly (counter ends at the expected total).
- **Guide examples (`docs/GUIDE.md` + `tests/guide.rs`):** add runnable
  `assert` examples for every new function. The function-level drift guard
  (`every_runtime_function_has_an_example`) **forces** each new `lur.kv.*` /
  `lur.state.*` function to have a worked example or the suite fails.
- **Docs:** README `lur.kv` / `lur.state` / `lur.db` API entries updated;
  ARCHITECTURE module map gains the `src/capabilities/kv.rs` row and notes the
  `BEGIN IMMEDIATE` / `busy_timeout` invariant.
- Gates unchanged: `cargo nextest run`, `cargo clippy --all-targets -- -D
  warnings`, `cargo fmt --all`, `cargo deny check`.

## Implementation note: batching

Land in dependency order so each step is independently testable:
1. Extract `kv` to its own module (no behavior change) + type-aware `get`
   (fixes the latent crash; add the regression test first).
2. `db` busy handling (`busy_timeout` + `begin_immediate` helper; `db.tx` →
   IMMEDIATE).
3. `kv` atomic ops (`add`, `cas`, `incr`/`decr`, `update`) on top of the helper.
4. `state` parity (`incr` → integer, `decr`, `cas`, `add`).
5. Guide examples, README, ARCHITECTURE.
