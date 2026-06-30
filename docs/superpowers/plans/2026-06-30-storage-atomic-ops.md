# Storage Atomic Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `lur.kv` real atomic operations (incr/decr, add, cas, update), bring matching primitives to `lur.state` (cas, add, decr; incr → integer), and make `lur.db` transactions wait out write-lock contention instead of failing.

**Architecture:** Extract `lur.kv` from `db.rs` into its own `src/capabilities/kv.rs` sharing `db.rs`'s lazily-opened SQLite pool. SQLite gives atomicity directly: counters/cas/add are single statements; `update` and `db.tx` use `BEGIN IMMEDIATE` on a pinned pool connection. `lur.state`'s in-memory store gains value-based CAS reusing its existing version-stamped `compare_and_set`.

**Tech Stack:** Rust (edition 2024), mlua (Luau), sqlx (SQLite, WAL), tempfile (dev-dep).

## Global Constraints

- Run tests with `cargo nextest run` (NOT `cargo test`). Lint gate: `cargo clippy --all-targets -- -D warnings`. Format with `cargo fmt --all` before every commit. All commits GPG-signed (`git commit -S`).
- Stage files explicitly by name; never `git add -A` / `git add .`.
- `build_lua` order is fixed; capabilities install before `sandbox(true)`. Do not reorder `capabilities::install`.
- Capability errors are voiced per-capability: `lur.<cap>.<fn>: <message>`. Argument-type errors go through `argcheck::arg(lua, value, "lur.<cap>.<fn>", n, "<expected>")`.
- `lur.kv` values are raw bytes; `lur.kv.get` MUST always return bytes (or `nil`) and MUST never panic.
- Counters are integers. `kv.incr`/`decr` and `state.incr`/`decr` reject a non-integer existing value with `… : existing value is not an integer`.
- `busy_timeout` is fixed at 5000 ms (no CLI flag). Write transactions (`db.tx`, `kv.update`) use `BEGIN IMMEDIATE`.
- Every new `lur.*` function MUST get a runnable example in `docs/GUIDE.md` — `tests/guide.rs::every_runtime_function_has_an_example` reflects the live table and fails otherwise.
- Spec: `docs/superpowers/specs/2026-06-30-storage-atomic-ops-design.md`.

## File Structure

- **`src/capabilities/db.rs`** (modify): keep the pool owner. Add `SqliteShared` handle, make `install` return it, expose `ensure_pool`/`begin_immediate` as `pub(crate)`, set `busy_timeout` in `open_pool`, convert `run_tx` to `BEGIN IMMEDIATE`. Remove `install_kv` (moves to kv.rs).
- **`src/capabilities/kv.rs`** (create): owns `lur.kv` — `get` (type-aware), `set`, `delete`, `add`, `cas`, `incr`, `decr`, `update`. Consumes `db::SqliteShared` + `db::ensure_pool`/`begin_immediate`.
- **`src/capabilities/mod.rs`** (modify): call `let sqlite = db::install(...)?;` then `kv::install(lua, &lur, &sqlite)?;`. Add `pub mod kv;`.
- **`src/capabilities/state.rs`** (modify): `incr` → integer; add `decr`, `cas`, `add`; derive `PartialEq` on `Prim`.
- **`tests/db.rs`** (modify): add kv atomic + concurrency + db busy tests.
- **`tests/state.rs`** (modify): add state cas/add/decr + integer-incr tests.
- **`docs/GUIDE.md`**, **`README.md`**, **`ARCHITECTURE.md`** (modify): document the new surface.

---

### Task 1: Extract `lur.kv` into its own module + type-aware `get`

Pure structural move plus the `get` bug fix (an INTEGER cell in `lur_kv` currently crashes `kv.get` because it decodes `Vec<u8>` against `INTEGER`). No new Lua surface yet.

**Files:**
- Create: `src/capabilities/kv.rs`
- Modify: `src/capabilities/db.rs` (add `SqliteShared`, return it from `install`, make `ensure_pool` `pub(crate)`, delete `install_kv`)
- Modify: `src/capabilities/mod.rs` (add `pub mod kv;`, wire `kv::install`)
- Test: `tests/db.rs`

**Interfaces:**
- Produces: `db::SqliteShared { cell: Arc<OnceLock<SqlitePool>>, path: Arc<Option<PathBuf>> }`; `pub(crate) async fn db::ensure_pool(cell, path) -> mlua::Result<SqlitePool>`; `pub fn db::install(lua, lur, db_path) -> Result<SqliteShared, RunError>`; `pub fn kv::install(lua, lur, shared: &db::SqliteShared) -> Result<(), RunError>`.

- [ ] **Step 1: Write the failing test** — append to `tests/db.rs`:

```rust
#[test]
fn kv_get_reads_an_integer_cell_as_decimal_bytes() {
    // A counter (INTEGER affinity) written into lur_kv must read back through
    // kv.get as its decimal-string bytes, not crash on a Vec<u8> type mismatch.
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec(\"INSERT INTO lur_kv(key,value) VALUES('c', 42)\")\n\
         assert(lur.kv.get('c') == '42', 'integer cell reads as \"42\"')\n\
         lur.kv.set('b', 'raw')\n\
         assert(lur.kv.get('b') == 'raw', 'blob cell still reads raw bytes')\n\
         assert(lur.kv.get('missing') == nil, 'absent is nil')",
    )
    .expect("type-aware kv.get");
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run --test db -E 'test(kv_get_reads_an_integer_cell)'`
Expected: FAIL — current `kv.get` errors with `mismatched types; Rust type Vec<u8> … is not compatible with SQL type INTEGER`.

- [ ] **Step 3: Add `SqliteShared` + make `ensure_pool` shareable in `db.rs`**

At the top of `src/capabilities/db.rs`, after the imports, add:

