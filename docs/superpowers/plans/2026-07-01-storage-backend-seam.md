# Storage backend seam (PostgreSQL Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract a storage-backend seam (`enum Backend { Sqlite(...) }`) that isolates all SQLite-specific code behind a defined method surface, as a pure refactor with SQLite as the sole implementation and zero user-visible behavior change.

**Architecture:** A new `src/capabilities/storage/` module owns the backend: `storage/sqlite.rs` holds `SqliteBackend` (pool + all SQLite dialect/binding/row-mapping/retry/tx/kv code); `storage/mod.rs` holds the `Backend` enum, the lazy-open `Shared` handle, and the neutral `ExecResult` / `Transaction` types. `db.rs` and `kv.rs` keep only their `install` functions, wiring Lua functions to `Backend` methods. Migration is incremental — move leaf helpers first, then the db surface, then the kv surface — with the unchanged test suite green at every commit.

**Tech Stack:** Rust (edition 2024), `sqlx` (SQLite), `mlua` (Luau), `tokio`.

## Global Constraints

- Edition 2024. MSRV / toolchain managed separately — do not bump MSRV.
- Run tests with `cargo nextest run` (NOT `cargo test`).
- `cargo fmt --all` before every commit; `cargo clippy --all-targets -- -D warnings` must pass with zero warnings.
- All commits GPG-signed; do not pass `--no-gpg-sign`.
- Stage files explicitly by name; never `git add -A` / `git add .`.
- **Zero user-visible behavior change.** The entire existing test suite passes **unchanged** — if any existing test needs editing to pass, the refactor changed behavior and must be reworked, not the test.
- **No new dependency, no new CLI flag or config field.** `RuntimeConfig.db_path` stays `Option<PathBuf>`.
- **Perf gate:** capture `cargo bench --bench runtime` baseline before any edit and compare at the end; no regression may be committed.
- **Single-variant enum is intentional** scaffolding for the Phase 2 `Postgres` variant. If clippy flags it under `-D warnings`, annotate with a commented `#[allow(...)]` noting Phase 2 — do not add filler structure.
- Commit messages end with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File Structure

- `src/capabilities/storage/mod.rs` — Create: `Backend` enum, `Shared` lazy-open handle, `ExecResult`, `Transaction` enum + their dispatch methods.
- `src/capabilities/storage/sqlite.rs` — Create: `SqliteBackend`, `SqliteTransaction`, and all moved SQLite leaf helpers.
- `src/capabilities/mod.rs` — Modify: declare `mod storage;`; update the `db::install` → `kv::install` wiring types (`SqliteShared` → `storage::Shared`).
- `src/capabilities/db.rs` — Modify: shrink to `install` only; wire `exec`/`query`/`tx` onto `Backend`.
- `src/capabilities/kv.rs` — Modify: shrink to `install` only; wire kv ops onto `Backend`; keep the `IN_KV_UPDATE` reentrancy guard.
- `ARCHITECTURE.md` — Modify: module-map rows + storage note.

---

### Task 1: Move SQLite leaf helpers into a new `storage/sqlite.rs`

Pure code move — no seam types yet. Reduces `db.rs`, sets up the module. `SqliteShared` and the pool lifecycle stay in `db.rs` for now.

**Files:**
- Create: `src/capabilities/storage/mod.rs`, `src/capabilities/storage/sqlite.rs`
- Modify: `src/capabilities/mod.rs`, `src/capabilities/db.rs`, `src/capabilities/kv.rs`
- Baseline artifact: `scratchpad/bench-baseline.txt`

**Interfaces:**
- Produces (all `pub(crate)` in `storage::sqlite`): `bind_all`, `bind_one`, `read_row`, `retry_busy`, `is_busy`, `jitter_delay`, `open_pool`, `value_to_bytes`, `decode`, plus the type alias `Query<'q>`.

- [ ] **Step 1: Capture the perf baseline BEFORE any edit**

Run (on the current unmodified branch):
```bash
cargo bench --bench runtime | tee "$SCRATCH/bench-baseline.txt"
```
where `$SCRATCH` is the session scratchpad dir. This file is compared in Task 4. Do not edit any source before this completes.

- [ ] **Step 2: Create the `storage` module skeleton**

Create `src/capabilities/storage/mod.rs`:
```rust
//! Storage backend seam. `Backend` isolates all backend-specific code (SQL
//! dialect, binding, row mapping, concurrency) so `db.rs`/`kv.rs` stay
//! backend-neutral. SQLite is the only backend today; the `Postgres` variant
//! lands in Phase 2.

pub(crate) mod sqlite;
```

