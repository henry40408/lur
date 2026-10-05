//! Storage backend seam: keeps `db.rs`/`kv.rs` backend-neutral. The `--db`
//! scheme picks `SQLite` or `Postgres`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};

use mlua::{Error, Function, Lua, Table, Value};

pub(crate) mod postgres;
pub(crate) mod sqlite;

use postgres::{PgBackend, PgTransaction};
use sqlite::{SqliteBackend, SqliteTransaction};

/// Wall clock in epoch milliseconds; the only clock kv expiry uses, so every
/// backend agrees on "now" regardless of the database server's own clock.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Absolute expiry for a relative `ttl_ms`, saturating instead of overflowing.
pub(crate) fn expiry_at(now: i64, ttl_ms: Option<i64>) -> Option<i64> {
    ttl_ms.map(|t| now.saturating_add(t))
}

/// Rate-limits the expired-row sweep that piggybacks on kv writes.
#[derive(Clone, Default)]
pub(crate) struct PurgeClock(Arc<AtomicI64>);

impl PurgeClock {
    const INTERVAL_MS: i64 = 3_600_000;

    /// Starts the interval at `now` (the sweep done when the pool opened).
    pub(crate) fn started(now: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now)))
    }

    /// True at most once per interval; the winner claims the slot.
    pub(crate) fn due(&self, now: i64) -> bool {
        let last = self.0.load(Ordering::Relaxed);
        now - last >= Self::INTERVAL_MS
            && self
                .0
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }
}

/// Result of a write statement.
pub(crate) struct ExecResult {
    pub rows_affected: u64,
    pub last_insert_id: i64,
}

enum StorageTarget {
    Sqlite(std::path::PathBuf),
    Postgres(String),
}

