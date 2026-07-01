//! Storage backend seam. `Backend` isolates all backend-specific code (SQL
//! dialect, binding, row mapping, concurrency) so `db.rs`/`kv.rs` stay
//! backend-neutral. `SQLite` is the only backend today; the `Postgres` variant
//! lands in Phase 2.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use mlua::{Error, Function, Lua, Table, Value};
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

    /// Transitional (removed in Task 3): raw `SQLite` pool for the not-yet-migrated
    /// kv code. Panics if a future non-SQLite backend reaches here.
    pub(crate) fn as_sqlite_pool(&self) -> &SqlitePool {
        match self {
            Backend::Sqlite(b) => b.pool(),
        }
    }

    pub(crate) async fn kv_update(
        &self,
        lua: &Lua,
        key: String,
        func: Function,
    ) -> mlua::Result<Value> {
        match self {
            Backend::Sqlite(b) => b.kv_update(lua, key, func).await,
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
        let path =
            self.path.as_ref().as_ref().ok_or_else(|| {
                Error::runtime("lur.db: no database configured; pass --db <path>")
            })?;
        let backend = Backend::Sqlite(SqliteBackend::open(path).await?);
        let _ = self.cell.set(backend);
        Ok(self.cell.get().expect("backend just set").clone())
    }
}