Create `src/capabilities/storage/sqlite.rs` with just the module doc for now:
```rust
//! SQLite storage backend: owns the `sqlx` SQLite pool and all SQLite-specific
//! SQL, `?` binding, row→Lua type mapping, WAL/busy handling, and retry.
```

In `src/capabilities/mod.rs`, add the module declaration next to the other capability modules (find the `mod db;` / `mod kv;` lines and add alongside):
```rust
mod storage;
```

- [ ] **Step 3: Move the leaf helpers verbatim into `storage/sqlite.rs`**

Move these items **verbatim** from `src/capabilities/db.rs` into `src/capabilities/storage/sqlite.rs`, changing only their visibility to `pub(crate)` and adding the imports they need:

- `type Query<'q>` (db.rs:21) → `pub(crate) type Query<'q>`
- `const MAX_BUSY_RETRIES` (db.rs:115)
- `fn is_busy` (db.rs:119-133)
- `fn jitter_delay` (db.rs:135-151)
- `fn retry_busy` (db.rs:153-173) — already `pub(crate)`
- `fn read_row` (db.rs:270-290) → `pub(crate) fn read_row`
- `fn get` (db.rs:292-299) → keep private to sqlite.rs (`fn get`)
- `fn open_pool` (db.rs:319-331) → `pub(crate) async fn open_pool`
- `fn bind_all` (db.rs:333-338) — already `pub(crate)`
- `fn bind_one` (db.rs:340-361)
- the entire `#[cfg(test)] mod tests` with `is_busy_classifies_sqlite_lock_errors` (db.rs:374-419)

Also move from `src/capabilities/kv.rs`:
- `fn value_to_bytes` (kv.rs:385-402) → `pub(crate) fn value_to_bytes`
- `fn decode` (kv.rs:404-end) → `pub(crate) fn decode`

Add to the top of `storage/sqlite.rs` the imports these need:
```rust
use std::future::Future;
use std::path::Path;

use mlua::{Error, Lua, Table, Value};
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteRow,
};
use sqlx::{Column, Row, TypeInfo, ValueRef};

use crate::capabilities::null;
```

- [ ] **Step 4: Repoint `db.rs` and `kv.rs` at the moved helpers**

In `src/capabilities/db.rs`: delete the moved items (Step 3 list). Where the remaining code referenced them, qualify via the new path. Add near the top:
```rust
use crate::capabilities::storage::sqlite::{bind_all, read_row, retry_busy, open_pool};
```
`ensure_pool` / `begin_immediate` / `run_tx` / `install` / `SqliteShared` stay in `db.rs`. `open_pool` is now called as the imported `open_pool`; `begin_immediate` and `exec`/`query`/`run_tx` call `bind_all`/`read_row`/`retry_busy` from the import.

In `src/capabilities/kv.rs`: delete `value_to_bytes` and `decode`; add:
```rust
use crate::capabilities::storage::sqlite::{value_to_bytes, retry_busy};
```
(kv.rs already used `db::retry_busy`; switch those call sites to the imported `retry_busy`, or qualify as `crate::capabilities::storage::sqlite::retry_busy`. Grep: `rg -n 'db::retry_busy' src/capabilities/kv.rs`.)

- [ ] **Step 5: Build, lint, run the full suite**

Run:
```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run
```
Expected: compiles with zero warnings; **all tests pass unchanged** (same count as before, currently 220).

- [ ] **Step 6: Commit**

