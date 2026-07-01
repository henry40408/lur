//! `SQLite` storage backend: owns the `sqlx` `SQLite` pool and all
//! `SQLite`-specific SQL, `?` binding, row→Lua type mapping, WAL/busy
//! handling, and retry.

use std::future::Future;
use std::path::Path;

use mlua::{Error, Function, Lua, Table, Value};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteRow,
};
use sqlx::{Column, Row, Sqlite, TypeInfo, ValueRef};

use crate::capabilities::null;

/// A dynamically-bound `SQLite` query.
pub(crate) type Query<'q> = sqlx::query::Query<'q, sqlx::Sqlite, SqliteArguments>;

/// Retry policy for write-lock contention: 4 retries on top of the first try.
const MAX_BUSY_RETRIES: u32 = 4;

/// True when `e` is `SQLite` busy/locked (primary result codes 5/6, including
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
    let ceil = (BASE_MS << attempt.min(6)).clamp(1, CAP_MS);
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
/// outside `SQLite`.
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

/// Convert a result row to a Lua table keyed by column name (spec §6 read map).
pub(crate) fn read_row(lua: &Lua, row: &SqliteRow) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for col in row.columns() {
        let i = col.ordinal();
        let raw = row
            .try_get_raw(i)
            .map_err(|e| Error::runtime(format!("lur.db: {e}")))?;
        let value = if raw.is_null() {
            null::value(lua)?
        } else {
            match raw.type_info().name() {
                "INTEGER" => Value::Integer(get::<i64>(row, i)?),
                "REAL" => Value::Number(get::<f64>(row, i)?),
                // TEXT and BLOB both come back as raw bytes (§4 byte semantics).
                _ => Value::String(lua.create_string(get::<Vec<u8>>(row, i)?)?),
            }
        };
        t.set(col.name(), value)?;
    }
    Ok(t)
}

fn get<'r, T>(row: &'r SqliteRow, i: usize) -> mlua::Result<T>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get::<T, usize>(i)
        .map_err(|e| Error::runtime(format!("lur.db: decoding column {i}: {e}")))
}

/// Open the `SQLite` pool in WAL mode and ensure the internal `lur_kv` table.
pub(crate) async fn open_pool(path: &Path) -> sqlx::Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_millis(200))
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().connect_with(opts).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS lur_kv (key TEXT PRIMARY KEY, value BLOB)")
        .execute(&pool)
        .await?;
    Ok(pool)
}

/// Bind each Lua value as a positional parameter (spec §6 write mapping).
pub(crate) fn bind_all<'q>(mut q: Query<'q>, params: &[Value]) -> mlua::Result<Query<'q>> {
    for v in params {
        q = bind_one(q, v)?;
    }
    Ok(q)
}

fn bind_one<'q>(q: Query<'q>, v: &Value) -> mlua::Result<Query<'q>> {
    Ok(match v {
        Value::Nil => q.bind(None::<i64>),
        Value::UserData(_) if null::is_null(v) => q.bind(None::<i64>),
        Value::Boolean(b) => q.bind(*b as i64),
        Value::Integer(i) => q.bind(*i),
        Value::Number(n) => {
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                q.bind(*n as i64)
            } else {
                q.bind(*n)
            }
        }
        Value::String(s) => {
            let bytes = s.as_bytes();
            match std::str::from_utf8(&bytes) {
                Ok(text) => q.bind(text.to_owned()),
                Err(_) => q.bind(bytes.to_vec()),
            }
        }
        other => {
            return Err(Error::runtime(format!(
                "lur.db: cannot bind a {} value (encode tables with lur.json.encode)",
                other.type_name()
            )));
        }
    })
}

