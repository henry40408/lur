# DB write retry-with-jitter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Transparently retry SQLite `SQLITE_BUSY`/`SQLITE_LOCKED` write contention with bounded jittered backoff so concurrent writers stop surfacing spurious "database is locked" errors.

**Architecture:** A `pub(crate)` `retry_busy` helper in `src/capabilities/db.rs` wraps write operations at the **sqlx layer** (before errors become lur-voiced `mlua::Error`), retrying only when the typed error is busy/locked. It is wired into `db.exec`, `begin_immediate` (covering `db.tx` + `kv.update`), and the `kv` single-statement atomic writes. The pool's `busy_timeout` drops from 5 s to 200 ms so app-level jitter — not SQLite's fixed-cadence polling — breaks the herd.

**Tech Stack:** Rust (edition 2024), `sqlx` (SQLite), `mlua` (Luau), `getrandom` (existing dependency, used for jitter), `tokio` (sleep + test runtime), `tempfile` (dev-dependency).

## Global Constraints

- Edition 2024. MSRV (`rust-version`) and toolchain managed separately — do not bump MSRV.
- Run tests with `cargo nextest run` (NOT `cargo test`).
- `cargo fmt --all` before every commit; `cargo clippy --all-targets -- -D warnings` must pass (no dead code — every helper must have a caller in the same task).
- All commits GPG-signed; do not pass `--no-gpg-sign`.
- Stage files explicitly by name; never `git add -A`/`git add .`.
- No new third-party dependency: jitter randomness comes from the existing `getrandom = "0.2.15"`.
- Retry policy is hardcoded (no CLI/config knobs): **5 attempts total** (1 initial + 4 retries), **full jitter** exponential backoff with `base = 5 ms`, `cap = 200 ms`.
- Scope is exactly: `db.exec`, `kv.incr`/`decr`, `kv.add`/`cas`, and `begin_immediate`. `db.query` (read-only), `kv.set`/`delete`, and statements *inside* an open transaction are intentionally NOT retried.
- Commit messages end with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File Structure

- `src/capabilities/db.rs` — Modify: add `is_busy`, `jitter_delay`, `retry_busy`; wire `db.exec` and `begin_immediate`; lower `busy_timeout`; add inline `#[cfg(test)]` unit test for `is_busy`.
- `src/capabilities/kv.rs` — Modify: wire `retry_busy` into `add`, `cas`, and `incr_by` (serves `incr`/`decr`).
- `tests/db.rs` — Modify: add `db.exec` and `db.tx` concurrency tests; bump the existing `kv_incr_is_atomic_under_concurrent_writers` from 2 → 4 writers.
- `README.md` — Modify: `lur.db` busy-handling sentence.
- `ARCHITECTURE.md` — Modify: `db.rs` module-map row and the `lur.db` storage note.

---

### Task 1: Retry primitive + `db.exec` wiring + lower `busy_timeout`

**Files:**
- Modify: `src/capabilities/db.rs`
- Test (unit, inline): `src/capabilities/db.rs` `#[cfg(test)] mod tests`
- Test (integration): `tests/db.rs`

**Interfaces:**
- Produces:
  - `fn is_busy(e: &sqlx::Error) -> bool` — true for SQLITE_BUSY/LOCKED (primary codes `"5"`/`"6"` or a "database is locked"/"database table is locked" message).
  - `fn jitter_delay(attempt: u32) -> std::time::Duration` — full-jitter backoff for the given zero-based failure count.
  - `pub(crate) async fn retry_busy<T, F, Fut>(op: F) -> sqlx::Result<T> where F: FnMut() -> Fut, Fut: Future<Output = sqlx::Result<T>>` — runs `op`, retrying up to 4 times on a busy error.

- [ ] **Step 1: Write the failing unit test for `is_busy`**

