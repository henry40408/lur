# Transaction Cancellation Safety — Design

**Date:** 2026-07-02
**Status:** Approved
**Scope:** Fix two cancellation-during-transform resource leaks surfaced by the
Phase-1/Phase-2 storage reviews (backlog items 0 and 0b).

## Problem

`lur`'s wall-clock timeout layer (`tokio::time::timeout(d, fut)` in
`runtime.rs` one-shot `guarded` and `serve.rs` `call_handler`) cancels a
handler by dropping its entire future tree. When the future is dropped while a
pinned resource is mid-cleanup, Rust's `Drop` cannot run the *async* cleanup
that the resource needs. Two such leaks exist, both in the same
cancellation-during-transform family:

### Item 0 — `IN_KV_UPDATE` thread-local poison

`kv.rs` guards a `kv.update` transform with a thread-local flag so a nested
`lur.kv`/`lur.db` call raises a clear error instead of deadlocking on the
pinned transaction connection:

```rust
IN_KV_UPDATE.with(|f| f.set(true));
let r = func.call_async::<Value>(cur).await;   // cancelled here
IN_KV_UPDATE.with(|f| f.set(false));           // never runs
```

If the future is dropped while parked in `func.call_async`, `set(false)` never
runs. The flag stays `true` on that worker thread, and every later
`lur.kv`/`lur.db` call on the same pooled VM is spuriously rejected as
re-entry. Pre-existing; the Phase-1 seam narrowed but did not close the window.

### Item 0b — pinned connection left mid-transaction

`db.tx` (`db.rs::run_tx`) and `kv.update` (`sqlite.rs::kv_update`,
`postgres.rs::kv_update`) acquire a pooled connection, issue a raw `BEGIN`
(`BEGIN IMMEDIATE` on SQLite, `BEGIN ISOLATION LEVEL SERIALIZABLE` on
Postgres), run the user body/transform, then explicitly `COMMIT`/`ROLLBACK`.

`sqlx` only auto-rolls-back a transaction it opened through its own `.begin()`
API (it tracks tx depth there). A **manually-issued** `BEGIN` is invisible to
that tracking, so if the future is dropped while parked in the user
body/transform, the `PoolConnection` returns to the pool **still inside an open
transaction**.

- **SQLite:** the connection holds a `BEGIN IMMEDIATE` write lock and returns
  to the pool mid-transaction — the next acquirer of that connection sees a
  stale open transaction.
- **Postgres (worse):** the connection holds a SERIALIZABLE snapshot + row
  locks against the *shared operator database*, blocking other writers and
  autovacuum until the connection is next reused (or indefinitely under pool
  churn) — exactly the shared-DB threat model SERIALIZABLE was chosen for.

Additionally, `run_tx` holds its connection in the `SqliteTransaction`/
`PgTransaction` type, but `kv_update` holds a **bare `conn` local** — both
paths leak and both must be fixed.

## Non-goals

- **No `?`→`$n` or dialect changes.** This is a cancellation-safety fix only.
- **No server-side timeouts** (`idle_in_transaction_session_timeout`,
  `statement_timeout`, `lock_timeout`). Rejected as the primary fix: PG-only,
  and they would also reap a *legitimate slow* transform (the pinned connection
  is legitimately idle-in-transaction while the Lua transform awaits, e.g. a
  valid slow `lur.http`). Defense-in-depth timeouts are left to a future change
  if a real need arises (YAGNI).
- **Minor #4 (nested `lur.db` write inside a transform) is not fully solved.**
  The fix converts the PG client-side-deadlock hang into a clean timeout *when
  `--timeout` is set* (drop → rollback → locks released); with no `--timeout`
  it still hangs. Transform-must-not-write stays a documented contract.

## Approach (Design C)

A **rollback-on-drop guard** on the connection-owning types, plus a synchronous
RAII guard for the thread-local flag. Chosen over:

- **Server-side timeouts (A):** PG-only; false-positives on slow transforms.
- **Routing through sqlx's transaction API (B):** idiomatic but regresses the
  deliberate concurrency design — sqlx's `.begin()` issues a *deferred* `BEGIN`,
  losing SQLite's `BEGIN IMMEDIATE` (upfront write lock, paired with
  `retry_busy`); PG SERIALIZABLE would need a post-hoc `SET TRANSACTION`.

Design C preserves the existing isolation choices, fixes both backends
uniformly, keeps all changes inside the `storage` module, and adds **zero
overhead on the normal path** (the guard is a no-op once `commit`/`rollback`
has taken the connection).

### Component 1 — `IN_KV_UPDATE` RAII guard (`kv.rs`)

Replace the manual `set(true)`/`set(false)` with a synchronous RAII guard whose
`Drop` restores the previous flag value:

```rust
struct KvUpdateGuard(bool);

impl KvUpdateGuard {
    fn enter() -> Self {
        KvUpdateGuard(IN_KV_UPDATE.replace(true))
    }
}
impl Drop for KvUpdateGuard {
    fn drop(&mut self) {
        IN_KV_UPDATE.with(|f| f.set(self.0));
    }
}
```

`wrapped` holds `let _g = KvUpdateGuard::enter();` around the transform call.
Normal return, error, and cancellation all restore the flag via `Drop`.
Restoring the *previous* value (not a hard `false`) is correct even though
`reject_kv_reentry` currently forbids nesting — it makes the guard
composition-safe by construction. Cleanup is fully synchronous, so `Drop`
handles it completely.

### Component 2 — rollback-on-drop connection guard (`sqlite.rs`, `postgres.rs`)

Add a `Drop` impl to `SqliteTransaction` and `PgTransaction`. If the connection
is still present (transaction unfinished), best-effort roll it back so it does
not return to the pool mid-transaction:

```rust
impl Drop for SqliteTransaction {          // PgTransaction identical
    fn drop(&mut self) {
        if let Some(conn) = self.conn.get_mut().take() {   // None once finished → no-op
            match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    h.spawn(async move {
                        let mut conn = conn;
                        let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                    });
                }
                Err(_) => {
                    // No runtime (not reachable in practice): close the
                    // connection instead of returning it to the pool, so the
                    // lock/snapshot is released. PG rolls back on disconnect;
                    // SQLite releases on file close.
                    drop(conn.detach());
                }
            }
        }
    }
}
```

- `tokio::sync::Mutex::get_mut()` is synchronous under `&mut self` (no await),
  so it is usable in `Drop`.
- `PoolConnection` is `Send + 'static`, so it moves into the spawned task.
- Only the rare "cancelled while transaction open" path spawns a rollback; the
  normal path already took the connection in `commit`/`rollback`, so `Drop`
  sees `None`.

### Component 3 — `kv_update` shares the guard

`kv_update` in both backends currently holds a bare `conn` local. Refactor it
to hold the pinned connection inside the same guard type used by `begin()`, so
its cancellation path is covered by Component 2's `Drop`:

- read → `func.call_async` → write → `COMMIT`; on success take the connection
  out of the guard so `Drop` is a no-op.
- any explicit error path keeps the existing `ROLLBACK` then returns the
  original error.
- if the future is cancelled inside the transform, the guard's `Drop` performs
  the rollback.

`db.tx` and `kv.update` thereby share one cleanup mechanism per backend.

## Error handling & edges

- **Normal path unchanged:** `commit`/`rollback` take the connection exactly as
  today; the guard acts only when a connection is still held.
- **Detached rollback failure:** best-effort (`let _ =`). If the rollback fails
  (broken connection), the connection drops with the task and closes — the lock
  is released regardless, no worse than today.
- **One-shot boundary:** a one-shot run that times out inside a pinned
  transaction may drop before the spawned rollback completes, but the process
  then exits — PG rolls back on disconnect, SQLite releases on close. The
  server mode that actually matters keeps a long-lived runtime, so the rollback
  always runs.

## Testing

- **Item 0 (deterministic, `kv.rs` inline unit test):** enter `KvUpdateGuard`,
  cancel the guard-holding future via `tokio::time::timeout(Duration::ZERO, …)`
  (or drop it explicitly) while it is parked, then assert the thread-local
  `IN_KV_UPDATE` has been restored to `false`.
- **Item 0b — SQLite (deterministic, no external PG), `tests/`:** open `--db`
  on a temp file with pool `max_connections = 1` (forces connection reuse); run
  a `db.tx` whose body writes a row then parks on a long await, wrapped in a
  short `--timeout` so it is cancelled mid-body; after cancellation run a second
  transaction on the same pool and assert (a) it commits and (b) the first row
  is absent (rolled back). Add a `kv.update` variant.
- **Item 0b — Postgres (`tests/pg.rs`, connect-or-skip):** same shape with
  `max_connections = 1`; after cancellation a new transaction must succeed and
  observe rolled-back state, proving the connection is not idle-in-transaction.
  Runs under the existing `.config/nextest.toml` pg serialization group.

## Files touched

- `src/capabilities/kv.rs` — Component 1 guard + inline unit test.
- `src/capabilities/storage/sqlite.rs` — Component 2 `Drop` + Component 3
  `kv_update` refactor.
- `src/capabilities/storage/postgres.rs` — Component 2 `Drop` + Component 3
  `kv_update` refactor.
- `tests/` — SQLite cancellation regression tests (`db.tx` + `kv.update`).
- `tests/pg.rs` — PG cancellation regression test.
- `ARCHITECTURE.md` — note the rollback-on-drop cleanup invariant.