```rust
/// Shared, lazily-opened SQLite pool plus its configured path, handed from
/// `db::install` to `kv::install` so both capabilities use one pool.
pub(crate) struct SqliteShared {
    pub(crate) cell: Arc<OnceLock<SqlitePool>>,
    pub(crate) path: Arc<Option<PathBuf>>,
}
```

Change `ensure_pool`'s signature to `pub(crate) async fn ensure_pool(...)` (body unchanged).

- [ ] **Step 4: Make `db::install` return `SqliteShared` and stop installing kv**

In `db::install`, change the return type to `Result<SqliteShared, RunError>`. Delete the `install_kv(lua, lur, &cell, &path)?;` call and the entire `install_kv` function (it moves to `kv.rs`). End `install` with:

```rust
    lur.set("db", db).map_err(RunError::Init)?;
    Ok(SqliteShared {
        cell: Arc::clone(&cell),
        path: Arc::clone(&path),
    })
}
```

- [ ] **Step 5: Create `src/capabilities/kv.rs` with type-aware `get` + moved `set`/`delete`**

```rust
//! `lur.kv` — key/value storage over the shared `lur_kv(key TEXT, value BLOB)`
//! table (spec §6). Keys are strings, values raw bytes. Atomic operations
//! (add/cas/incr/decr/update) use SQLite's own atomicity; see the design spec.

use mlua::{Error, Lua, Table, Value};
use sqlx::{Row, TypeInfo, ValueRef};

use super::db::{self, SqliteShared};
use crate::runtime::RunError;

/// Install `lur.kv` sharing `db`'s lazily-opened pool.
pub fn install(lua: &Lua, lur: &Table, shared: &SqliteShared) -> Result<(), RunError> {
    let kv = lua.create_table().map_err(RunError::Init)?;

    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let get = lua
            .create_async_function(move |lua, key: String| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    let pool = db::ensure_pool(&cell, &path).await?;
                    let row = sqlx::query("SELECT value FROM lur_kv WHERE key = ?")
                        .bind(key)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
                    match row {
                        None => Ok(Value::Nil),
                        Some(r) => {
                            let raw = r
                                .try_get_raw(0)
                                .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
                            if raw.is_null() {
                                return Ok(Value::Nil);
                            }
                            // Always hand back bytes: counters (INTEGER) and
                            // REAL render as their decimal string; TEXT/BLOB are
                            // the raw bytes. Never a Vec<u8> type-mismatch panic.
                            let bytes: Vec<u8> = match raw.type_info().name() {
                                "INTEGER" => decode::<i64>(&r)?.to_string().into_bytes(),
                                "REAL" => decode::<f64>(&r)?.to_string().into_bytes(),
                                _ => decode::<Vec<u8>>(&r)?,
                            };
                            Ok(Value::String(lua.create_string(bytes)?))
                        }
                    }
                }
            })
            .map_err(RunError::Init)?;
        kv.set("get", get).map_err(RunError::Init)?;
    }
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let set = lua
            .create_async_function(move |_, (key, value): (String, mlua::String)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    let pool = db::ensure_pool(&cell, &path).await?;
                    sqlx::query("INSERT OR REPLACE INTO lur_kv (key, value) VALUES (?, ?)")
                        .bind(key)
                        .bind(value.as_bytes().to_vec())
                        .execute(&pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.set: {e}")))?;
                    Ok(())
                }
            })
            .map_err(RunError::Init)?;
        kv.set("set", set).map_err(RunError::Init)?;
    }
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let delete = lua
            .create_async_function(move |_, key: String| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    let pool = db::ensure_pool(&cell, &path).await?;
                    sqlx::query("DELETE FROM lur_kv WHERE key = ?")
                        .bind(key)
                        .execute(&pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.delete: {e}")))?;
                    Ok(())
                }
            })
            .map_err(RunError::Init)?;
        kv.set("delete", delete).map_err(RunError::Init)?;
    }

    lur.set("kv", kv).map_err(RunError::Init)?;
    Ok(())
}

/// Decode column 0 of a single-column row, lur-voiced on failure.
fn decode<'r, T>(row: &'r sqlx::sqlite::SqliteRow) -> mlua::Result<T>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get::<T, usize>(0)
        .map_err(|e| Error::runtime(format!("lur.kv.get: decoding value: {e}")))
}
```

- [ ] **Step 6: Wire it in `mod.rs`**

Add `pub mod kv;` to the module list. Replace the `db::install(...)?;` line with:

```rust
    let sqlite = db::install(lua, &lur, config.db_path.clone())?;
    kv::install(lua, &lur, &sqlite)?;
```

(Keep the line position — still before `state::install`.)

- [ ] **Step 7: Run the new test + the full suite**

Run: `cargo nextest run --test db` then `cargo nextest run`
Expected: PASS — the new type-aware test passes and every existing `db.rs`/`capabilities.rs` kv test still passes.