Append to the end of `src/capabilities/db.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    // A second BEGIN IMMEDIATE while the first still holds the write lock, with
    // busy_timeout=0, yields a genuine SQLITE_BUSY — the exact error retry_busy
    // must recognize. A syntax error must NOT be classified busy.
    #[test]
    fn is_busy_classifies_sqlite_lock_errors() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let opts = SqliteConnectOptions::new()
                .filename(dir.path().join("busy.db"))
                .create_if_missing(true)
                .busy_timeout(std::time::Duration::from_millis(0))
                .journal_mode(SqliteJournalMode::Wal);
            let pool = SqlitePoolOptions::new()
                .max_connections(2)
                .connect_with(opts)
                .await
                .unwrap();
            sqlx::query("CREATE TABLE t (x)").execute(&pool).await.unwrap();

            let mut a = pool.acquire().await.unwrap();
            sqlx::query("BEGIN IMMEDIATE").execute(&mut *a).await.unwrap();
            let mut b = pool.acquire().await.unwrap();
            let busy = sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *b)
                .await
                .unwrap_err();
            assert!(is_busy(&busy), "SQLITE_BUSY not classified busy: {busy:?}");

            let syntax = sqlx::query("NOT VALID SQL")
                .execute(&pool)
                .await
                .unwrap_err();
            assert!(!is_busy(&syntax), "syntax error wrongly classified busy");
        });
    }
}
```

- [ ] **Step 2: Run the unit test to verify it fails**

Run: `cargo nextest run -E 'test(is_busy_classifies)'`
Expected: FAIL to compile — `cannot find function is_busy in this scope`.

- [ ] **Step 3: Implement the retry primitive**

In `src/capabilities/db.rs`, add these three functions (place them near `begin_immediate`, after the `use` block). `Future` needs importing:

```rust
use std::future::Future;
```

```rust
/// Retry policy for write-lock contention: 4 retries on top of the first try.
const MAX_BUSY_RETRIES: u32 = 4;

/// True when `e` is SQLite busy/locked (primary result codes 5/6, including
/// their extended variants, recognized via code or message).
fn is_busy(e: &sqlx::Error) -> bool {
    if let Some(db) = e.as_database_error() {
        let code = db.code();
        let code = code.as_deref().unwrap_or("");
        return code == "5"
            || code == "6"
            || db.message().contains("database is locked")
            || db.message().contains("database table is locked");
    }
    false
}

/// Full-jitter exponential backoff: after the `attempt`-th failure (0-based),
/// sleep a uniform random duration in `[0, min(cap, base·2^attempt))`.
/// `base = 5 ms`, `cap = 200 ms`. Randomness is drawn from the OS CSPRNG
/// (`getrandom`) so no new dependency is added.
fn jitter_delay(attempt: u32) -> std::time::Duration {
    const BASE_MS: u64 = 5;
    const CAP_MS: u64 = 200;
    // `attempt.min(6)` keeps the shift well clear of overflow; base·2^6 = 320 > cap.
    let ceil = (BASE_MS << attempt.min(6)).min(CAP_MS).max(1);
    let mut buf = [0u8; 8];
    getrandom::getrandom(&mut buf).expect("OS CSPRNG unavailable");
    let ms = u64::from_le_bytes(buf) % ceil;
    std::time::Duration::from_millis(ms)
}

/// Run `op`, retrying on a busy/locked error with jittered backoff up to
/// `MAX_BUSY_RETRIES` times. Non-busy errors return immediately. The caller
/// keeps its own lur-voiced error mapping on the returned `sqlx::Error`.
///
/// `op` MUST rebuild its query (and re-clone any bound parameters) on each
/// call, and MUST NOT be given work whose re-run would duplicate a side effect
/// outside SQLite.
pub(crate) async fn retry_busy<T, F, Fut>(mut op: F) -> sqlx::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = sqlx::Result<T>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if is_busy(&e) && attempt < MAX_BUSY_RETRIES => {
                tokio::time::sleep(jitter_delay(attempt)).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}
```

- [ ] **Step 4: Wire `db.exec` through `retry_busy`**

Replace the `exec` async-function body (currently `src/capabilities/db.rs:44-55`, the `async move { ... }` block) with:

```rust
async move {
    let pool = ensure_pool(&cell, &path).await?;
    // Surface non-retryable bind errors (bad Lua value types) once, before the
    // retry loop — a logic error must never be retried. After this succeeds the
    // in-loop `bind_all` cannot fail, so its Result is unwrapped.
    bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?;
    let res = retry_busy(|| async {
        bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)
            .expect("params validated before retry loop")
            .execute(&pool)
            .await
    })
    .await
    .map_err(|e| Error::runtime(format!("lur.db.exec: {e}")))?;
    let t = lua.create_table()?;
    t.set("rows_affected", res.rows_affected())?;
    t.set("last_insert_id", res.last_insert_rowid())?;
    Ok(t)
}
```

