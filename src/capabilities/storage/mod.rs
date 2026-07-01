//! Storage backend seam. `Backend` isolates all backend-specific code (SQL
//! dialect, binding, row mapping, concurrency) so `db.rs`/`kv.rs` stay
//! backend-neutral. `SQLite` is the only backend today; the `Postgres` variant
//! lands in Phase 2.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use mlua::{Error, Function, Lua, Table, Value};

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

    pub(crate) async fn kv_get(&self, lua: &Lua, key: String) -> mlua::Result<Value> {
        match self {
            Backend::Sqlite(b) => b.kv_get(lua, key).await,
        }
    }

    pub(crate) async fn kv_set(&self, key: String, value: Vec<u8>) -> mlua::Result<()> {
        match self {
            Backend::Sqlite(b) => b.kv_set(key, value).await,
        }
    }

    pub(crate) async fn kv_delete(&self, key: String) -> mlua::Result<()> {
        match self {
            Backend::Sqlite(b) => b.kv_delete(key).await,
        }
    }

    pub(crate) async fn kv_add(&self, key: String, value: Vec<u8>) -> mlua::Result<bool> {
        match self {
            Backend::Sqlite(b) => b.kv_add(key, value).await,
        }
    }

    pub(crate) async fn kv_cas(
        &self,
        key: String,
        expected: Option<Vec<u8>>,
        new: Option<Vec<u8>>,
    ) -> mlua::Result<bool> {
        match self {
            Backend::Sqlite(b) => b.kv_cas(key, expected, new).await,
        }
    }

    pub(crate) async fn kv_incr(
        &self,
        voice: &'static str,
        key: String,
        delta: i64,
    ) -> mlua::Result<i64> {
        match self {
            Backend::Sqlite(b) => b.kv_incr(voice, key, delta).await,
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