- [ ] **Step 8: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/capabilities/db.rs src/capabilities/kv.rs src/capabilities/mod.rs tests/db.rs
git commit -S -m "refactor(kv): extract lur.kv into its own module; type-aware get"
```

---

### Task 2: `lur.db` busy handling — `busy_timeout` + `BEGIN IMMEDIATE`

Set `busy_timeout` on the pool and convert `run_tx` (and the shared helper `kv.update` will use) to acquire the write lock up front via `BEGIN IMMEDIATE` on a pinned connection.

**Files:**
- Modify: `src/capabilities/db.rs` (`open_pool`, `run_tx`, add `begin_immediate`)
- Test: `tests/db.rs`

**Interfaces:**
- Produces: `pub(crate) async fn db::begin_immediate(pool: &SqlitePool) -> mlua::Result<sqlx::pool::PoolConnection<sqlx::Sqlite>>` — acquires a connection and runs `BEGIN IMMEDIATE`. Callers run statements on `&mut *conn` and finish with `COMMIT`/`ROLLBACK` via `sqlx::query`.

- [ ] **Step 1: Write the failing test** — append to `tests/db.rs`:

```rust
#[test]
fn tx_uses_a_write_lock_and_still_commits_and_rolls_back() {
    // Smoke test that the BEGIN IMMEDIATE rewrite preserves tx semantics:
    // a committing tx persists, an erroring tx rolls back, on the same db.
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)')\n\
         lur.db.exec('INSERT INTO t VALUES (1, 0)')\n\
         lur.db.tx(function(tx) tx.exec('UPDATE t SET n = 5 WHERE id = 1') end)\n\
         assert(lur.db.query('SELECT n FROM t WHERE id=1')[1].n == 5, 'committed')\n\
         pcall(function()\n\
           lur.db.tx(function(tx)\n\
             tx.exec('UPDATE t SET n = 99 WHERE id = 1')\n\
             error('boom')\n\
           end)\n\
         end)\n\
         assert(lur.db.query('SELECT n FROM t WHERE id=1')[1].n == 5, 'rolled back')",
    )
    .expect("tx commit + rollback under IMMEDIATE");
}
```

- [ ] **Step 2: Run it to confirm current behavior**

Run: `cargo nextest run --test db -E 'test(tx_uses_a_write_lock)'`
Expected: PASS today (this is a regression guard for the refactor — it must keep passing). Note the green result, then proceed; Steps 3-5 must not break it.

- [ ] **Step 3: Set `busy_timeout` in `open_pool`**

In `src/capabilities/db.rs`, in `open_pool`, add `busy_timeout` to the connect options:

```rust
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_millis(5000))
        .journal_mode(SqliteJournalMode::Wal);
```

- [ ] **Step 4: Add `begin_immediate` and rewrite `run_tx` to use it**

Add the helper:

```rust
/// Acquire a pooled connection and open a write transaction with `BEGIN
/// IMMEDIATE`, so the write lock is taken up front (no read→write upgrade
/// busy/deadlock) and the caller's body runs exactly once. The caller MUST
/// finish with `COMMIT` or `ROLLBACK`.
pub(crate) async fn begin_immediate(
    pool: &SqlitePool,
) -> mlua::Result<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| Error::runtime(format!("lur.db.tx: acquire: {e}")))?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *conn)
        .await
        .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
    Ok(conn)
}
```

Rewrite `run_tx` to drive the pinned connection (replaces `pool.begin()` / `Transaction`). The exec/query handles run on the shared connection; commit/rollback are explicit SQL:

```rust
async fn run_tx(
    lua: Lua,
    cell: &OnceLock<SqlitePool>,
    path: &Option<PathBuf>,
    func: Function,
) -> mlua::Result<()> {
    let pool = ensure_pool(cell, path).await?;
    let conn = begin_immediate(&pool).await?;
    let shared = Arc::new(tokio::sync::Mutex::new(Some(conn)));

    let handle = lua.create_table()?;
    {
        let shared = Arc::clone(&shared);
        let exec =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = Arc::clone(&shared);
                async move {
                    let mut guard = shared.lock().await;
                    let conn = guard
                        .as_mut()
                        .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
                    let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?
                        .execute(&mut **conn)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.db.tx exec: {e}")))?;
                    let t = lua.create_table()?;
                    t.set("rows_affected", res.rows_affected())?;
                    t.set("last_insert_id", res.last_insert_rowid())?;
                    Ok(t)
                }
            })?;
        handle.set("exec", exec)?;
    }
    {
        let shared = Arc::clone(&shared);
        let query =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = Arc::clone(&shared);
                async move {
                    let mut guard = shared.lock().await;
                    let conn = guard
                        .as_mut()
                        .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
                    let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?
                        .fetch_all(&mut **conn)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.db.tx query: {e}")))?;
                    let out = lua.create_table()?;
                    for (i, row) in rows.iter().enumerate() {
                        out.raw_set(i as i64 + 1, read_row(&lua, row)?)?;
                    }
                    Ok(out)
                }
            })?;
        handle.set("query", query)?;
    }

    let result = func.call_async::<MultiValue>(handle).await;
    let conn = shared.lock().await.take();
    match result {
        Ok(_) => {
            if let Some(mut conn) = conn {
                sqlx::query("COMMIT")
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| Error::runtime(format!("lur.db.tx: commit: {e}")))?;
            }
            Ok(())
        }
        Err(e) => {
            if let Some(mut conn) = conn {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            }
            Err(e)
        }
    }
}
```

Remove the now-unused `SqliteRow` import only if the compiler flags it (it is still used by `read_row`). Keep `Function`, `MultiValue` imports.

- [ ] **Step 5: Run tx tests + full suite**

Run: `cargo nextest run --test db` then `cargo nextest run`
Expected: PASS — `tx_commits_on_normal_return`, `tx_rolls_back_on_error`, and the new `tx_uses_a_write_lock_and_still_commits_and_rolls_back` all green.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/capabilities/db.rs tests/db.rs
git commit -S -m "feat(db): busy_timeout + BEGIN IMMEDIATE write transactions"
```

---

### Task 3: `lur.kv.add` and `lur.kv.cas`

Single-statement set-if-absent and compare-and-set (byte equality). No transaction needed; `busy_timeout` covers contention.

**Files:**
- Modify: `src/capabilities/kv.rs`
- Test: `tests/db.rs`

**Interfaces:**
- Produces: `lur.kv.add(key, value) -> bool`; `lur.kv.cas(key, expected|nil, new|nil) -> bool`.

- [ ] **Step 1: Write the failing test** — append to `tests/db.rs`:

```rust
#[test]
fn kv_add_and_cas() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "assert(lur.kv.add('k', 'first') == true, 'add inserts when absent')\n\
         assert(lur.kv.add('k', 'second') == false, 'add is a no-op when present')\n\
         assert(lur.kv.get('k') == 'first', 'value kept from first add')\n\
         -- cas update-if-equal\n\
         assert(lur.kv.cas('k', 'first', 'next') == true, 'cas applies on match')\n\
         assert(lur.kv.cas('k', 'first', 'nope') == false, 'cas rejects on mismatch')\n\
         assert(lur.kv.get('k') == 'next', 'value is the cas result')\n\
         -- cas set-if-absent (expected = nil)\n\
         assert(lur.kv.cas('fresh', nil, 'v') == true, 'cas(nil,...) sets when absent')\n\
         assert(lur.kv.cas('fresh', nil, 'v2') == false, 'cas(nil,...) fails when present')\n\
         -- cas delete-if-equal (new = nil)\n\
         assert(lur.kv.cas('fresh', 'v', nil) == true, 'cas(...,nil) deletes on match')\n\
         assert(lur.kv.get('fresh') == nil, 'deleted')",
    )
    .expect("kv add + cas");
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run --test db -E 'test(kv_add_and_cas)'`
Expected: FAIL — `attempt to call a nil value (field 'add')`.

- [ ] **Step 3: Implement `add` and `cas` in `kv.rs`**

Add, before `lur.set("kv", kv)`, two blocks. `add`:

```rust
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let add = lua
            .create_async_function(move |_, (key, value): (String, mlua::String)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    let pool = db::ensure_pool(&cell, &path).await?;
                    let res = sqlx::query(
                        "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                         ON CONFLICT(key) DO NOTHING",
                    )
                    .bind(key)
                    .bind(value.as_bytes().to_vec())
                    .execute(&pool)
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.add: {e}")))?;
                    Ok(res.rows_affected() == 1)
                }
            })
            .map_err(RunError::Init)?;
        kv.set("add", add).map_err(RunError::Init)?;
    }
```

`cas` (branch on nil expected/new; `mlua::String` optionals so `nil` is `None`):

```rust
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let cas = lua
            .create_async_function(
                move |_, (key, expected, new): (String, Option<mlua::String>, Option<mlua::String>)| {
                    let cell = std::sync::Arc::clone(&cell);
                    let path = std::sync::Arc::clone(&path);
                    async move {
                        let pool = db::ensure_pool(&cell, &path).await?;
                        let exp = expected.map(|s| s.as_bytes().to_vec());
                        let neu = new.map(|s| s.as_bytes().to_vec());
                        let applied = match (exp, neu) {
                            // expect absent, set new
                            (None, Some(v)) => {
                                sqlx::query(
                                    "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                                     ON CONFLICT(key) DO NOTHING",
                                )
                                .bind(key)
                                .bind(v)
                                .execute(&pool)
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
                                sqlx::query("UPDATE lur_kv SET value = ? WHERE key = ? AND value = ?")
                                    .bind(v)
                                    .bind(key)
                                    .bind(e)
                                    .execute(&pool)
                                    .await
                                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                    .rows_affected()
                                    == 1
                            }
                            // expect value, delete
                            (Some(e), None) => {
                                sqlx::query("DELETE FROM lur_kv WHERE key = ? AND value = ?")
                                    .bind(key)
                                    .bind(e)
                                    .execute(&pool)
                                    .await
                                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                    .rows_affected()
                                    == 1
                            }
                        };
                        Ok(applied)
                    }
                },
            )
            .map_err(RunError::Init)?;
        kv.set("cas", cas).map_err(RunError::Init)?;
    }
```

- [ ] **Step 4: Run the test + suite**

Run: `cargo nextest run --test db -E 'test(kv_add_and_cas)'` then `cargo nextest run --test db`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/capabilities/kv.rs tests/db.rs
git commit -S -m "feat(kv): add (set-if-absent) and cas (compare-and-set)"
```

---

### Task 4: `lur.kv.incr` and `lur.kv.decr`

Single-statement integer counters via a guarded upsert with `RETURNING` (a `None` result means the existing value is not an integer).

**Files:**
- Modify: `src/capabilities/kv.rs`
- Test: `tests/db.rs`

**Interfaces:**
- Produces: `lur.kv.incr(key, n?) -> integer`; `lur.kv.decr(key, n?) -> integer`. `n` defaults to 1, must be an integer.

- [ ] **Step 1: Write the failing test** — append to `tests/db.rs`:

```rust
#[test]
fn kv_incr_decr_counters() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "assert(lur.kv.incr('hits') == 1, 'first incr creates at 1')\n\
         assert(lur.kv.incr('hits', 4) == 5, 'incr by 4')\n\
         assert(lur.kv.decr('hits', 2) == 3, 'decr by 2')\n\
         assert(lur.kv.get('hits') == '3', 'counter reads back as decimal bytes')\n\
         -- incr on a non-integer value errors and leaves it intact\n\
         lur.kv.set('blob', 'hello')\n\
         local ok, err = pcall(function() return lur.kv.incr('blob') end)\n\
         assert(ok == false, 'incr on a blob errors')\n\
         assert(tostring(err):find('not an integer'), 'clear message: ' .. tostring(err))\n\
         assert(lur.kv.get('blob') == 'hello', 'blob untouched after failed incr')",
    )
    .expect("kv incr/decr");
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run --test db -E 'test(kv_incr_decr_counters)'`
Expected: FAIL — `attempt to call a nil value (field 'incr')`.

- [ ] **Step 3: Implement `incr`/`decr` via a shared helper in `kv.rs`**

Add a free helper and two install blocks. The guarded upsert returns the new value, or no row when the existing value is non-integer:

```rust
/// Atomically add `delta` to an integer counter `key`, creating it at `delta`
/// when absent. Returns the new value, or errors if the key holds a
/// non-integer (the `WHERE typeof(value)='integer'` guard returns no row).
async fn incr_by(
    cell: &std::sync::Arc<std::sync::OnceLock<sqlx::sqlite::SqlitePool>>,
    path: &std::sync::Arc<Option<std::path::PathBuf>>,
    key: String,
    delta: i64,
) -> mlua::Result<i64> {
    let pool = db::ensure_pool(cell, path).await?;
    let row = sqlx::query(
        "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = value + excluded.value \
         WHERE typeof(lur_kv.value) = 'integer' \
         RETURNING value",
    )
    .bind(key)
    .bind(delta)
    .fetch_optional(&pool)
    .await
    .map_err(|e| Error::runtime(format!("lur.kv.incr: {e}")))?;
    match row {
        Some(r) => r
            .try_get::<i64, usize>(0)
            .map_err(|e| Error::runtime(format!("lur.kv.incr: {e}"))),
        None => Err(Error::runtime(
            "lur.kv.incr: existing value is not an integer".to_string(),
        )),
    }
}
```

Install blocks (`n` via `Option<i64>` so a fractional step is rejected by mlua with our message):

```rust
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let incr = lua
            .create_async_function(move |lua, (key, n): (String, Value)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    let n: Option<i64> = argcheck::arg(&lua, n, "lur.kv.incr", 2, "integer")?;
                    incr_by(&cell, &path, key, n.unwrap_or(1)).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("incr", incr).map_err(RunError::Init)?;
    }
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let decr = lua
            .create_async_function(move |lua, (key, n): (String, Value)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    let n: Option<i64> = argcheck::arg(&lua, n, "lur.kv.decr", 2, "integer")?;
                    let delta = n.unwrap_or(1).checked_neg().ok_or_else(|| {
                        Error::runtime("lur.kv.decr: step too large".to_string())
                    })?;
                    incr_by(&cell, &path, key, delta).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("decr", decr).map_err(RunError::Init)?;
    }