```bash
git add src/capabilities/storage/mod.rs src/capabilities/storage/sqlite.rs src/capabilities/mod.rs src/capabilities/db.rs src/capabilities/kv.rs
git commit -m "refactor(storage): move SQLite leaf helpers into storage/sqlite.rs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Introduce `Backend`/`Shared`/`Transaction` and migrate `lur.db`

**Files:**
- Modify: `src/capabilities/storage/mod.rs`, `src/capabilities/storage/sqlite.rs`, `src/capabilities/db.rs`, `src/capabilities/kv.rs`, `src/capabilities/mod.rs`

**Interfaces:**
- Consumes: the Task 1 leaf helpers.
- Produces:
  - `pub(crate) struct ExecResult { pub rows_affected: u64, pub last_insert_id: i64 }`
  - `#[derive(Clone)] pub(crate) enum Backend { Sqlite(SqliteBackend) }` with `async fn exec(&self, &Lua, String, Vec<Value>) -> mlua::Result<ExecResult>`, `async fn query(&self, &Lua, String, Vec<Value>) -> mlua::Result<Table>`, `async fn begin(&self) -> mlua::Result<Transaction>`, and a transitional `pub(crate) fn as_sqlite_pool(&self) -> &SqlitePool` (removed in Task 3).
  - `pub(crate) enum Transaction { Sqlite(SqliteTransaction) }` with `async fn exec(&self, &Lua, String, Vec<Value>) -> mlua::Result<ExecResult>`, `async fn query(&self, &Lua, String, Vec<Value>) -> mlua::Result<Table>`, `async fn commit(&self) -> mlua::Result<()>`, `async fn rollback(&self)`.
  - `#[derive(Clone)] pub(crate) struct Shared { cell: Arc<OnceLock<Backend>>, path: Arc<Option<PathBuf>> }` with `pub(crate) async fn ensure(&self) -> mlua::Result<Backend>` and `pub(crate) fn new(db_path: Option<PathBuf>) -> Shared`.
  - `pub(crate) struct SqliteBackend { pool: SqlitePool }` with `async fn open(&Path)`, `exec`, `query`, `begin`, `pool()`.
  - `pub(crate) struct SqliteTransaction { conn: tokio::sync::Mutex<Option<PoolConnection<Sqlite>>> }`.

- [ ] **Step 1: Add `SqliteBackend`, `SqliteTransaction`, and their db methods to `storage/sqlite.rs`**

Add (moving the transaction/pool logic out of `db.rs`):
```rust
use sqlx::pool::PoolConnection;
use sqlx::Sqlite;

/// SQLite backend: owns the pool. Cloning is a cheap `sqlx` pool handle clone.
#[derive(Clone)]
pub(crate) struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    /// Open the WAL-mode pool and ensure the internal `lur_kv` table.
    pub(crate) async fn open(path: &Path) -> mlua::Result<Self> {
        let pool = open_pool(path)
            .await
            .map_err(|e| Error::runtime(format!("lur.db: opening {}: {e}", path.display())))?;
        Ok(Self { pool })
    }

    /// Transitional (Task 3 removes it): raw pool access for kv until kv migrates.
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub(crate) async fn exec(
        &self,
        _lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<super::ExecResult> {
        // Validate binds once (non-retryable), then retry the execute.
        bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?;
        let res = retry_busy(|| async {
            bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)
                .expect("params validated before retry loop")
                .execute(&self.pool)
                .await
        })
        .await
        .map_err(|e| Error::runtime(format!("lur.db.exec: {e}")))?;
        Ok(super::ExecResult {
            rows_affected: res.rows_affected(),
            last_insert_id: res.last_insert_rowid(),
        })
    }

    pub(crate) async fn query(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<Table> {
        let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.query: {e}")))?;
        let out = lua.create_table()?;
        for (i, row) in rows.iter().enumerate() {
            out.raw_set(i as i64 + 1, read_row(lua, row)?)?;
        }
        Ok(out)
    }

    /// Open a `BEGIN IMMEDIATE` write transaction on a pinned connection,
    /// retrying the lock acquisition on busy.
    pub(crate) async fn begin(&self) -> mlua::Result<SqliteTransaction> {
        let conn = retry_busy(|| async {
            let mut conn = self.pool.acquire().await?;
            sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
            Ok(conn)
        })
        .await
        .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        Ok(SqliteTransaction {
            conn: tokio::sync::Mutex::new(Some(conn)),
        })
    }
}

/// A pinned-connection SQLite write transaction. `exec`/`query` run on the
/// pinned connection; `commit`/`rollback` take it. A call after finish errors.
pub(crate) struct SqliteTransaction {
    conn: tokio::sync::Mutex<Option<PoolConnection<Sqlite>>>,
}

impl SqliteTransaction {
    pub(crate) async fn exec(
        &self,
        _lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<super::ExecResult> {
        let mut guard = self.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
        let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .execute(&mut **conn)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx exec: {e}")))?;
        Ok(super::ExecResult {
            rows_affected: res.rows_affected(),
            last_insert_id: res.last_insert_rowid(),
        })
    }

    pub(crate) async fn query(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<Table> {
        let mut guard = self.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
        let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .fetch_all(&mut **conn)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx query: {e}")))?;
        let out = lua.create_table()?;
        for (i, row) in rows.iter().enumerate() {
            out.raw_set(i as i64 + 1, read_row(lua, row)?)?;
        }
        Ok(out)
    }

    pub(crate) async fn commit(&self) -> mlua::Result<()> {
        let mut guard = self.conn.lock().await;
        if let Some(mut conn) = guard.take()
            && let Err(e) = sqlx::query("COMMIT").execute(&mut *conn).await
        {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            return Err(Error::runtime(format!("lur.db.tx: commit: {e}")));
        }
        Ok(())
    }

    pub(crate) async fn rollback(&self) {
        let mut guard = self.conn.lock().await;
        if let Some(mut conn) = guard.take() {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
        }
    }
}
```