impl StorageTarget {
    fn resolve(path: &std::path::Path) -> Self {
        match path.to_str() {
            Some(s) if s.starts_with("postgres://") || s.starts_with("postgresql://") => {
                StorageTarget::Postgres(s.to_owned())
            }
            _ => StorageTarget::Sqlite(path.to_path_buf()),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Backend {
    Sqlite(SqliteBackend),
    Postgres(PgBackend),
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
            Backend::Postgres(b) => b.exec(lua, sql, params).await,
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
            Backend::Postgres(b) => b.query(lua, sql, params).await,
        }
    }

    pub(crate) async fn begin(&self) -> mlua::Result<Transaction> {
        match self {
            Backend::Sqlite(b) => Ok(Transaction::Sqlite(b.begin().await?)),
            Backend::Postgres(b) => Ok(Transaction::Postgres(b.begin().await?)),
        }
    }

    pub(crate) async fn kv_get(&self, lua: &Lua, key: String) -> mlua::Result<Value> {
        match self {
            Backend::Sqlite(b) => b.kv_get(lua, key).await,
            Backend::Postgres(b) => b.kv_get(lua, key).await,
        }
    }

    pub(crate) async fn kv_set(
        &self,
        key: String,
        value: Vec<u8>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<()> {
        match self {
            Backend::Sqlite(b) => b.kv_set(key, value, ttl_ms).await,
            Backend::Postgres(b) => b.kv_set(key, value, ttl_ms).await,
        }
    }

    pub(crate) async fn kv_delete(&self, key: String) -> mlua::Result<()> {
        match self {
            Backend::Sqlite(b) => b.kv_delete(key).await,
            Backend::Postgres(b) => b.kv_delete(key).await,
        }
    }

    pub(crate) async fn kv_add(
        &self,
        key: String,
        value: Vec<u8>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<bool> {
        match self {
            Backend::Sqlite(b) => b.kv_add(key, value, ttl_ms).await,
            Backend::Postgres(b) => b.kv_add(key, value, ttl_ms).await,
        }
    }

    pub(crate) async fn kv_cas(
        &self,
        key: String,
        expected: Option<Vec<u8>>,
        new: Option<Vec<u8>>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<bool> {
        match self {
            Backend::Sqlite(b) => b.kv_cas(key, expected, new, ttl_ms).await,
            Backend::Postgres(b) => b.kv_cas(key, expected, new, ttl_ms).await,
        }
    }

    pub(crate) async fn kv_incr(
        &self,
        voice: &'static str,
        key: String,
        delta: i64,
        ttl: Option<Ttl>,
    ) -> mlua::Result<i64> {
        match self {
            Backend::Sqlite(b) => b.kv_incr(voice, key, delta, ttl).await,
            Backend::Postgres(b) => b.kv_incr(voice, key, delta, ttl).await,
        }
    }

    /// `false` when the key is absent or already expired.
    pub(crate) async fn kv_expire(&self, key: String, ttl_ms: i64) -> mlua::Result<bool> {
        match self {
            Backend::Sqlite(b) => b.kv_expire(key, ttl_ms).await,
            Backend::Postgres(b) => b.kv_expire(key, ttl_ms).await,
        }
    }

    /// `None`: absent/expired. `Some(None)`: no expiry. `Some(Some(ms))`: remaining.
    pub(crate) async fn kv_ttl(&self, key: String) -> mlua::Result<Option<Option<i64>>> {
        match self {
            Backend::Sqlite(b) => b.kv_ttl(key).await,
            Backend::Postgres(b) => b.kv_ttl(key).await,
        }
    }

    pub(crate) async fn kv_update(
        &self,
        lua: &Lua,
        key: String,
        func: Function,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<Value> {
        match self {
            Backend::Sqlite(b) => b.kv_update(lua, key, func, ttl_ms).await,
            Backend::Postgres(b) => b.kv_update(lua, key, func, ttl_ms).await,
        }
    }
}

/// `ttl_ms` for `kv_incr`, with its renew mode.
#[derive(Clone, Copy)]
pub(crate) struct Ttl {
    pub ms: i64,
    /// Reset the expiry on every call, not only when the key has none.
    pub renew: bool,
}

/// A write transaction over some backend.
pub(crate) enum Transaction {
    Sqlite(SqliteTransaction),
    Postgres(PgTransaction),
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
            Transaction::Postgres(t) => t.exec(lua, sql, params).await,
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
            Transaction::Postgres(t) => t.query(lua, sql, params).await,
        }
    }

    pub(crate) async fn commit(&self) -> mlua::Result<()> {
        match self {
            Transaction::Sqlite(t) => t.commit().await,
            Transaction::Postgres(t) => t.commit().await,
        }
    }

    pub(crate) async fn rollback(&self) {
        match self {
            Transaction::Sqlite(t) => t.rollback().await,
            Transaction::Postgres(t) => t.rollback().await,
        }
    }
}

/// Backend handle shared by `lur.db` and `lur.kv`, opened on first use.
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

    pub(crate) async fn ensure(&self) -> mlua::Result<Backend> {
        if let Some(b) = self.cell.get() {
            return Ok(b.clone());
        }
        let path =
            self.path.as_ref().as_ref().ok_or_else(|| {
                Error::runtime("lur.db: no database configured; pass --db <path>")
            })?;
        let backend = match StorageTarget::resolve(path) {
            StorageTarget::Sqlite(p) => Backend::Sqlite(SqliteBackend::open(&p).await?),
            StorageTarget::Postgres(url) => Backend::Postgres(PgBackend::open(&url).await?),
        };
        let _ = self.cell.set(backend);
        Ok(self.cell.get().expect("backend just set").clone())
    }
}

#[cfg(test)]
impl Shared {
    /// Wrap an already-open backend for tests that need a specific pool config.
    pub(crate) fn from_backend(backend: Backend) -> Self {
        let cell = Arc::new(OnceLock::new());
        let _ = cell.set(backend);
        Self {
            cell,
            path: Arc::new(None),
        }
    }
}

/// Single-connection `SQLite` backend for `db.rs` cancellation tests.
#[cfg(test)]
pub(crate) async fn sqlite_max1_backend(dir: &std::path::Path) -> Backend {
    Backend::Sqlite(sqlite::max1_backend(dir).await)
}