```

Add `use crate::capabilities::argcheck;` to the imports.

- [ ] **Step 4: Run the test + suite**

Run: `cargo nextest run --test db -E 'test(kv_incr_decr_counters)'` then `cargo nextest run --test db`
Expected: PASS.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/capabilities/kv.rs tests/db.rs
git commit -S -m "feat(kv): integer incr/decr counters with a non-integer guard"
```

---

### Task 5: `lur.kv.update` (read-modify-write)

A `BEGIN IMMEDIATE` transaction that reads the current value, calls the user transform, and writes the result (or deletes on `nil`). A re-entry guard prevents a nested `lur.kv`/`lur.db` call inside the transform from deadlocking on the pinned connection.

**Files:**
- Modify: `src/capabilities/kv.rs`
- Test: `tests/db.rs`

**Interfaces:**
- Produces: `lur.kv.update(key, fn) -> bytes|nil`. `fn(current: bytes|nil) -> new: bytes|nil`.

- [ ] **Step 1: Write the failing test** — append to `tests/db.rs`:

```rust
#[test]
fn kv_update_read_modify_write() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "-- create via update (current is nil)\n\
         local v = lur.kv.update('k', function(cur)\n\
           assert(cur == nil, 'absent starts nil')\n\
           return 'a'\n\
         end)\n\
         assert(v == 'a', 'update returns the new value')\n\
         -- transform existing\n\
         lur.kv.update('k', function(cur) return cur .. 'b' end)\n\
         assert(lur.kv.get('k') == 'ab', 'appended')\n\
         -- delete by returning nil\n\
         local d = lur.kv.update('k', function(_) return nil end)\n\
         assert(d == nil and lur.kv.get('k') == nil, 'nil deletes')\n\
         -- re-entry from inside the transform errors\n\
         local ok, err = pcall(function()\n\
           lur.kv.update('k', function(_) lur.kv.set('x', 'y'); return '1' end)\n\
         end)\n\
         assert(ok == false and tostring(err):find('re%-enter'), 'reentry blocked: ' .. tostring(err))",
    )
    .expect("kv update RMW + reentry guard");
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run --test db -E 'test(kv_update_read_modify_write)'`
Expected: FAIL — `attempt to call a nil value (field 'update')`.

- [ ] **Step 3: Implement `update` with a re-entry guard in `kv.rs`**

Add a `thread_local` guard near the top of `kv.rs` (after imports):

```rust
use std::cell::Cell;

thread_local! {
    /// Set while a kv.update transform runs, so a nested lur.kv/lur.db call on
    /// the same stack raises a clear error instead of deadlocking on the pinned
    /// transaction connection.
    static IN_KV_UPDATE: Cell<bool> = const { Cell::new(false) };
}

fn reject_kv_reentry(fname: &str) -> mlua::Result<()> {
    if IN_KV_UPDATE.with(Cell::get) {
        return Err(Error::runtime(format!(
            "{fname}: cannot re-enter lur.kv from inside update()"
        )));
    }
    Ok(())
}
```

Guard every kv async body by calling `reject_kv_reentry("lur.kv.<fn>")?;` as the **first statement**, before `db::ensure_pool` (so a nested call fails fast without touching the pool). Add it to `get`/`set`/`delete`/`add`/`cas` and to the `incr`/`decr` closures (before calling `incr_by`). `update` itself also guards first (a nested `update` must error too). This makes any nested kv op inside a transform fail.

Install `update` (drives `begin_immediate`, reads, runs the transform with the guard set, writes, commits):