- [ ] **Step 2: Add the `Backend`/`Transaction`/`Shared`/`ExecResult` seam to `storage/mod.rs`**

Replace `storage/mod.rs` contents with:
```rust
//! Storage backend seam. `Backend` isolates all backend-specific code (SQL
//! dialect, binding, row mapping, concurrency) so `db.rs`/`kv.rs` stay
//! backend-neutral. SQLite is the only backend today; the `Postgres` variant
//! lands in Phase 2.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use mlua::{Error, Lua, Table, Value};
use sqlx::sqlite::SqlitePool;

pub(crate) mod sqlite;

use sqlite::{SqliteBackend, SqliteTransaction};

/// Result of a write statement.
pub(crate) struct ExecResult {
    pub rows_affected: u64,
    pub last_insert_id: i64,
}

/// A storage backend. One variant today; `Postgres` is added in Phase 2, which
/// only extends the match arms below — no `db.rs`/`kv.rs` call site changes.
#[derive(Clone)]
pub(crate) enum Backend {
    Sqlite(SqliteBackend),
}

impl Backend {
    pub(crate) async fn exec(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<ExecResult> {
        match self {
            Backend::Sqlite(b) => b.exec(lua, sql, params).await,
        }
    }

    pub(crate) async fn query(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<Table> {
        match self {
            Backend::Sqlite(b) => b.query(lua, sql, params).await,
        }
    }

    pub(crate) async fn begin(&self) -> mlua::Result<Transaction> {
        match self {
            Backend::Sqlite(b) => Ok(Transaction::Sqlite(b.begin().await?)),
        }
    }

    /// Transitional (removed in Task 3): raw SQLite pool for the not-yet-migrated
    /// kv code. Panics if a future non-SQLite backend reaches here.
    pub(crate) fn as_sqlite_pool(&self) -> &SqlitePool {
        match self {
            Backend::Sqlite(b) => b.pool(),
        }
    }
}

/// A write transaction over some backend.
pub(crate) enum Transaction {
    Sqlite(SqliteTransaction),
}

impl Transaction {
    pub(crate) async fn exec(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<ExecResult> {
        match self {
            Transaction::Sqlite(t) => t.exec(lua, sql, params).await,
        }
    }

    pub(crate) async fn query(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<Table> {
        match self {
            Transaction::Sqlite(t) => t.query(lua, sql, params).await,
        }
    }

    pub(crate) async fn commit(&self) -> mlua::Result<()> {
        match self {
            Transaction::Sqlite(t) => t.commit().await,
        }
    }

    pub(crate) async fn rollback(&self) {
        match self {
            Transaction::Sqlite(t) => t.rollback().await,
        }
    }
}

/// Lazily-opened backend handle shared by `lur.db` and `lur.kv`. Cheaply
/// cloneable; the backend opens on first use.
#[derive(Clone)]
pub(crate) struct Shared {
    cell: Arc<OnceLock<Backend>>,
    path: Arc<Option<PathBuf>>,
}

impl Shared {
    pub(crate) fn new(db_path: Option<PathBuf>) -> Self {
        Self {
            cell: Arc::new(OnceLock::new()),
            path: Arc::new(db_path),
        }
    }

    /// Get the backend, opening it on first use. Errors clearly when no `--db`.
    pub(crate) async fn ensure(&self) -> mlua::Result<Backend> {
        if let Some(b) = self.cell.get() {
            return Ok(b.clone());
        }
        let path = self
            .path
            .as_ref()
            .as_ref()
            .ok_or_else(|| Error::runtime("lur.db: no database configured; pass --db <path>"))?;
        let backend = Backend::Sqlite(SqliteBackend::open(path).await?);
        let _ = self.cell.set(backend);
        Ok(self.cell.get().expect("backend just set").clone())
    }
}
```

- [ ] **Step 3: Rewrite `db.rs` `install` onto the seam**

