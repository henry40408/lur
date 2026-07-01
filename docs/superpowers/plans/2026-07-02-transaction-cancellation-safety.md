# Transaction Cancellation Safety Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two cancellation-during-transform resource leaks (backlog items 0 and 0b) — the `IN_KV_UPDATE` thread-local poison and the pinned connection left mid-transaction — with a rollback-on-drop guard plus a synchronous RAII flag guard.

**Architecture:** When the wall-clock timeout (`tokio::time::timeout`) drops a handler future mid-await, Rust's `Drop` cannot run async cleanup. Component 1 replaces the manual flag set/clear with a synchronous RAII guard whose `Drop` restores the flag. Components 2–3 give the connection-owning types (`SqliteTransaction`, `PgTransaction`, and a new `PinnedTx` used by `kv_update`) a `Drop` that best-effort rolls back an unfinished transaction via a detached task, so the pooled connection never returns idle-in-transaction. The existing `BEGIN IMMEDIATE` / `BEGIN ISOLATION LEVEL SERIALIZABLE` isolation choices are preserved; all changes stay inside the `storage` module and `kv.rs`.

**Tech Stack:** Rust (edition 2024), mlua 0.11 (luau/async), sqlx 0.9 (sqlite + postgres), tokio.

## Global Constraints

- All commits MUST be GPG-signed. Never pass `--no-gpg-sign`.
- Stage files explicitly by name. Never use `git add -A` / `git add .`.
- Run `cargo fmt --all` before every commit.
- Tests run with `cargo nextest run`, never `cargo test`.
- Lint gate: `cargo clippy --all-targets -- -D warnings` must pass.
- Unit tests live inline in each module (`#[cfg(test)] mod tests`); this plan adds no new `tests/` files.
- Commit messages end with the trailer: `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- Do NOT change SQL dialect, placeholders, isolation levels, or any user-facing behavior. This is a cancellation-safety fix only.
- Do NOT add server-side timeouts (`idle_in_transaction_session_timeout`, `statement_timeout`, `lock_timeout`).

---

### Task 1: `IN_KV_UPDATE` RAII guard (Component 1)

Replace the manual `set(true)`/`set(false)` around the `kv.update` transform with an RAII guard whose `Drop` restores the previous flag value, so a cancelled transform no longer poisons later kv/db calls.

**Files:**
- Modify: `src/capabilities/kv.rs` (guard type near the `IN_KV_UPDATE` thread-local ~lines 13-27; `wrapped` closure ~lines 160-168; add inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: the existing `IN_KV_UPDATE` thread-local (`Cell<bool>`) and `reject_kv_reentry`.
- Produces: `struct KvUpdateGuard(bool)` with `KvUpdateGuard::enter() -> Self` (private to `kv.rs`).

- [ ] **Step 1: Write the failing test**

Add to the bottom of `src/capabilities/kv.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // A guard entered inside a future that is then cancelled mid-await must
    // still restore IN_KV_UPDATE to false — otherwise the flag poisons every
    // later kv/db call on the pooled VM (backlog item 0).
    #[test]
    fn kv_update_guard_restores_flag_on_cancellation() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert!(!IN_KV_UPDATE.with(Cell::get), "flag starts clear");
            let parked = async {
                let _guard = KvUpdateGuard::enter();
                assert!(IN_KV_UPDATE.with(Cell::get), "flag set inside guard");
                std::future::pending::<()>().await;
            };
            // The zero-duration timeout polls `parked` once (entering the guard),
            // then fires and drops it while parked — exactly the cancellation path.
            let _ = tokio::time::timeout(Duration::ZERO, parked).await;
            assert!(
                !IN_KV_UPDATE.with(Cell::get),
                "flag must be restored after the guarded future is cancelled"
            );
        });
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -E 'test(kv_update_guard_restores_flag_on_cancellation)'`
Expected: FAIL to compile — `KvUpdateGuard` does not exist yet.

- [ ] **Step 3: Add the guard type**

Insert into `src/capabilities/kv.rs` immediately after the `reject_kv_reentry` function (after line 27):

```rust
/// RAII guard for `IN_KV_UPDATE`. `enter` sets the flag and remembers the prior
/// value; `Drop` restores it — so a transform that returns, errors, or is
/// cancelled (its future dropped mid-await) always restores the flag instead of
/// leaving it stuck `true` and poisoning later kv/db calls on the pooled VM.
struct KvUpdateGuard(bool);