- [ ] **Step 5: Lower the pool `busy_timeout`**

In `open_pool` (`src/capabilities/db.rs:255-259`), change the timeout from 5000 ms to 200 ms:

```rust
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_millis(200))
        .journal_mode(SqliteJournalMode::Wal);
```

- [ ] **Step 6: Run the unit test to verify it passes**

Run: `cargo nextest run -E 'test(is_busy_classifies)'`
Expected: PASS.

- [ ] **Step 7: Add the `db.exec` concurrency integration test**

Append to `tests/db.rs`:

```rust
#[test]
fn db_exec_survives_concurrent_writers() {
    // Many writers INSERTing into one table over separate pools must all succeed
    // with no "database is locked" surfacing — the retry-with-jitter guarantee.
    const THREADS: i64 = 4;
    const PER_THREAD: i64 = 100;

    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        db_path: Some(dir.path().join("w.db")),
        ..Default::default()
    };

    // Create the table + WAL file up front so workers contend on writes only.
    Runtime::with_config(config.clone())
        .expect("runtime builds")
        .run("lur.db.exec('CREATE TABLE hits (id INTEGER PRIMARY KEY AUTOINCREMENT)')")
        .expect("create table");

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let cfg = config.clone();
            std::thread::spawn(move || {
                let rt = Runtime::with_config(cfg).expect("runtime builds");
                rt.run(&format!(
                    "for _ = 1, {PER_THREAD} do lur.db.exec('INSERT INTO hits DEFAULT VALUES') end"
                ))
                .expect("concurrent inserts succeed without a busy error");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("worker thread joined");
    }

    let rt = Runtime::with_config(config).expect("runtime builds");
    rt.run(&format!(
        "local r = lur.db.query('SELECT COUNT(*) AS n FROM hits'); \
         assert(r[1].n == {}, 'row count mismatch: ' .. tostring(r[1].n))",
        THREADS * PER_THREAD
    ))
    .expect("all inserts landed");
}
```

- [ ] **Step 8: Run the new integration test**

Run: `cargo nextest run --test db -E 'test(db_exec_survives_concurrent_writers)'`
Expected: PASS.

- [ ] **Step 9: fmt, clippy, full db test file**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run --test db`
Expected: no warnings; all `db` tests PASS.

- [ ] **Step 10: Commit**

```bash
git add src/capabilities/db.rs tests/db.rs
git commit -m "feat(storage): retry-with-jitter for db.exec busy contention

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Wire `begin_immediate` (covers `db.tx` + `kv.update`)

**Files:**
- Modify: `src/capabilities/db.rs:109-121` (`begin_immediate`)
- Test (integration): `tests/db.rs`

**Interfaces:**
- Consumes: `retry_busy` from Task 1.
- Produces: `begin_immediate` signature unchanged (`pub(crate) async fn begin_immediate(pool: &SqlitePool) -> mlua::Result<PoolConnection<Sqlite>>`); on busy it now retries acquiring the write lock instead of erroring.

- [ ] **Step 1: Write the failing `db.tx` concurrency test**

Append to `tests/db.rs`:

```rust
#[test]
fn db_tx_survives_concurrent_writers() {
    // Each tx takes the write lock via BEGIN IMMEDIATE; many concurrent writers
    // must not surface a busy error. begin_immediate's retry covers this because
    // a busy failure happens before the user body runs.
    const THREADS: i64 = 4;
    const PER_THREAD: i64 = 60;

    let dir = tempfile::tempdir().unwrap();
    let config = RuntimeConfig {
        db_path: Some(dir.path().join("tx.db")),
        ..Default::default()
    };

    Runtime::with_config(config.clone())
        .expect("runtime builds")
        .run(
            "lur.db.exec('CREATE TABLE ctr (k TEXT PRIMARY KEY, n INTEGER)') \
             lur.db.exec('INSERT INTO ctr VALUES (?, 0)', 'c')",
        )
        .expect("seed counter");

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let cfg = config.clone();
            std::thread::spawn(move || {
                let rt = Runtime::with_config(cfg).expect("runtime builds");
                rt.run(&format!(
                    "for _ = 1, {PER_THREAD} do \
                       lur.db.tx(function(tx) \
                         tx.exec('UPDATE ctr SET n = n + 1 WHERE k = ?', 'c') \
                       end) \
                     end"
                ))
                .expect("concurrent transactions commit without a busy error");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("worker thread joined");
    }

    let rt = Runtime::with_config(config).expect("runtime builds");
    rt.run(&format!(
        "local r = lur.db.query('SELECT n FROM ctr WHERE k = ?', 'c'); \
         assert(r[1].n == {}, 'lost a tx update: ' .. tostring(r[1].n))",
        THREADS * PER_THREAD
    ))
    .expect("all transactions committed");
}
```