Replace the whole of `src/capabilities/db.rs` with an `install` that returns `storage::Shared` and wires `exec`/`query`/`tx` through `Backend`. (`ensure_pool`, `begin_immediate`, `run_tx`, `SqliteShared`, `bind_all`/`read_row`/etc. are gone from this file.)

```rust
//! `lur.db` — long-term SQLite storage. This file only wires the Lua `lur.db`
//! table to the backend-neutral `storage::Backend`; all SQL/dialect lives in
//! `storage`.

use mlua::{Function, Lua, MultiValue, Table, Value, Variadic};
use std::path::PathBuf;
use std::sync::Arc;

use crate::capabilities::storage::Shared;
use crate::runtime::RunError;

/// Install `lur.db`. Returns the shared backend handle so `kv::install` reuses it.
pub fn install(lua: &Lua, lur: &Table, db_path: Option<PathBuf>) -> Result<Shared, RunError> {
    let shared = Shared::new(db_path);
    let db = lua.create_table().map_err(RunError::Init)?;

    {
        let shared = shared.clone();
        let exec = lua
            .create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = shared.clone();
                async move {
                    let backend = shared.ensure().await?;
                    let res = backend.exec(&lua, sql, params.into_iter().collect()).await?;
                    let t = lua.create_table()?;
                    t.set("rows_affected", res.rows_affected)?;
                    t.set("last_insert_id", res.last_insert_id)?;
                    Ok(t)
                }
            })
            .map_err(RunError::Init)?;
        db.set("exec", exec).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let query = lua
            .create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = shared.clone();
                async move {
                    let backend = shared.ensure().await?;
                    backend.query(&lua, sql, params.into_iter().collect()).await
                }
            })
            .map_err(RunError::Init)?;
        db.set("query", query).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let tx = lua
            .create_async_function(move |lua, func: Function| {
                let shared = shared.clone();
                async move { run_tx(lua, &shared, func).await }
            })
            .map_err(RunError::Init)?;
        db.set("tx", tx).map_err(RunError::Init)?;
    }

    lur.set("db", db).map_err(RunError::Init)?;
    Ok(shared)
}

/// Build the Lua `tx` handle over a pinned-connection transaction, run
/// `func(tx)`, then commit on normal return / roll back and re-raise on error.
async fn run_tx(lua: Lua, shared: &Shared, func: Function) -> mlua::Result<()> {
    let backend = shared.ensure().await?;
    let tx = Arc::new(backend.begin().await?);

    let handle = lua.create_table()?;
    {
        let tx = Arc::clone(&tx);
        let exec = lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
            let tx = Arc::clone(&tx);
            async move {
                let res = tx.exec(&lua, sql, params.into_iter().collect()).await?;
                let t = lua.create_table()?;
                t.set("rows_affected", res.rows_affected)?;
                t.set("last_insert_id", res.last_insert_id)?;
                Ok(t)
            }
        })?;
        handle.set("exec", exec)?;
    }
    {
        let tx = Arc::clone(&tx);
        let query = lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
            let tx = Arc::clone(&tx);
            async move { tx.query(&lua, sql, params.into_iter().collect()).await }
        })?;
        handle.set("query", query)?;
    }

    match func.call_async::<MultiValue>(handle).await {
        Ok(_) => tx.commit().await,
        Err(e) => {
            tx.rollback().await;
            Err(e)
        }
    }
}
```

- [ ] **Step 4: Point `kv.rs` and `mod.rs` at `Shared` (transitional pool access)**

In `src/capabilities/mod.rs`, the wiring stays shape-identical, only the type flows as `storage::Shared`:
```rust
    let shared = db::install(lua, &lur, config.db_path.clone())?;
    kv::install(lua, &lur, &shared)?;
```
(If `mod.rs` named the local `sqlite`/annotated the type, rename to `shared` / drop the annotation.)

In `src/capabilities/kv.rs`, replace `use super::db::{self, SqliteShared};` with:
```rust
use crate::capabilities::storage::Shared;
use crate::capabilities::storage::sqlite::{retry_busy, value_to_bytes};
```
Change `install`'s signature from `shared: &SqliteShared` to `shared: &Shared`. Everywhere kv currently obtained a pool via `db::ensure_pool(&cell, &path)`, obtain it transitionally via:
```rust
let backend = shared.ensure().await?;
let pool = backend.as_sqlite_pool();
```
and keep the existing kv SQL against `pool` unchanged. The `shared.cell` / `shared.path` field clones kv did (e.g. `Arc::clone(&shared.cell)`) become a single `let shared = shared.clone();` captured into each async function (Shared is `Clone`). For `kv.update`'s `db::begin_immediate`, replace with `backend.begin().await?` and drive the transaction through the `Transaction` handle's `exec`/`query` — OR, to keep Task 2 a pure db-migration, leave `kv.update` using a transitional path: `let tx = backend.begin().await?;` then use `tx.exec(&lua, ...)` / `tx.query(&lua, ...)` / `tx.commit()` / `tx.rollback()` with the existing kv SQL strings. Keep the `IN_KV_UPDATE` guard exactly as-is.