```rust
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let update = lua
            .create_async_function(move |lua, (key, func): (String, mlua::Function)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.update")?;
                    let pool = db::ensure_pool(&cell, &path).await?;
                    let mut conn = db::begin_immediate(&pool).await?;

                    // read current value as bytes|nil (type-aware, same as get)
                    let cur: Value = match sqlx::query("SELECT value FROM lur_kv WHERE key = ?")
                        .bind(&key)
                        .fetch_optional(&mut *conn)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?
                    {
                        None => Value::Nil,
                        Some(r) => {
                            let raw = r
                                .try_get_raw(0)
                                .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                            if raw.is_null() {
                                Value::Nil
                            } else {
                                let bytes: Vec<u8> = match raw.type_info().name() {
                                    "INTEGER" => decode::<i64>(&r)?.to_string().into_bytes(),
                                    "REAL" => decode::<f64>(&r)?.to_string().into_bytes(),
                                    _ => decode::<Vec<u8>>(&r)?,
                                };
                                Value::String(lua.create_string(bytes)?)
                            }
                        }
                    };

                    IN_KV_UPDATE.with(|f| f.set(true));
                    let result = func.call_async::<Value>(cur).await;
                    IN_KV_UPDATE.with(|f| f.set(false));

                    let new = match result {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                            return Err(e);
                        }
                    };

                    match &new {
                        Value::Nil => {
                            sqlx::query("DELETE FROM lur_kv WHERE key = ?")
                                .bind(&key)
                                .execute(&mut *conn)
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                        }
                        Value::String(s) => {
                            sqlx::query(
                                "INSERT OR REPLACE INTO lur_kv (key, value) VALUES (?, ?)",
                            )
                            .bind(&key)
                            .bind(s.as_bytes().to_vec())
                            .execute(&mut *conn)
                            .await
                            .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                        }
                        other => {
                            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                            return Err(Error::runtime(format!(
                                "lur.kv.update: transform must return a string or nil, got {}",
                                other.type_name()
                            )));
                        }
                    }
                    sqlx::query("COMMIT")
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: commit: {e}")))?;
                    Ok(new)
                }
            })
            .map_err(RunError::Init)?;
        kv.set("update", update).map_err(RunError::Init)?;
    }
```

> Note on the guard: setting `IN_KV_UPDATE` is on the same OS thread as the awaited transform because the runtime drives Lua on a `LocalSet`/current-thread executor. A nested kv op sees the flag set and raises. Re-entry into `lur.db` write paths is naturally serialized by the `busy_timeout`/IMMEDIATE machinery, but the kv guard is the explicit, fast failure.

- [ ] **Step 4: Run the test + full suite**

Run: `cargo nextest run --test db -E 'test(kv_update)'` then `cargo nextest run`
Expected: PASS — RMW create/transform/delete and the re-entry error all hold; no existing test regresses.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/capabilities/kv.rs tests/db.rs
git commit -S -m "feat(kv): update (read-modify-write) with a re-entry guard"
```

---

### Task 6: `lur.state.incr` → integer + `lur.state.decr`

Change `state.incr` to integer-semantics (reject fractional step and non-integer existing value) and add `decr`.

**Files:**
- Modify: `src/capabilities/state.rs`
- Test: `tests/state.rs`

**Interfaces:**
- Produces: `lur.state.incr(key, n?) -> integer`; `lur.state.decr(key, n?) -> integer`. Both reject fractional `n` and a non-integer existing value.

- [ ] **Step 1: Write the failing test** — append to `tests/state.rs` (match the file's existing harness; if it builds a `Runtime::new()`, reuse that helper):

```rust
#[test]
fn state_incr_is_integer_and_has_decr() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    rt.run(
        "assert(lur.state.incr('n') == 1, 'first incr -> 1')\n\
         assert(lur.state.incr('n', 4) == 5, 'incr by 4')\n\
         assert(lur.state.decr('n', 2) == 3, 'decr by 2')\n\
         -- fractional step is rejected\n\
         local ok = pcall(function() return lur.state.incr('n', 0.5) end)\n\
         assert(ok == false, 'fractional step rejected')\n\
         -- non-integer existing value is rejected\n\
         lur.state.set('s', 'text')\n\
         local ok2, err = pcall(function() return lur.state.incr('s') end)\n\
         assert(ok2 == false and tostring(err):find('not an integer'), 'msg: ' .. tostring(err))",
    )
    .expect("state integer incr/decr");
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run --test state -E 'test(state_incr_is_integer_and_has_decr)'`
Expected: FAIL — `decr` is nil and/or `incr('n',0.5)` currently succeeds.

- [ ] **Step 3: Change `StateStore::incr` to integer and add the guard**

In `src/capabilities/state.rs`, replace the `incr` method on `StateStore`:

```rust
    /// Atomic integer `+n` fast path. Errors if the existing value is not a
    /// whole number, or on i64 overflow.
    fn incr(&self, key: Vec<u8>, n: i64) -> Result<i64, IncrError> {
        let mut map = self.lock();
        let base: i64 = match map.get(&key).and_then(|v| v.value.as_ref()) {
            None => 0,
            Some(Prim::Num(x))
                if x.fract() == 0.0 && *x >= i64::MIN as f64 && *x <= i64::MAX as f64 =>
            {
                *x as i64
            }
            Some(_) => return Err(IncrError::NotInteger),
            // Some(Prim::Num(non-whole)) also falls through to NotInteger.
        };
        let new = base.checked_add(n).ok_or(IncrError::Overflow)?;
        let version = map.get(&key).map_or(0, |v| v.version) + 1;
        map.insert(
            key,
            Versioned {
                value: Some(Prim::Num(new as f64)),
                version,
            },
        );
        Ok(new)
    }