- [ ] **Step 2: Run it to verify it fails (or flakes)**

Run: `cargo nextest run --test db -E 'test(db_tx_survives_concurrent_writers)'`
Expected: FAIL / flaky — a worker `.expect("concurrent transactions commit without a busy error")` panics with "database is locked" under contention (with `busy_timeout` now 200 ms and no retry on `begin_immediate`).

Note: contention is timing-dependent; if it happens to pass, proceed anyway — Step 3 makes it reliable, which is the point of the guard.

- [ ] **Step 3: Wrap `begin_immediate` in `retry_busy`**

Replace the body of `begin_immediate` (`src/capabilities/db.rs:112-120`) with:

```rust
    let conn = retry_busy(|| async {
        let mut conn = pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        Ok(conn)
    })
    .await
    .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
    Ok(conn)
```

Note: the previous code distinguished `acquire:` from `begin:` in the error text; the retry wrapper collapses both sqlx failures into one `lur.db.tx: begin: {e}` message. Grep for any test asserting the old `acquire:` wording and update it: `rg -n 'lur.db.tx: acquire' tests/ src/`. (None expected.)

- [ ] **Step 4: Run the `db.tx` concurrency test**

Run: `cargo nextest run --test db -E 'test(db_tx_survives_concurrent_writers)'`
Expected: PASS reliably.

- [ ] **Step 5: fmt, clippy, full db test file**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run --test db`
Expected: no warnings; all `db` tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/capabilities/db.rs tests/db.rs
git commit -m "feat(storage): retry begin_immediate lock acquisition (db.tx, kv.update)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Wire `kv` atomic writes + bump the counter concurrency guard

**Files:**
- Modify: `src/capabilities/kv.rs` — `add` (`:113-124`), `cas` (`:147-196`), `incr_by` (`:343-354`)
- Modify: `tests/db.rs:114-164` (`kv_incr_is_atomic_under_concurrent_writers`)

**Interfaces:**
- Consumes: `db::retry_busy` from Task 1.
- Produces: `incr_by`/`add`/`cas` behavior unchanged except busy contention is now retried. Signatures unchanged.

- [ ] **Step 1: Bump the counter guard to 4 writers (make it the failing test)**

In `tests/db.rs`, replace the doc comment and `const THREADS` of `kv_incr_is_atomic_under_concurrent_writers` (`:115-127`) with:

```rust
    // The atomicity claim: kv.incr is a single guarded upsert, so concurrent
    // writers (each its own Runtime + pool, all pointing at one db file) under
    // WAL must not lose an update. Each thread runs a tight incr loop; the final
    // counter must equal threads * per_thread exactly.
    //
    // Four writers hammering one key: retry-with-jitter on the upsert absorbs the
    // SQLITE_BUSY thundering-herd that a bare 200 ms busy_timeout would surface,
    // so every increment lands and none is lost.
    const THREADS: i64 = 4;
```

Leave `const PER_THREAD: i64 = 200;` unchanged.

- [ ] **Step 2: Run it to verify it fails (or flakes)**

Run: `cargo nextest run --test db -E 'test(kv_incr_is_atomic_under_concurrent_writers)'`
Expected: FAIL / flaky — a worker panics with "database is locked" (incr not yet retried, `busy_timeout` now 200 ms). If it passes by luck, proceed — Step 3–5 make it reliable.

- [ ] **Step 3: Wrap `incr_by`'s upsert**

In `src/capabilities/kv.rs`, replace the `let row = sqlx::query(...) ... .fetch_optional(&pool).await.map_err(...)?;` block in `incr_by` (`:344-354`) with a retried version. `key` is moved by `bind`, so clone it per attempt:

```rust
    let row = db::retry_busy(|| async {
        sqlx::query(
            "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = value + excluded.value \
             WHERE typeof(lur_kv.value) = 'integer' \
             RETURNING value",
        )
        .bind(key.clone())
        .bind(delta)
        .fetch_optional(&pool)
        .await
    })
    .await
    .map_err(|e| Error::runtime(format!("{voice}: {e}")))?;
