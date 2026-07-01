# Storage backend seam (Phase 1 of PostgreSQL support) — design

Date: 2026-07-01
Status: approved, ready for implementation planning

This is **Phase 1 of two** for PostgreSQL support. Phase 1 extracts a storage-backend
seam as a pure refactor with SQLite as the sole implementation and **zero user-visible
change**. Phase 2 (separate spec/plan) adds the PostgreSQL backend behind the seam.

## Background / why

`lur` reserved a multi-backend storage route from the start (2026-06-26 design spec:
"Storage backends other than SQLite (e.g. Postgres) — trait reserved, not implemented";
sqlx was chosen over rusqlite specifically for its "same-API multi-backend path … the
reserved Postgres route"). That reservation was only ever at the dependency level: the
actual `db.rs` / `kv.rs` are saturated with SQLite-specific types, SQL, concurrency
primitives, and (as of #56) busy-error classification. No abstraction seam exists.

A concrete need has arrived: connect `lur` to an operator's existing PostgreSQL. Rather
than bolt PG onto the SQLite-concrete code, Phase 1 first carves out a clean internal
seam so Phase 2's changes stay localized behind it.

## Goals

- Introduce a storage-backend abstraction that isolates all SQLite-specific code behind a
  defined method surface.
- `db.rs` / `kv.rs` no longer reference `sqlx::sqlite::*` types directly (only the backend
  module does).
- **Zero user-visible behavior change**: the entire existing test suite passes unchanged;
  no perf regression.
- Shape the seam so Phase 2 adds PostgreSQL by extending an `enum` and filling in match
  arms — no changes to `db.rs` / `kv.rs` call sites.

## Non-goals

- **No PostgreSQL implementation** in Phase 1 (that is Phase 2).
- **No runtime/dynamic backend switching** ever (SQLite⇄PG toggled per-request or mid-run).
  A deployment selects one backend and stays on it; this assumption is load-bearing and
  deliberately keeps the design to enum dispatch, not a `dyn` trait object.
- **No data-migration tooling** (moving an existing SQLite dataset into PostgreSQL). This
  is a deliberate non-goal, not an oversight — user app tables are the user's to migrate,
  and no such need exists today. See "Future migration friction" below for how the seam
  keeps a future migration cheap without building anything now.
- No new user-facing API, CLI flag, or config field. `RuntimeConfig.db_path` stays
  `Option<PathBuf>` (Phase 2 changes it to a connection target).

## Chosen approach: enum dispatch

`enum Backend { Sqlite(SqliteBackend) }` — a single variant in Phase 1, deliberate
scaffolding for the `Postgres` variant Phase 2 adds. Dispatch methods `match` on the enum
(one arm today).

Rejected alternative: `StorageBackend` trait + `Box<dyn>`. It matches the original spec's
"reserved trait" wording and would allow out-of-tree backend *types*, but adds
async-trait + dynamic-dispatch + generic-row complexity for extensibility we do not need
(exactly two backends, no plugin model). The choice is reversible: the enum's method set
*is* the interface, so promoting it to a trait later is a mechanical refactor. Holding
multiple backend *instances* (e.g. a future per-alias connection registry) does **not**
require the trait — a `HashMap<alias, Backend>` works with the enum — so that possibility
is not foreclosed either.

## Module layout

New directory `src/capabilities/storage/`:

- `storage/mod.rs` — `enum Backend { Sqlite(SqliteBackend) }`; the backend-neutral `Shared`
  lazy-open handle; the public dispatch methods; the neutral result types (`ExecResult`,
  and the `Transaction` handle enum).
- `storage/sqlite.rs` — `SqliteBackend`: owns the pool and **all** SQLite specifics — SQL
  dialect (`INSERT OR REPLACE`, `ON CONFLICT`, `typeof(value)='integer'`, `RETURNING`,
  `last_insert_rowid()`), `?` binding (`bind_all`/`bind_one`), row→Lua type mapping
  (`type_info().name()` → INTEGER/REAL/TEXT/BLOB), WAL / `busy_timeout` / `BEGIN
  IMMEDIATE`, and the `retry_busy` / `is_busy` / `jitter_delay` helpers from #56.

`db.rs` and `kv.rs` keep only their `install` functions (build the `lur.db` / `lur.kv` Lua
tables, wire Lua functions to `Backend` methods). Both shrink substantially.

## The seam: `Backend` method surface

Derived from current usage. All take `&Lua` where they build Lua values directly (mirrors
today's `read_row(&Lua, …)`), so **no intermediate row materialization is introduced** —
protecting the current query-path performance.

For `lur.db`:
- `exec(&Lua, sql, params) -> ExecResult { rows_affected: u64, last_insert_id: i64 }`
  (retry-wrapped).
- `query(&Lua, sql, params) -> Table` (array of row tables keyed by column name).
- `begin(&Lua) -> Transaction` — a pinned-connection write transaction handle exposing
  async `exec` / `query` / `commit` / `rollback`. `exec`/`query` on it are dialect-specific
  (binding + row mapping), so they live in the backend.

For `lur.kv` (no Lua callback; dialect SQL stays in the backend):
- `kv_get(&Lua, key) -> Value` (type-aware: NULL→nil, INTEGER/REAL→decimal-string bytes,
  TEXT/BLOB→raw bytes).
- `kv_set(key, bytes)`, `kv_delete(key)`.
- `kv_add(key, bytes) -> bool`, `kv_cas(key, expected, new) -> bool`.
- `kv_incr(key, delta: i64) -> i64` (integer-guarded; serves both `incr` and `decr`).

### The two Lua-in-transaction sites

- **`db.tx`**: `db.rs` obtains `Transaction` from `backend.begin(&Lua)`, wraps it in the
  existing `Arc<Mutex<Option<Transaction>>>`, builds the Lua `tx` table whose `exec`/`query`
  call through to it, then `commit` on normal return / `rollback` on error — same control
  flow as today, only the concrete pinned-connection type is now the backend's
  `Transaction`.
- **`kv.update`**: the backend exposes a purpose-built
  `kv_update(&Lua, key, func: Function) -> Value` that owns the whole sequence — begin →
  type-aware read → call the Lua transform `func` → write/delete → commit / rollback — so
  the kv dialect SQL never leaks into `kv.rs`. `kv.rs` keeps the `IN_KV_UPDATE`
  reentrancy guard (`reject_kv_reentry`) set around this single call; because the transform
  only runs inside `kv_update`, guarding the whole call is behaviorally equivalent to
  today's narrower guard. This is the only seam method that must accept an `mlua::Function`.

## kv logical value model (backend-neutral) — load-bearing commitment

The kv logical value model is exactly two cases: **opaque bytes** (from `set`/`add`/`cas`)
and **integer counter** (from `incr`/`decr`, read back as a decimal string). This model is
backend-neutral and defined at the seam — both backends MUST expose identical
Lua-visible semantics for it (parity requires this regardless).

Consequence for future migration: a one-way SQLite→PostgreSQL migration, if ever needed,
goes **through the seam** (read via one `Backend`, write via another) — never via raw table
copies or schema-transformation SQL. Nothing is built for this now (YAGNI), but the seam
makes a future migration a small tool rather than a fiddly transform. Phase 2 MUST choose a
PostgreSQL kv schema that maps 1:1 onto this logical model (e.g. a `kind` discriminator +
value column) so the round-trip stays trivial; it MUST NOT harmonize by altering the
SQLite schema (that would regress the shipped native-integer counter path and impose a
migration on current SQLite users).

## Testing

- **Primary acceptance = zero behavior change.** The entire existing suite — especially
  `tests/db.rs` (exec/query/tx, kv set/get/delete, atomic add/cas/incr/decr/update, the
  4-writer concurrency guards, type-aware reads) — passes **unchanged, not one line
  edited**. Any required test edit is a signal the refactor changed behavior and must be
  reworked.
- The `is_busy` unit test moves with its code into `storage/sqlite.rs` and still passes.
- **Perf gate (per CLAUDE.md):** this touches the exec/query/kv hot path. Capture
  `cargo bench --bench runtime` before and after; no regression may be committed. The seam
  adds one enum `match` per call and preserves direct table building (no new allocation), so
  the expectation is flat.

## Risks

- A single-variant enum may trip a clippy lint under `-D warnings`. If so, annotate with a
  commented `#[allow(...)]` noting the `Postgres` variant lands in Phase 2 — do not add
  filler structure to appease the lint.
- Moving the pinned-connection transaction and the `kv_update` Lua-callback orchestration
  behind the seam is the highest-risk part; the unchanged `tx` and `kv.update` tests are the
  guard.

## Deliverable

An internal refactor that ships no user feature. Its value is a clean boundary and a seam
shaped so Phase 2 (PostgreSQL) is additive behind the enum. The user has agreed to the
two-phase split on that basis.