/// Decode column 0 of a single-column row into bytes, returning `None` for
/// NULL. INTEGER and REAL values are rendered as their decimal string; TEXT/BLOB
/// come back as raw bytes. Never panics on a type mismatch.
///
/// A later task (`kv.update`) reuses this helper for the same type dispatch.
pub(crate) fn value_to_bytes(row: &sqlx::sqlite::SqliteRow) -> mlua::Result<Option<Vec<u8>>> {
    let raw = row
        .try_get_raw(0)
        .map_err(|e| Error::runtime(format!("lur.kv: {e}")))?;
    if raw.is_null() {
        return Ok(None);
    }
    // Always hand back bytes: counters (INTEGER) and REAL render as their
    // decimal string; TEXT/BLOB are the raw bytes. Never a Vec<u8> type-mismatch
    // panic.
    let bytes: Vec<u8> = match raw.type_info().name() {
        "INTEGER" => decode::<i64>(row)?.to_string().into_bytes(),
        "REAL" => decode::<f64>(row)?.to_string().into_bytes(),
        _ => decode::<Vec<u8>>(row)?,
    };
    Ok(Some(bytes))
}

/// Decode column 0 of a single-column row, lur-voiced on failure.
pub(crate) fn decode<'r, T>(row: &'r sqlx::sqlite::SqliteRow) -> mlua::Result<T>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get::<T, usize>(0)
        .map_err(|e| Error::runtime(format!("lur.kv: decoding value: {e}")))
}

/// `SQLite` backend: owns the pool. Cloning is a cheap `sqlx` pool handle clone.
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
        let _ = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?;
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

    /// Read-modify-write orchestration for `lur.kv.update`: begins a
    /// `BEGIN IMMEDIATE` write on a pinned connection, reads the current value
    /// (type-aware, matching `kv_get`), calls `func`, then writes/deletes the
    /// result before committing — rolling back and re-raising on any error.
    /// The write binds the returned string as raw bytes (`Vec<u8>` → BLOB) so
    /// a value written by `update` compares equal under `lur.kv.cas`, which
    /// also binds BLOB; going through the generic `?`-bind path would store
    /// it as TEXT instead, which `SQLite` never treats as equal to a BLOB.
    pub(crate) async fn kv_update(
        &self,
        lua: &Lua,
        key: String,
        func: Function,
    ) -> mlua::Result<Value> {
        let mut conn = retry_busy(|| async {
            let mut conn = self.pool.acquire().await?;
            sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
            Ok(conn)
        })
        .await
        .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;

        // Run the full read → transform → write sequence, then commit/roll
        // back below based on its outcome.
        let result: mlua::Result<Value> = async {
            // read current value as bytes|nil (type-aware, same as kv_get)
            let cur: Value = match sqlx::query("SELECT value FROM lur_kv WHERE key = ?")
                .bind(&key)
                .fetch_optional(&mut *conn)
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
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                Value::String(s) => {
                    sqlx::query("INSERT OR REPLACE INTO lur_kv (key, value) VALUES (?, ?)")
                        .bind(&key)
                        .bind(s.as_bytes().to_vec())
                        .execute(&mut *conn)
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
                .execute(&mut *conn)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: commit: {e}")))?;
            Ok(new)
        }
        .await;

        // On any error from the sequence above, roll back the open
        // transaction and return the original error unchanged.
        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                Err(e)
            }
        }
    }
}

/// A pinned-connection `SQLite` write transaction. `exec`/`query` run on the
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
            sqlx::query("CREATE TABLE t (x)")
                .execute(&pool)
                .await
                .unwrap();

            let mut a = pool.acquire().await.unwrap();
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *a)
                .await
                .unwrap();
            let mut b = pool.acquire().await.unwrap();
            let busy = sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *b)
                .await
                .unwrap_err();
            assert!(is_busy(&busy), "SQLITE_BUSY not classified busy: {busy:?}");

            // A non-busy DB error (syntax) must classify as not-busy. Run it on
            // the connection we already hold: the pool (max 2) is fully checked
            // out by `a` and `b`, so `execute(&pool)` would block on acquire for
            // the 30 s timeout instead of reaching SQLite. `b`'s failed BEGIN
            // left no open transaction, so it is a usable connection.
            let syntax = sqlx::query("NOT VALID SQL")
                .execute(&mut *b)
                .await
                .unwrap_err();
            assert!(!is_busy(&syntax), "syntax error wrongly classified busy");
        });
    }
}