impl KvUpdateGuard {
    fn enter() -> Self {
        KvUpdateGuard(IN_KV_UPDATE.with(|f| f.replace(true)))
    }
}

impl Drop for KvUpdateGuard {
    fn drop(&mut self) {
        IN_KV_UPDATE.with(|f| f.set(self.0));
    }
}
```

- [ ] **Step 4: Wire the guard into `wrapped`**

In `src/capabilities/kv.rs`, replace the `wrapped` closure body (the `async move` block currently at lines 160-167):

```rust
                    let wrapped = lua.create_async_function(move |_, cur: Value| {
                        let func = func.clone();
                        async move {
                            IN_KV_UPDATE.with(|f| f.set(true));
                            let r = func.call_async::<Value>(cur).await;
                            IN_KV_UPDATE.with(|f| f.set(false));
                            r
                        }
                    })?;
```

with:

```rust
                    let wrapped = lua.create_async_function(move |_, cur: Value| {
                        let func = func.clone();
                        async move {
                            // Guard restores IN_KV_UPDATE on every exit path,
                            // including cancellation (future dropped mid-await).
                            let _guard = KvUpdateGuard::enter();
                            func.call_async::<Value>(cur).await
                        }
                    })?;
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo nextest run -E 'test(kv_update_guard_restores_flag_on_cancellation)'`
Expected: PASS.

- [ ] **Step 6: Full gate**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run`
Expected: fmt clean, clippy clean, all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/capabilities/kv.rs
git commit -S -m "fix(kv): restore IN_KV_UPDATE via RAII guard on cancellation

A kv.update transform cancelled mid-await left IN_KV_UPDATE stuck true,
poisoning later kv/db calls on the pooled VM. Replace the manual
set(true)/set(false) with an RAII guard whose Drop restores the prior
value on every exit path (return, error, cancellation).

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: SQLite rollback-on-drop guard (Components 2 & 3)

Give `SqliteTransaction` a `Drop` that rolls back an unfinished transaction, and refactor `kv_update` to hold its pinned connection in a new `PinnedTx` guard with the same `Drop`, so a cancelled `db.tx` body or `kv.update` transform never leaves the pooled connection inside an open `BEGIN IMMEDIATE`.