```

Add the error enum near the top:

```rust
/// Why an integer counter operation failed.
enum IncrError {
    NotInteger,
    Overflow,
}
```

- [ ] **Step 4: Rewire the `incr` closure and add `decr` in `install`**

Replace the existing `incr` closure body so `n` is an integer and errors map to messages:

```rust
    let s = store.clone();
    let incr = lua
        .create_function(move |lua, (key, n): (Value, Value)| {
            let key: mlua::String = argcheck::arg(lua, key, "lur.state.incr", 1, "string")?;
            let n: Option<i64> = argcheck::arg(lua, n, "lur.state.incr", 2, "integer")?;
            reject_reentry()?;
            s.incr(key.as_bytes().to_vec(), n.unwrap_or(1)).map_err(|e| match e {
                IncrError::NotInteger => mlua::Error::RuntimeError(
                    "lur.state.incr: existing value is not an integer".into(),
                ),
                IncrError::Overflow => {
                    mlua::Error::RuntimeError("lur.state.incr: counter overflow".into())
                }
            })
        })
        .map_err(RunError::Init)?;
    state.set("incr", incr).map_err(RunError::Init)?;

    let s = store.clone();
    let decr = lua
        .create_function(move |lua, (key, n): (Value, Value)| {
            let key: mlua::String = argcheck::arg(lua, key, "lur.state.decr", 1, "string")?;
            let n: Option<i64> = argcheck::arg(lua, n, "lur.state.decr", 2, "integer")?;
            reject_reentry()?;
            let delta = n.unwrap_or(1).checked_neg().ok_or_else(|| {
                mlua::Error::RuntimeError("lur.state.decr: step too large".into())
            })?;
            s.incr(key.as_bytes().to_vec(), delta).map_err(|e| match e {
                IncrError::NotInteger => mlua::Error::RuntimeError(
                    "lur.state.decr: existing value is not an integer".into(),
                ),
                IncrError::Overflow => {
                    mlua::Error::RuntimeError("lur.state.decr: counter overflow".into())
                }
            })
        })
        .map_err(RunError::Init)?;
    state.set("decr", decr).map_err(RunError::Init)?;
```

Update the inline unit test `compare_and_set_respects_versions` only if it called the old `incr`; it does not. If any existing test calls `store.incr(..)` with an `f64`, change the literal to an integer and expect `Ok(i64)`.

- [ ] **Step 5: Run the test + suite**

Run: `cargo nextest run --test state` then `cargo nextest run -E 'package(lur) and test(state)'`
Expected: PASS.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/capabilities/state.rs tests/state.rs
git commit -S -m "feat(state): integer incr + decr (reject fractional/non-integer)"
```

---

### Task 7: `lur.state.cas` and `lur.state.add`

Value-based compare-and-set reusing the version-stamped `compare_and_set`, and `add` as set-if-absent.

**Files:**
- Modify: `src/capabilities/state.rs` (derive `PartialEq` on `Prim`; add `cas`/`add`)
- Test: `tests/state.rs`

**Interfaces:**
- Produces: `lur.state.cas(key, expected|nil, new|nil) -> bool`; `lur.state.add(key, value) -> bool`.

- [ ] **Step 1: Write the failing test** — append to `tests/state.rs`:

```rust
#[test]
fn state_cas_and_add() {
    let rt = lur::runtime::Runtime::new().expect("runtime builds");
    rt.run(
        "assert(lur.state.add('k', 'first') == true, 'add when absent')\n\
         assert(lur.state.add('k', 'second') == false, 'add no-op when present')\n\
         assert(lur.state.get('k') == 'first', 'value kept')\n\
         assert(lur.state.cas('k', 'first', 'next') == true, 'cas on match')\n\
         assert(lur.state.cas('k', 'first', 'nope') == false, 'cas on mismatch')\n\
         assert(lur.state.get('k') == 'next', 'cas applied')\n\
         -- cas set-if-absent and delete-if-equal\n\
         assert(lur.state.cas('fresh', nil, 'v') == true, 'cas(nil,..) sets when absent')\n\
         assert(lur.state.cas('fresh', 'v', nil) == true, 'cas(..,nil) deletes on match')\n\
         assert(lur.state.get('fresh') == nil, 'deleted')\n\
         -- numbers compare by value\n\
         lur.state.set('c', 7)\n\
         assert(lur.state.cas('c', 7, 8) == true, 'numeric cas')\n\
         assert(lur.state.get('c') == 8, 'numeric applied')",
    )
    .expect("state cas + add");
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo nextest run --test state -E 'test(state_cas_and_add)'`
Expected: FAIL — `cas`/`add` are nil.

- [ ] **Step 3: Derive `PartialEq` on `Prim` and add a value-CAS method**

Change `#[derive(Debug, Clone)]` on `Prim` to `#[derive(Debug, Clone, PartialEq)]`. Add a method on `StateStore`:

```rust
    /// Value-based CAS: store `new` iff the current value equals `expected`.
    /// ABA-safe — uses the per-key version under the hood, so a concurrent
    /// write makes it return `false` (caller retries). Returns whether applied.
    fn cas_value(&self, key: &[u8], expected: &Option<Prim>, new: Option<Prim>) -> bool {
        let (current, version) = self.snapshot(key);
        if &current != expected {
            return false;
        }
        self.compare_and_set(key, version, new)
    }
```

- [ ] **Step 4: Install `cas` and `add` in `install`**

Add after the `update` closure:

```rust
    let s = store.clone();
    let cas = lua
        .create_function(move |lua, (key, expected, new): (Value, Value, Value)| {
            let key: mlua::String = argcheck::arg(lua, key, "lur.state.cas", 1, "string")?;
            reject_reentry()?;
            let expected = from_lua(&expected)?;
            let new = from_lua(&new)?;
            Ok(s.cas_value(&key.as_bytes(), &expected, new))
        })
        .map_err(RunError::Init)?;
    state.set("cas", cas).map_err(RunError::Init)?;

    let s = store.clone();
    let add = lua
        .create_function(move |lua, (key, value): (Value, Value)| {
            let key: mlua::String = argcheck::arg(lua, key, "lur.state.add", 1, "string")?;
            reject_reentry()?;
            let new = from_lua(&value)?;
            // add == cas(key, nil, value): set iff currently absent.
            Ok(s.cas_value(&key.as_bytes(), &None, new))
        })
        .map_err(RunError::Init)?;
    state.set("add", add).map_err(RunError::Init)?;
```

- [ ] **Step 5: Run the test + suite**