Grep to confirm no stale references remain: `rg -n 'db::|SqliteShared|ensure_pool|begin_immediate' src/capabilities/kv.rs` → only the intended new paths.

- [ ] **Step 5: Build, lint, full suite**

Run:
```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run
```
Expected: zero warnings; all tests pass unchanged (220).

- [ ] **Step 6: Commit**

```bash
git add src/capabilities/storage/mod.rs src/capabilities/storage/sqlite.rs src/capabilities/db.rs src/capabilities/kv.rs src/capabilities/mod.rs
git commit -m "refactor(storage): add Backend seam and migrate lur.db onto it

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Migrate `lur.kv` onto backend methods; remove the transitional accessor

**Files:**
- Modify: `src/capabilities/storage/mod.rs`, `src/capabilities/storage/sqlite.rs`, `src/capabilities/kv.rs`

**Interfaces:**
- Consumes: `Backend`, `Shared`, `Transaction` from Task 2.
- Produces on `Backend` (dispatching to `SqliteBackend`): `kv_get(&Lua, String) -> Value`, `kv_set(String, Vec<u8>)`, `kv_delete(String)`, `kv_add(String, Vec<u8>) -> bool`, `kv_cas(String, Option<Vec<u8>>, Option<Vec<u8>>) -> bool`, `kv_incr(&'static str voice, String, i64) -> i64`, `kv_update(&Lua, String, Function) -> Value`. The `as_sqlite_pool` accessor and `Transaction`'s use from kv are removed.

- [ ] **Step 1: Add the kv methods to `SqliteBackend` (move kv SQL out of `kv.rs`)**

In `storage/sqlite.rs`, add to `impl SqliteBackend` the kv operations, moving the SQL bodies from the current `kv.rs` install closures and `incr_by`:

- `kv_get` — from kv.rs get closure (`SELECT value ... fetch_optional`, then `value_to_bytes`). Signature `pub(crate) async fn kv_get(&self, lua: &Lua, key: String) -> mlua::Result<Value>`.
- `kv_set` — `INSERT OR REPLACE`. `pub(crate) async fn kv_set(&self, key: String, value: Vec<u8>) -> mlua::Result<()>`.
- `kv_delete` — `DELETE`. `pub(crate) async fn kv_delete(&self, key: String) -> mlua::Result<()>`.
- `kv_add` — retry-wrapped `INSERT ... ON CONFLICT DO NOTHING`, returns `rows_affected() == 1`. `pub(crate) async fn kv_add(&self, key: String, value: Vec<u8>) -> mlua::Result<bool>`.
- `kv_cas` — the four match arms (three retry-wrapped writes + the read-only `(None,None)` arm), moved verbatim from kv.rs cas. `pub(crate) async fn kv_cas(&self, key: String, expected: Option<Vec<u8>>, new: Option<Vec<u8>>) -> mlua::Result<bool>`.
- `kv_incr` — the body of the current `incr_by` (retry-wrapped `INSERT ... ON CONFLICT DO UPDATE ... typeof=integer ... RETURNING`), with the `voice` param kept. `pub(crate) async fn kv_incr(&self, voice: &'static str, key: String, delta: i64) -> mlua::Result<i64>`.
- `kv_update` — the read-modify-write orchestration from the current kv.rs update closure (kv.rs:250-323), owning `begin` → type-aware read → `func.call_async` → write/delete → commit/rollback. **Remove** the `IN_KV_UPDATE.with(...set...)` lines from this moved body — the reentrancy guard stays in `kv.rs` (Step 2). Signature `pub(crate) async fn kv_update(&self, lua: &Lua, key: String, func: Function) -> mlua::Result<Value>`. It uses `self.pool` and `begin_immediate`-style acquisition; reuse `retry_busy` for the initial `BEGIN IMMEDIATE`, matching `SqliteBackend::begin`.

All use `self.pool`, `retry_busy`, `bind`/`value_to_bytes` already in this file. Add `use mlua::Function;` to the imports.

Then add matching dispatch methods to `impl Backend` in `storage/mod.rs`, each a one-arm `match`:
```rust
    pub(crate) async fn kv_get(&self, lua: &Lua, key: String) -> mlua::Result<Value> {
        match self { Backend::Sqlite(b) => b.kv_get(lua, key).await }
    }
    pub(crate) async fn kv_set(&self, key: String, value: Vec<u8>) -> mlua::Result<()> {
        match self { Backend::Sqlite(b) => b.kv_set(key, value).await }
    }
    pub(crate) async fn kv_delete(&self, key: String) -> mlua::Result<()> {
        match self { Backend::Sqlite(b) => b.kv_delete(key).await }
    }
    pub(crate) async fn kv_add(&self, key: String, value: Vec<u8>) -> mlua::Result<bool> {
        match self { Backend::Sqlite(b) => b.kv_add(key, value).await }
    }
    pub(crate) async fn kv_cas(
        &self, key: String, expected: Option<Vec<u8>>, new: Option<Vec<u8>>,
    ) -> mlua::Result<bool> {
        match self { Backend::Sqlite(b) => b.kv_cas(key, expected, new).await }
    }
    pub(crate) async fn kv_incr(
        &self, voice: &'static str, key: String, delta: i64,
    ) -> mlua::Result<i64> {
        match self { Backend::Sqlite(b) => b.kv_incr(voice, key, delta).await }
    }
    pub(crate) async fn kv_update(
        &self, lua: &Lua, key: String, func: Function,
    ) -> mlua::Result<Value> {
        match self { Backend::Sqlite(b) => b.kv_update(lua, key, func).await }
    }
```
Add `use mlua::Function;` to `storage/mod.rs` imports.

- [ ] **Step 2: Rewrite `kv.rs` `install` onto the backend methods**

`kv.rs` keeps: the `IN_KV_UPDATE` thread-local + `reject_kv_reentry`, `argcheck::integer_arg` use, and the install wiring. Each Lua function now: `reject_kv_reentry(...)?` → validate args → `let backend = shared.ensure().await?;` → call the matching `backend.kv_*`. For `update`, wrap the guard around the backend call:
```rust
            let update = lua
                .create_async_function(move |lua, (key, func): (String, Function)| {
                    let shared = shared.clone();
                    async move {
                        reject_kv_reentry("lur.kv.update")?;
                        let backend = shared.ensure().await?;
                        IN_KV_UPDATE.with(|f| f.set(true));
                        let result = backend.kv_update(&lua, key, func).await;
                        IN_KV_UPDATE.with(|f| f.set(false));
                        result
                    }
                })
                .map_err(RunError::Init)?;
```
`incr`/`decr` keep their arg handling and pass the voice + computed delta:
```rust
// incr:
let n = argcheck::integer_arg(n, "lur.kv.incr", 2)?;
backend.kv_incr("lur.kv.incr", key, n.unwrap_or(1)).await
// decr:
let n = argcheck::integer_arg(n, "lur.kv.decr", 2)?;
let delta = n.unwrap_or(1).checked_neg()
    .ok_or_else(|| Error::runtime("lur.kv.decr: step too large"))?;
backend.kv_incr("lur.kv.decr", key, delta).await
```
Drop the now-unused `use ...::{retry_busy, value_to_bytes}` import from kv.rs (those are used inside the backend now). Keep `use ...storage::Shared;`.

- [ ] **Step 3: Remove the transitional `as_sqlite_pool` accessor**

Delete `Backend::as_sqlite_pool` from `storage/mod.rs` and the unused `use sqlx::sqlite::SqlitePool;` import if it is now unused. Confirm nothing references it: `rg -n 'as_sqlite_pool' src/`.

- [ ] **Step 4: Build, lint, full suite (repeat concurrency guards)**

Run:
```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo nextest run
```
Expected: zero warnings; all tests pass unchanged (220). Then run the concurrency guards a few times to confirm stability under the refactor:
```bash
for i in 1 2 3; do cargo nextest run --test db -E 'test(concurrent) or test(kv_incr_is_atomic)' || break; done
```

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/storage/mod.rs src/capabilities/storage/sqlite.rs src/capabilities/kv.rs
git commit -m "refactor(storage): migrate lur.kv onto Backend methods; drop transitional pool accessor

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Perf gate + docs + final verification

**Files:**
- Modify: `ARCHITECTURE.md`

- [ ] **Step 1: Compare the perf benchmark against the Task 1 baseline**

Run:
```bash
cargo bench --bench runtime | tee "$SCRATCH/bench-after.txt"
```
Compare against `$SCRATCH/bench-baseline.txt`. Expected: within noise (criterion typically reports "No change in performance" / changes under ~±3%). If any benchmark regresses meaningfully (> ~5% and outside the noise band), STOP and report — a regression must not be committed. Note the comparison outcome in the commit message body.

- [ ] **Step 2: Update `ARCHITECTURE.md` module map**

Add a row for the new module and adjust the `db.rs`/`kv.rs` rows. Find the module-map table rows for `src/capabilities/db.rs` and `src/capabilities/kv.rs` and replace them with:
```
| `src/capabilities/storage/` | The storage backend seam: `Backend` enum (SQLite today; Postgres reserved for Phase 2), the lazy-open `Shared` handle, `ExecResult`/`Transaction`, and `sqlite.rs` owning all SQLite SQL/binding/row-mapping/retry/tx/kv. |
| `src/capabilities/db.rs` | Wires the `lur.db` table (`exec`/`query`/`tx`) to `storage::Backend`. Returns `storage::Shared` to `kv`. |
| `src/capabilities/kv.rs` | Wires the `lur.kv` table to `storage::Backend` kv methods; owns the `IN_KV_UPDATE` reentrancy guard. |
```

- [ ] **Step 3: Update the `ARCHITECTURE.md` storage note**

In the `lur.db` storage note (the paragraph mentioning `begin_immediate` / `busy_timeout` / `retry_busy`), prepend a sentence establishing the seam:
```
Storage goes through a backend seam (`capabilities/storage`): `db.rs`/`kv.rs` call the
backend-neutral `Backend` enum, and `storage/sqlite.rs` owns every SQLite specific — the
`retry_busy`/`busy_timeout`/`BEGIN IMMEDIATE` handling described here included. The kv
logical value model (opaque bytes vs. integer counter) is backend-neutral and defined at
the seam.
```
Keep the existing detailed sentences that follow (they still describe the SQLite backend's behavior).

- [ ] **Step 4: Final full gate**

Run:
```bash
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo nextest run
```
Expected: clean; all pass.

- [ ] **Step 5: Commit**

```bash
git add ARCHITECTURE.md
git commit -m "docs(storage): document the backend seam in ARCHITECTURE

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

Then hand off to `superpowers:finishing-a-development-branch`.

---

## Self-Review

**Spec coverage:**
- Backend seam isolating SQLite behind a method surface → Tasks 1–3. ✓
- `db.rs`/`kv.rs` no longer reference `sqlx::sqlite::*` directly → all such code moved to `storage/sqlite.rs` (Tasks 1–3); verify at end with `rg -n 'sqlx::sqlite|SqliteRow|SqlitePool' src/capabilities/db.rs src/capabilities/kv.rs` → no hits. ✓
- Zero user-visible change → every task asserts the existing suite passes unchanged. ✓
- Enum extends to Postgres by adding match arms only → `Backend`/`Transaction` dispatch shape (Task 2/3). ✓
- Module layout (`storage/mod.rs` + `storage/sqlite.rs`) → Task 1/2. ✓
- `db.tx` via `Transaction`; `kv.update` via `kv_update` owning orchestration, guard stays in kv.rs → Task 2/3. ✓
- kv neutral value model + documented → Task 4 Step 3. ✓
- Perf gate → Task 1 Step 1 baseline + Task 4 Step 1 compare. ✓
- `RuntimeConfig.db_path` unchanged, no new flag/dep → constraints honored throughout. ✓
- Single-variant enum allowance → Global Constraints. ✓

**Placeholder scan:** No TBD/TODO; moved code cited by exact source range (a move, not a placeholder); all new glue shown in full. ✓

**Type consistency:** `ExecResult { rows_affected: u64, last_insert_id: i64 }`, `Backend`/`Transaction`/`Shared` signatures, and the `kv_*` method signatures are defined in Task 2/3 and consumed verbatim by `db.rs`/`kv.rs`. `Shared::new`/`ensure` and `SqliteBackend::open`/`pool`/`begin` names are consistent across tasks. ✓

**Note on the transitional accessor:** `as_sqlite_pool` is introduced in Task 2 and removed in Task 3 — a deliberate, clearly-commented bridge so `lur.db` and `lur.kv` migrate in separate reviewable tasks. It does not survive the branch.