```

- [ ] **Step 4: Wrap `kv.add`'s upsert**

In `src/capabilities/kv.rs`, replace the `let res = sqlx::query(...) ... .execute(&pool).await.map_err(...)?;` block in `add` (`:114-122`) with (clone the moved binds per attempt):

```rust
                    let res = db::retry_busy(|| async {
                        sqlx::query(
                            "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                             ON CONFLICT(key) DO NOTHING",
                        )
                        .bind(key.clone())
                        .bind(value.as_bytes().to_vec())
                        .execute(&pool)
                        .await
                    })
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.add: {e}")))?;
```

- [ ] **Step 5: Wrap each write branch of `kv.cas`**

In `src/capabilities/kv.rs` `cas` (`:147-196`), the four match arms build `exp`/`neu` once as `Vec<u8>` and move `key` into `bind`. Wrap each of the three *write* arms — `(None, Some(v))`, `(Some(e), Some(v))`, `(Some(e), None)` — in `db::retry_busy`, cloning the moved values per attempt. Leave the read-only `(None, None)` arm as-is. Replace the three write arms with:

```rust
                            // expect absent, set new
                            (None, Some(v)) => {
                                db::retry_busy(|| async {
                                    sqlx::query(
                                        "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                                         ON CONFLICT(key) DO NOTHING",
                                    )
                                    .bind(key.clone())
                                    .bind(v.clone())
                                    .execute(&pool)
                                    .await
                                })
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                .rows_affected()
                                    == 1
                            }
                            // expect absent, want absent: succeeds iff already absent
                            (None, None) => {
                                let r = sqlx::query("SELECT 1 FROM lur_kv WHERE key = ?")
                                    .bind(key)
                                    .fetch_optional(&pool)
                                    .await
                                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?;
                                r.is_none()
                            }
                            // expect value, set new
                            (Some(e), Some(v)) => {
                                db::retry_busy(|| async {
                                    sqlx::query(
                                        "UPDATE lur_kv SET value = ? WHERE key = ? AND value = ?",
                                    )
                                    .bind(v.clone())
                                    .bind(key.clone())
                                    .bind(e.clone())
                                    .execute(&pool)
                                    .await
                                })
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                .rows_affected()
                                    == 1
                            }
                            // expect value, delete
                            (Some(e), None) => {
                                db::retry_busy(|| async {
                                    sqlx::query("DELETE FROM lur_kv WHERE key = ? AND value = ?")
                                        .bind(key.clone())
                                        .bind(e.clone())
                                        .execute(&pool)
                                        .await
                                })
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                .rows_affected()
                                    == 1
                            }
```

Note: `retry_busy` is already reachable as `db::retry_busy` — `kv.rs` already imports `use super::db::{self, SqliteShared};`.

- [ ] **Step 6: Run the counter guard to verify it passes reliably**

Run: `cargo nextest run --test db -E 'test(kv_incr_is_atomic_under_concurrent_writers)'`
Expected: PASS. Optionally repeat a few times to confirm stability:
`for i in 1 2 3; do cargo nextest run --test db -E 'test(kv_incr_is_atomic_under_concurrent_writers)' || break; done`

- [ ] **Step 7: fmt, clippy, full db + kv-related tests**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run --test db`
Expected: no warnings; all `db` tests PASS (including `kv_add_and_cas`, `kv_incr_decr_counters`, `kv_update_read_modify_write`).

- [ ] **Step 8: Commit**

```bash
git add src/capabilities/kv.rs tests/db.rs
git commit -m "feat(storage): retry-with-jitter for kv add/cas/incr/decr; bump concurrency guard to 4 writers

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Documentation

**Files:**
- Modify: `README.md:249-250`
- Modify: `ARCHITECTURE.md:44` and `ARCHITECTURE.md:210-211`

**Interfaces:** none (docs only).

- [ ] **Step 1: Update the README `lur.db` busy sentence**

In `README.md`, replace (`:249-250`):

```
  back on error. Write transactions use `BEGIN IMMEDIATE` and wait out lock contention via
  a 5 s `busy_timeout`. Use `?` placeholders; tables must be JSON-encoded first.