Run: `cargo nextest run --test state` then `cargo nextest run -E 'package(lur) and test(state)'`
Expected: PASS.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/capabilities/state.rs tests/state.rs
git commit -S -m "feat(state): value-based cas and add (set-if-absent)"
```

---

### Task 8: Documentation — GUIDE.md examples, README, ARCHITECTURE

Add runnable examples for every new function (the function-level guard forces this) and update the API reference + module map.

**Files:**
- Modify: `docs/GUIDE.md` (lur.kv, lur.state sections)
- Modify: `README.md` (lur.kv, lur.state, lur.db entries)
- Modify: `ARCHITECTURE.md` (module map row for kv.rs; busy/IMMEDIATE note)
- Test: `tests/guide.rs` (no edits; its drift guards run the new examples)

**Interfaces:**
- Consumes: all functions added in Tasks 3-7.

- [ ] **Step 1: Run the guide drift guard to see it fail**

Run: `cargo nextest run --test guide -E 'test(every_runtime_function_has_an_example)'`
Expected: FAIL — listing the new uncovered functions: `kv.add`, `kv.cas`, `kv.incr`, `kv.decr`, `kv.update`, `state.cas`, `state.add`, `state.decr`.

- [ ] **Step 2: Replace the `lur.kv` section body in `docs/GUIDE.md`**

Find the `### lur.kv` block and replace its example with one that exercises every function:

````markdown
### lur.kv

A key/value store over the same SQLite pool. Values are raw bytes; counters are
integers and read back as their decimal string. `get`/`set`/`delete` plus atomic
`add` (set-if-absent), `cas` (compare-and-set), `incr`/`decr`, and `update`
(read-modify-write).

```lua
lur.kv.set("greeting", "hi")
assert(lur.kv.get("greeting") == "hi")
lur.kv.delete("greeting")
assert(lur.kv.get("greeting") == nil)

-- set-if-absent
assert(lur.kv.add("once", "v") == true)
assert(lur.kv.add("once", "v2") == false)

-- compare-and-set: nil expected = "must be absent", nil new = delete
assert(lur.kv.cas("once", "v", "v2") == true)
assert(lur.kv.cas("once", "v", "x") == false)
assert(lur.kv.cas("once", "v2", nil) == true)

-- integer counters
assert(lur.kv.incr("hits") == 1)
assert(lur.kv.incr("hits", 4) == 5)
assert(lur.kv.decr("hits", 2) == 3)
assert(lur.kv.get("hits") == "3")

-- read-modify-write; return nil to delete
local n = lur.kv.update("hits", function(cur) return tostring(tonumber(cur) + 10) end)
assert(n == "13")
```
````

- [ ] **Step 3: Replace the `lur.state` section body in `docs/GUIDE.md`**

````markdown
### lur.state

Process-wide shared state across the VM pool (primitives only): `get`/`set`
(`nil` deletes), integer `incr`/`decr` (atomic), `add` (set-if-absent), `cas`
(value compare-and-set), and `update` (optimistic CAS with a transform).

```lua
lur.state.set("hits", 0)
assert(lur.state.incr("hits", 2) == 2)
assert(lur.state.decr("hits") == 1)
lur.state.update("hits", function(n) return (n or 0) + 1 end)
assert(lur.state.get("hits") == 2)

assert(lur.state.add("lock", "held") == true)
assert(lur.state.add("lock", "again") == false)
assert(lur.state.cas("lock", "held", nil) == true)
assert(lur.state.get("lock") == nil)

lur.state.set("hits", nil)
assert(lur.state.get("hits") == nil)
```
````

- [ ] **Step 4: Run the guide tests to confirm green**

Run: `cargo nextest run --test guide`
Expected: PASS — `every_runtime_function_has_an_example`, `every_runnable_example_succeeds`, and `every_capability_is_documented` all green.

- [ ] **Step 5: Update README API entries**

In `README.md`, update the `lur.kv` bullet to list `add`/`cas`/`incr`/`decr`/`update`, the `lur.state` bullet to list `decr`/`cas`/`add` and note `incr` is integer, and add one line to the `lur.db` description: transactions use `BEGIN IMMEDIATE` and wait out lock contention via a 5 s `busy_timeout`. Keep wording terse and consistent with the surrounding entries.

- [ ] **Step 6: Update ARCHITECTURE module map**

In `ARCHITECTURE.md`, add a row for `src/capabilities/kv.rs` (owns `lur.kv`; atomic ops over the shared pool), adjust the `db.rs` row to note it owns the pool + `begin_immediate`/`busy_timeout` and hands `SqliteShared` to kv, and add a one-line invariant: counters are integers; `kv.get` is type-aware and always returns bytes; write transactions are `BEGIN IMMEDIATE`.

- [ ] **Step 7: Full gate + commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run
git add docs/GUIDE.md README.md ARCHITECTURE.md
git commit -S -m "docs: document kv/state atomic ops and db busy handling"
```

---

## Self-Review notes (for the executor)

- **Spec coverage:** Task 1 = module split + type-aware get (spec §1, §2 get); Task 2 = db busy (§4); Task 3 = add/cas (§2); Task 4 = incr/decr (§2); Task 5 = update (§2); Tasks 6-7 = state parity + integer incr (§3); Task 8 = docs/tests (§testing). All spec components are covered.
- **Single-statement vs transaction:** add/cas/incr/decr are single atomic statements (busy_timeout only); update and db.tx use begin_immediate. Matches the corrected spec.
- **Type consistency:** `db::SqliteShared`, `db::ensure_pool`, `db::begin_immediate`, `kv::install`, `incr_by`, `IncrError`, `cas_value` names are used identically across tasks.
- **Concurrency caveat:** the kv concurrency stress test is intentionally omitted from the unit suite (nextest runs each test in its own process; a real multi-writer race needs threads sharing one db path). The single-process atomicity tests plus the SQLite single-statement guarantee cover correctness; a threaded stress test can be a follow-up if desired.