**Files:**
- Modify: `src/capabilities/storage/sqlite.rs` (add `spawn_rollback` + `PinnedTx` before `SqliteTransaction` ~line 495; add `impl Drop for SqliteTransaction`; refactor `kv_update` ~lines 419-492; add tests to the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `SqliteBackend { pool }`, `retry_busy`, `value_to_bytes`, `bind_all`.
- Produces (private to `sqlite.rs`): `fn spawn_rollback(conn: PoolConnection<Sqlite>)`; `struct PinnedTx { conn: Option<PoolConnection<Sqlite>> }` with `PinnedTx::new(conn)`, `fn conn(&mut self) -> &mut PoolConnection<Sqlite>`, `fn disarm(self)`.

- [ ] **Step 1: Write the failing tests**

Add these two tests and the helper inside the existing `#[cfg(test)] mod tests` in `src/capabilities/storage/sqlite.rs` (`Lua`/`Value` are already in scope via that module's `use super::*;`):

```rust
    // A single-connection pool so the second acquire is forced onto the SAME
    // connection the cancelled transaction used — the only way to observe a
    // connection returned to the pool mid-transaction.
    async fn max1_backend(dir: &std::path::Path) -> SqliteBackend {
        let opts = SqliteConnectOptions::new()
            .filename(dir.join("cancel.db"))
            .create_if_missing(true)
            .busy_timeout(std::time::Duration::from_millis(200))
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS lur_kv (key TEXT PRIMARY KEY, value BLOB)")
            .execute(&pool)
            .await
            .unwrap();
        SqliteBackend { pool }
    }

    // Dropping an unfinished SqliteTransaction (as cancellation does) must roll
    // it back: the write is undone and — critically — the sole pooled connection
    // is usable for a fresh transaction rather than stuck in an open BEGIN.
    #[test]
    fn sqlite_dropped_tx_rolls_back() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let backend = max1_backend(dir.path()).await;
            let lua = Lua::new();

            let tx = backend.begin().await.unwrap();
            tx.exec(
                &lua,
                "INSERT INTO lur_kv (key, value) VALUES ('k', 'v')".to_string(),
                vec![],
            )
            .await
            .unwrap();
            drop(tx); // simulate a future cancelled mid-transaction

            // With max_connections=1 this begin can only acquire the sole
            // connection after the detached rollback has released it — natural
            // synchronization, no sleep. On the unfixed code the connection
            // returns to the pool inside an open BEGIN and this begin errors.
            let tx2 = backend
                .begin()
                .await
                .expect("second begin must succeed after the dropped tx rolled back");
            let rows = tx2
                .query(
                    &lua,
                    "SELECT value FROM lur_kv WHERE key = 'k'".to_string(),
                    vec![],
                )
                .await
                .unwrap();
            assert_eq!(rows.raw_len(), 0, "row from the cancelled tx must be rolled back");
            tx2.rollback().await;
        });
    }

    // A kv.update whose transform is cancelled mid-flight must roll back its
    // pinned connection, leaving it reusable (a fresh begin succeeds).
    #[test]
    fn sqlite_cancelled_kv_update_rolls_back() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let backend = max1_backend(dir.path()).await;
            let lua = Lua::new();

            // Transform signals when entered, then parks forever, so we cancel
            // the update exactly mid-transform.
            let entered = std::sync::Arc::new(tokio::sync::Notify::new());
            let entered2 = entered.clone();
            let parking = lua
                .create_async_function(move |_, _cur: Value| {
                    let entered2 = entered2.clone();
                    async move {
                        entered2.notify_one();
                        std::future::pending::<Value>().await
                    }
                })
                .unwrap();

            let mut fut = Box::pin(backend.kv_update(&lua, "k".to_string(), parking));
            tokio::select! {
                _ = &mut fut => panic!("kv_update should park in the transform"),
                _ = entered.notified() => {}
            }
            drop(fut); // cancel mid-transform → PinnedTx::drop rolls back

            // kv_get acquires the sole connection, so it blocks until the
            // detached rollback frees it; on the unfixed code the fresh begin
            // below errors (connection stuck in BEGIN IMMEDIATE).
            let got = backend.kv_get(&lua, "k".to_string()).await.unwrap();
            assert_eq!(got, Value::Nil, "cancelled update must not commit");
            let tx = backend
                .begin()
                .await
                .expect("connection must be reusable after a cancelled update");
            tx.rollback().await;
        });
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(sqlite_dropped_tx_rolls_back) + test(sqlite_cancelled_kv_update_rolls_back)'`
Expected: FAIL to compile — `PinnedTx` does not exist yet (used by the refactored `kv_update`), and/or the tests panic on the `.expect(...)` because the connection is returned mid-transaction.

- [ ] **Step 3: Add `spawn_rollback` and `PinnedTx`**

Insert into `src/capabilities/storage/sqlite.rs` immediately before the `SqliteTransaction` struct definition (before line 495, `/// A pinned-connection SQLite write transaction.`):

```rust
/// Best-effort rollback of a pinned connection whose transaction is still open
/// because the enclosing future was cancelled before COMMIT/ROLLBACK. The
/// rollback is detached onto the runtime so the connection returns to the pool
/// clean instead of inside an open `BEGIN IMMEDIATE`. With no runtime (not
/// reached in practice — the storage APIs always run inside one) the connection
/// is closed instead, which also releases the write lock.
fn spawn_rollback(conn: PoolConnection<Sqlite>) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let mut conn = conn;
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            });
        }
        Err(_) => {
            drop(conn.detach());
        }
    }
}

/// Owns a pinned connection with a manually-opened transaction. Dropping it
/// while the transaction is still open (e.g. a `kv.update` future cancelled
/// mid-transform) best-effort rolls back via `spawn_rollback`. `disarm` takes
/// the connection back after an explicit COMMIT/ROLLBACK so `Drop` is a no-op.
struct PinnedTx {
    conn: Option<PoolConnection<Sqlite>>,
}

impl PinnedTx {
    fn new(conn: PoolConnection<Sqlite>) -> Self {
        Self { conn: Some(conn) }
    }

    /// Exclusive access to the pinned connection (present until `disarm`).
    fn conn(&mut self) -> &mut PoolConnection<Sqlite> {
        self.conn.as_mut().expect("connection present until disarm")
    }

    /// Disarm the rollback-on-drop guard after an explicit COMMIT/ROLLBACK.
    fn disarm(mut self) {
        self.conn = None;
    }
}

impl Drop for PinnedTx {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            spawn_rollback(conn);
        }
    }
}
```

- [ ] **Step 4: Add `Drop` for `SqliteTransaction`**

Append to `src/capabilities/storage/sqlite.rs`, immediately after the closing brace of `impl SqliteTransaction { ... }` (after line 560):

```rust
impl Drop for SqliteTransaction {
    /// If the transaction was never committed/rolled back — its future was
    /// cancelled mid-body — best-effort roll it back so the pinned connection
    /// does not return to the pool inside an open `BEGIN IMMEDIATE`.
    fn drop(&mut self) {
        if let Some(conn) = self.conn.get_mut().take() {
            spawn_rollback(conn);
        }
    }
}
```

- [ ] **Step 5: Refactor `kv_update` to use `PinnedTx`**

In `src/capabilities/storage/sqlite.rs`, replace the entire body of `kv_update` (lines 419-492, from `let mut conn = retry_busy(...)` through the final `match result { ... }`) with:

```rust
        let conn = retry_busy(|| async {
            let mut conn = self.pool.acquire().await?;
            sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
            Ok(conn)
        })
        .await
        .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        let mut tx = PinnedTx::new(conn);

        // Run the full read → transform → write sequence. If the enclosing
        // future is cancelled during `func`, `tx` drops and rolls back.
        let result: mlua::Result<Value> = async {
            let cur: Value = match sqlx::query("SELECT value FROM lur_kv WHERE key = ?")
                .bind(&key)
                .fetch_optional(&mut **tx.conn())
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?
            {
                None => Value::Nil,
                Some(r) => match value_to_bytes(&r)? {
                    None => Value::Nil,
                    Some(bytes) => Value::String(lua.create_string(bytes)?),
                },
            };

            let new = func.call_async::<Value>(cur).await?;

            match &new {
                Value::Nil => {
                    sqlx::query("DELETE FROM lur_kv WHERE key = ?")
                        .bind(&key)
                        .execute(&mut **tx.conn())
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                Value::String(s) => {
                    sqlx::query("INSERT OR REPLACE INTO lur_kv (key, value) VALUES (?, ?)")
                        .bind(&key)
                        .bind(s.as_bytes().to_vec())
                        .execute(&mut **tx.conn())
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                other => {
                    return Err(Error::runtime(format!(
                        "lur.kv.update: transform must return a string or nil, got {}",
                        other.type_name()
                    )));
                }
            }
            sqlx::query("COMMIT")
                .execute(&mut **tx.conn())
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: commit: {e}")))?;
            Ok(new)
        }
        .await;

        match result {
            Ok(v) => {
                tx.disarm();
                Ok(v)
            }
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut **tx.conn()).await;
                tx.disarm();
                Err(e)
            }
        }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo nextest run -E 'test(sqlite_dropped_tx_rolls_back) + test(sqlite_cancelled_kv_update_rolls_back)'`
Expected: PASS.

- [ ] **Step 7: Full gate**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run`
Expected: fmt clean, clippy clean, all tests pass (existing `is_busy_classifies_sqlite_lock_errors`, kv/db suites, and the two new tests).

- [ ] **Step 8: Commit**

```bash
git add src/capabilities/storage/sqlite.rs
git commit -S -m "fix(storage): roll back pinned SQLite tx on cancellation

A db.tx body or kv.update transform cancelled mid-flight dropped the
pinned connection inside an open BEGIN IMMEDIATE, returning it to the
pool mid-transaction. Add a rollback-on-drop guard (Drop for
SqliteTransaction + a PinnedTx wrapper for kv_update) that best-effort
rolls back via a detached task so the connection returns clean.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Postgres rollback-on-drop guard + docs (Components 2 & 3)

Mirror Task 2 for the Postgres backend (where the leak is worst — an idle-in-transaction connection holds a SERIALIZABLE snapshot + row locks on the shared operator DB), add a deterministic PG regression test under the pg serialization group, and document the invariant.

**Files:**
- Modify: `src/capabilities/storage/postgres.rs` (add `spawn_rollback` + `PinnedTx` before `PgTransaction` ~line 424; add `impl Drop for PgTransaction`; refactor `kv_update` ~lines 348-418; add `#[cfg(test)] mod tests`)
- Modify: `.config/nextest.toml` (add the inline PG test to the `pg-serial` group)
- Modify: `ARCHITECTURE.md` (add the cancellation-safety invariant after line 248)

**Interfaces:**
- Consumes: `PgBackend { pool }`, `bind_all`, `kv_row_to_bytes`, `PgConnectOptions`, `PgPoolOptions`.
- Produces (private to `postgres.rs`): `fn spawn_rollback(conn: PoolConnection<Postgres>)`; `struct PinnedTx { conn: Option<PoolConnection<Postgres>> }` with `PinnedTx::new`, `conn`, `disarm` (same shape as the SQLite one).

- [ ] **Step 1: Write the failing test**

Add a `#[cfg(test)] mod tests` at the bottom of `src/capabilities/storage/postgres.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Uniquely-named scratch table per test so a leaked/parallel transaction
    // never collides on a shared name.
    fn unique_table() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        format!("lur_cancel_{}", N.fetch_add(1, Ordering::Relaxed))
    }

    // A single-connection PG pool, or None when Postgres is unreachable. Locally
    // an unreachable server SKIPS; under CI it is a hard failure (CI provisions
    // the service). max_connections=1 forces the post-cancellation query onto
    // the same connection the cancelled transaction used.
    async fn pg_max1() -> Option<PgBackend> {
        use std::time::Duration;
        let url = std::env::var("LUR_TEST_PG_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".to_string());
        let Ok(opts) = PgConnectOptions::from_str(&url) else {
            return None;
        };
        let pool = match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect_with(opts)
            .await
        {
            Ok(p) => p,
            Err(_) => {
                if std::env::var("CI").is_ok() {
                    panic!("CI: Postgres unreachable but CI must provision it");
                }
                eprintln!("skipping PG test: Postgres unreachable (start it: docker compose up -d)");
                return None;
            }
        };
        Some(PgBackend { pool })
    }

    // Dropping an unfinished PgTransaction (as cancellation does) must roll it
    // back so the pinned connection is not returned to the pool holding a
    // SERIALIZABLE snapshot + row locks. Detected via row visibility on a
    // single-connection pool: the written row must be gone afterward.
    #[test]
    fn pg_dropped_tx_rolls_back() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let Some(backend) = pg_max1().await else { return };
            let lua = Lua::new();
            let t = unique_table();

            // Idempotent across reruns (a failed pre-fix run may leave the table).
            backend
                .exec(&lua, format!("DROP TABLE IF EXISTS {t}"), vec![])
                .await
                .unwrap();
            backend
                .exec(&lua, format!("CREATE TABLE {t} (x INT)"), vec![])
                .await
                .unwrap();

            let tx = backend.begin().await.unwrap();
            tx.exec(&lua, format!("INSERT INTO {t} (x) VALUES (1)"), vec![])
                .await
                .unwrap();
            drop(tx); // simulate a future cancelled mid-transaction

            // On the fixed code the detached rollback frees the connection before
            // this query can acquire it, and the row is gone. On the unfixed code
            // the connection returns to the pool inside the open transaction, so
            // the query runs inside it and still sees the uncommitted row.
            let rows = backend
                .query(&lua, format!("SELECT x FROM {t}"), vec![])
                .await
                .unwrap();
            assert_eq!(rows.raw_len(), 0, "row from the cancelled tx must be rolled back");

            backend
                .exec(&lua, format!("DROP TABLE {t}"), vec![])
                .await
                .unwrap();
        });
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -E 'test(pg_dropped_tx_rolls_back)'` (requires a reachable Postgres — `docker compose up -d`)
Expected: FAIL — either the assertion (`raw_len()` is 1: the query saw the uncommitted row inside the leaked transaction) or a compile error until the code below is in place. If Postgres is not running locally, the test is SKIPPED (returns early); start it before relying on this step.

- [ ] **Step 3: Add `spawn_rollback` and `PinnedTx`**

Insert into `src/capabilities/storage/postgres.rs` immediately before the `PgTransaction` struct definition (before line 424, `/// A pinned-connection Postgres write transaction.`):

```rust
/// Best-effort rollback of a pinned connection whose transaction is still open
/// because the enclosing future was cancelled before COMMIT/ROLLBACK. The
/// rollback is detached onto the runtime so the connection returns to the pool
/// clean instead of idle-in-transaction (which on Postgres would hold a
/// SERIALIZABLE snapshot + row locks on the shared database). With no runtime
/// (not reached in practice) the connection is closed, which also releases them.
fn spawn_rollback(conn: PoolConnection<Postgres>) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let mut conn = conn;
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            });
        }
        Err(_) => {
            drop(conn.detach());
        }
    }
}

/// Owns a pinned connection with a manually-opened transaction. Dropping it
/// while the transaction is still open (e.g. a `kv.update` future cancelled
/// mid-transform) best-effort rolls back via `spawn_rollback`. `disarm` takes
/// the connection back after an explicit COMMIT/ROLLBACK so `Drop` is a no-op.
struct PinnedTx {
    conn: Option<PoolConnection<Postgres>>,
}

impl PinnedTx {
    fn new(conn: PoolConnection<Postgres>) -> Self {
        Self { conn: Some(conn) }
    }

    /// Exclusive access to the pinned connection (present until `disarm`).
    fn conn(&mut self) -> &mut PoolConnection<Postgres> {
        self.conn.as_mut().expect("connection present until disarm")
    }

    /// Disarm the rollback-on-drop guard after an explicit COMMIT/ROLLBACK.
    fn disarm(mut self) {
        self.conn = None;
    }
}

impl Drop for PinnedTx {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            spawn_rollback(conn);
        }
    }
}
```

- [ ] **Step 4: Add `Drop` for `PgTransaction`**

Append to `src/capabilities/storage/postgres.rs`, immediately after the closing brace of `impl PgTransaction { ... }` (after line 487):

```rust
impl Drop for PgTransaction {
    /// If the transaction was never committed/rolled back — its future was
    /// cancelled mid-body — best-effort roll it back so the pinned connection
    /// does not return to the pool idle-in-transaction, holding a SERIALIZABLE
    /// snapshot + row locks on the shared operator database.
    fn drop(&mut self) {
        if let Some(conn) = self.conn.get_mut().take() {
            spawn_rollback(conn);
        }
    }
}
```

- [ ] **Step 5: Refactor `kv_update` to use `PinnedTx`**

In `src/capabilities/storage/postgres.rs`, replace the entire body of `kv_update` (lines 354-417, from `let mut conn = self.pool.acquire()...` through the final `match result { ... }`) with:

```rust
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        let mut tx = PinnedTx::new(conn);
        sqlx::query("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut **tx.conn())
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;

        // Run the full read → transform → write sequence. If the enclosing
        // future is cancelled during `func`, `tx` drops and rolls back.
        let result: mlua::Result<Value> = async {
            let cur: Value = match sqlx::query("SELECT kind, bytes, num FROM lur_kv WHERE key = $1")
                .bind(&key)
                .fetch_optional(&mut **tx.conn())
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?
            {
                None => Value::Nil,
                Some(r) => Value::String(lua.create_string(kv_row_to_bytes(&r)?)?),
            };

            let new = func.call_async::<Value>(cur).await?;

            match &new {
                Value::Nil => {
                    sqlx::query("DELETE FROM lur_kv WHERE key = $1")
                        .bind(&key)
                        .execute(&mut **tx.conn())
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                Value::String(s) => {
                    sqlx::query(
                        "INSERT INTO lur_kv (key, kind, bytes, num) VALUES ($1, 0, $2, NULL) \
                         ON CONFLICT (key) DO UPDATE SET kind = 0, bytes = excluded.bytes, num = NULL",
                    )
                    .bind(&key)
                    .bind(s.as_bytes().to_vec())
                    .execute(&mut **tx.conn())
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                other => {
                    return Err(Error::runtime(format!(
                        "lur.kv.update: transform must return a string or nil, got {}",
                        other.type_name()
                    )));
                }
            }
            sqlx::query("COMMIT")
                .execute(&mut **tx.conn())
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: commit: {e}")))?;
            Ok(new)
        }
        .await;

        match result {
            Ok(v) => {
                tx.disarm();
                Ok(v)
            }
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut **tx.conn()).await;
                tx.disarm();
                Err(e)
            }
        }
```

- [ ] **Step 6: Add the inline PG test to the pg serialization group**

The new test runs in the main `lur` test binary, not `lur::pg`, so extend `.config/nextest.toml` to keep it in the single-threaded `pg-serial` group (avoids SERIALIZABLE cross-test false-positive `40001`s against the shared `lur_kv`). Replace the `filter` line:

```toml
filter = "binary_id(lur::pg)"
```

with:

```toml
filter = "binary_id(lur::pg) or test(pg_dropped_tx_rolls_back)"
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo nextest run -E 'test(pg_dropped_tx_rolls_back)'` (Postgres running)
Expected: PASS. Also confirm it is grouped: `cargo nextest list --message-format json 2>/dev/null | rg pg_dropped_tx_rolls_back` shows the test; the run must not raise a spurious `40001`.

- [ ] **Step 8: Document the invariant**

In `ARCHITECTURE.md`, insert a new bullet immediately after the isolation-model bullet (after line 248, the `The retry_busy/busy_timeout layer ... stays SQLite-only.` line):

```markdown
- **Cancellation-safe pinned transactions:** `db.tx` and `kv.update` run the
  user body/transform on a pinned connection inside a manually-opened
  transaction (`BEGIN IMMEDIATE` on SQLite, `BEGIN ISOLATION LEVEL SERIALIZABLE`
  on Postgres), which `sqlx` does not auto-roll-back. If the wall-clock timeout
  drops that future mid-transform, the connection-owning guard
  (`SqliteTransaction`/`PgTransaction` and the `PinnedTx` used by `kv_update`)
  rolls back on `Drop` via a detached task, so the connection never returns to
  the pool mid-transaction — on Postgres it would otherwise sit
  idle-in-transaction holding a SERIALIZABLE snapshot + row locks on the shared
  database. An explicit COMMIT/ROLLBACK disarms the guard, so the normal path
  costs nothing.
```

- [ ] **Step 9: Full gate**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run` (Postgres running so the PG test executes, not skips)
Expected: fmt clean, clippy clean, all tests pass including `pg_dropped_tx_rolls_back`, and no `40001` from cross-test interference.

- [ ] **Step 10: Commit**

```bash
git add src/capabilities/storage/postgres.rs .config/nextest.toml ARCHITECTURE.md
git commit -S -m "fix(storage): roll back pinned Postgres tx on cancellation

Mirror the SQLite cancellation-safety fix for Postgres, where a
cancelled db.tx/kv.update left the connection idle-in-transaction holding
a SERIALIZABLE snapshot + row locks on the shared operator DB. Add
Drop for PgTransaction + a PinnedTx wrapper for kv_update that detaches a
best-effort ROLLBACK. Adds a deterministic single-connection regression
test (in the pg-serial nextest group) and documents the invariant.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Coverage note

- **Item 0 (flag poison):** Task 1 test `kv_update_guard_restores_flag_on_cancellation` (deterministic).
- **Item 0b (pinned connection), SQLite:** Task 2 tests `sqlite_dropped_tx_rolls_back` (db.tx) and `sqlite_cancelled_kv_update_rolls_back` (kv.update) — both deterministic via a single-connection pool.
- **Item 0b, Postgres:** Task 3 test `pg_dropped_tx_rolls_back` (db.tx) — deterministic, connect-or-skip. There is no PG-specific `kv_update` cancellation test: on a single connection, PG re-issues `BEGIN` as a no-op warning (not an error) inside a leaked transaction, so a read-only probe cannot distinguish fixed from unfixed. The PG `kv_update` path is covered transitively — it shares the exact `PinnedTx`/`spawn_rollback` mechanism exercised by `pg_dropped_tx_rolls_back` (PG) and `sqlite_cancelled_kv_update_rolls_back` (kv.update shape).