```

with:

```
  back on error. Write transactions use `BEGIN IMMEDIATE`; write-lock contention is
  handled by a 200 ms `busy_timeout` plus bounded retry-with-jitter (up to 5 attempts) on
  single-statement writes and lock acquisition, so concurrent writers wait successfully
  instead of raising a spurious "database is locked". Use `?` placeholders; tables must be
  JSON-encoded first.
```

- [ ] **Step 2: Update the ARCHITECTURE module-map row for `db.rs`**

In `ARCHITECTURE.md`, replace the `src/capabilities/db.rs` row (`:44`):

```
| `src/capabilities/db.rs` | Owns the shared SQLite pool (`SqliteShared`), `begin_immediate`/`busy_timeout`, and `lur.db` (`exec`/`query`/`tx`). Hands `SqliteShared` to `kv`. |
```

with:

```
| `src/capabilities/db.rs` | Owns the shared SQLite pool (`SqliteShared`), `begin_immediate`, `busy_timeout`, the `retry_busy` write-contention helper, and `lur.db` (`exec`/`query`/`tx`). Hands `SqliteShared` to `kv`. |
```

- [ ] **Step 3: Update the ARCHITECTURE `lur.db` storage note**

In `ARCHITECTURE.md`, replace (`:210-211`):

```
  `begin_immediate` opens write transactions with `BEGIN IMMEDIATE`; a 5 s `busy_timeout`
  on the pool handles lock contention. Dynamic SQL is wrapped in `sqlx::AssertSqlSafe` at
```

with:

```
  `begin_immediate` opens write transactions with `BEGIN IMMEDIATE`; write-lock contention
  is handled by a 200 ms `busy_timeout` plus `retry_busy`, a bounded (5-attempt) full-jitter
  backoff wrapping single-statement writes (`db.exec`, `kv.add`/`cas`/`incr`/`decr`) and
  lock acquisition (`begin_immediate`, covering `db.tx`/`kv.update`) — retried only where no
  user code has run or the retried body is pure, so a retry never duplicates a side effect.
  Dynamic SQL is wrapped in `sqlx::AssertSqlSafe` at
```

- [ ] **Step 4: Verify docs render and nothing else references the old 5 s timeout**

Run: `rg -n '5 s .busy_timeout|5000' README.md ARCHITECTURE.md src/`
Expected: no stale "5 s busy_timeout"/"5000" references remain (the `open_pool` value is now 200).

- [ ] **Step 5: Commit**

```bash
git add README.md ARCHITECTURE.md
git commit -m "docs(storage): describe db retry-with-jitter and 200ms busy_timeout

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Full suite + branch wrap-up

**Files:** none (verification only).

- [ ] **Step 1: Run the entire test suite**

Run: `cargo nextest run`
Expected: all tests PASS.

- [ ] **Step 2: Final lint + format gate**

Run: `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Confirm the branch is ready**

Run: `git log --oneline main..HEAD`
Expected: the spec commit plus the four implementation commits, in order.

Then hand off to the `superpowers:finishing-a-development-branch` skill to open the PR.

---

## Self-Review

**Spec coverage:**
- Retry helper (`retry_busy`, `is_busy`, full-jitter policy) → Task 1. ✓
- Application points `db.exec` → Task 1; `begin_immediate` → Task 2; `kv.incr`/`decr`/`add`/`cas` → Task 3. ✓
- Lower `busy_timeout` to 200 ms → Task 1 Step 5. ✓
- Bump #55 counter test to 4 writers → Task 3 Step 1. ✓
- `db.exec` concurrency test → Task 1 Step 7. ✓
- `is_busy` unit test → Task 1 Step 1. ✓
- README + ARCHITECTURE docs → Task 4. ✓
- No new dependency (getrandom for jitter) → Task 1 Step 3. ✓
- Not-in-scope (`db.query`, `kv.set`/`delete`, in-transaction statements) → left untouched by construction. ✓

**Placeholder scan:** No TBD/TODO; every code step shows complete code. ✓

**Type consistency:** `retry_busy`/`is_busy`/`jitter_delay` signatures defined in Task 1 and consumed verbatim (`db::retry_busy`) in Tasks 2–3. `RuntimeConfig { db_path, .. }` matches the existing test pattern. ✓
