//! `SQLite` storage backend: pool, SQL, `?` binding, row→Lua mapping, busy retry.

use std::future::Future;
use std::path::Path;

use mlua::{Error, Function, Lua, Table, Value};
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteRow,
};
use sqlx::{Column, Row, Sqlite, TypeInfo, ValueRef};

use super::{PurgeClock, Ttl, expiry_at, now_ms};
use crate::capabilities::null;

/// A dynamically-bound `SQLite` query.
pub(crate) type Query<'q> = sqlx::query::Query<'q, sqlx::Sqlite, SqliteArguments>;

/// Retries on write-lock contention, on top of the first try.
const MAX_BUSY_RETRIES: u32 = 4;

/// `SQLITE_BUSY`/`SQLITE_LOCKED` (codes 5/6); extended variants match by message.
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

/// Full-jitter backoff: uniform in `[0, min(200 ms, 5 ms·2^attempt))`.
fn jitter_delay(attempt: u32) -> std::time::Duration {
    const BASE_MS: u64 = 5;
    const CAP_MS: u64 = 200;
    // Bound the shift; 5·2^6 already exceeds the cap.
    let ceil = (BASE_MS << attempt.min(6)).clamp(1, CAP_MS);
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).expect("OS CSPRNG unavailable");
    let ms = u64::from_le_bytes(buf) % ceil;
    std::time::Duration::from_millis(ms)
}

/// Run `op`, retrying busy errors with jittered backoff. `op` must rebuild its
/// query on each call and have no side effects outside `SQLite`.
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

/// Convert a result row to a Lua table keyed by column name.
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
                // TEXT and BLOB both come back as raw bytes.
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

/// Open the WAL-mode pool and ensure the internal `lur_kv` table.
///
/// Both layers are needed: `busy_timeout` (5 s) waits out ordinary write-lock
/// contention; `retry_busy` covers locks `SQLite` fails fast on (the WAL-mode
/// switch on a fresh connection, lock upgrades). Don't lower `busy_timeout`:
/// `retry_busy` sleeps < 75 ms in total, so 200 ms surfaced `database is
/// locked` under load.
pub(crate) async fn open_pool(path: &Path) -> sqlx::Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(5))
        .journal_mode(SqliteJournalMode::Wal);
    let pool = retry_busy(|| {
        let opts = opts.clone();
        async move { SqlitePoolOptions::new().connect_with(opts).await }
    })
    .await?;
    retry_busy(|| ensure_kv_schema(&pool)).await?;
    retry_busy(|| purge_expired(&pool, now_ms())).await?;
    Ok(pool)
}

/// Create `lur_kv`, adding `expires_at` to tables that predate TTLs.
async fn ensure_kv_schema(pool: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS lur_kv \
         (key TEXT PRIMARY KEY, value BLOB, expires_at INTEGER)",
    )
    .execute(pool)
    .await?;
    let has_expiry: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('lur_kv') WHERE name = 'expires_at'",
    )
    .fetch_one(pool)
    .await?;
    if has_expiry == 0 {
        // SQLite has no ADD COLUMN IF NOT EXISTS: tolerate losing the race to
        // another process opening the same file.
        match sqlx::query("ALTER TABLE lur_kv ADD COLUMN expires_at INTEGER")
            .execute(pool)
            .await
        {
            Err(e) if !e.to_string().contains("duplicate column") => return Err(e),
            _ => {}
        }
    }
    sqlx::query("CREATE INDEX IF NOT EXISTS lur_kv_expires_at ON lur_kv (expires_at)")
        .execute(pool)
        .await?;
    Ok(())
}

async fn purge_expired(pool: &SqlitePool, now: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM lur_kv WHERE expires_at IS NOT NULL AND expires_at <= ?")
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

/// Bind each Lua value as a positional parameter.
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
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n < i64::MAX as f64 {
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

/// Column 0 as bytes (`None` for NULL); INTEGER/REAL render as decimal text.
pub(crate) fn value_to_bytes(row: &sqlx::sqlite::SqliteRow) -> mlua::Result<Option<Vec<u8>>> {
    let raw = row
        .try_get_raw(0)
        .map_err(|e| Error::runtime(format!("lur.kv: {e}")))?;
    if raw.is_null() {
        return Ok(None);
    }
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

/// `SQLite` backend; cloning clones the pool handle.
#[derive(Clone)]
pub(crate) struct SqliteBackend {
    pool: SqlitePool,
    purge: PurgeClock,
}

impl SqliteBackend {
    pub(crate) async fn open(path: &Path) -> mlua::Result<Self> {
        let pool = open_pool(path)
            .await
            .map_err(|e| Error::runtime(format!("lur.db: opening {}: {e}", path.display())))?;
        Ok(Self {
            pool,
            purge: PurgeClock::started(now_ms()),
        })
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

    /// `BEGIN IMMEDIATE` on a pinned connection, retrying on busy.
    pub(crate) async fn begin(&self) -> mlua::Result<SqliteTransaction> {
        let tx = retry_busy(|| self.pool.begin_with("BEGIN IMMEDIATE"))
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        Ok(SqliteTransaction {
            tx: tokio::sync::Mutex::new(Some(tx)),
        })
    }

    pub(crate) async fn kv_get(&self, lua: &Lua, key: String) -> mlua::Result<Value> {
        let row = sqlx::query(
            "SELECT value FROM lur_kv WHERE key = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
        )
        .bind(key)
        .bind(now_ms())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
        match row {
            None => Ok(Value::Nil),
            Some(r) => match value_to_bytes(&r)? {
                None => Ok(Value::Nil),
                Some(bytes) => Ok(Value::String(lua.create_string(bytes)?)),
            },
        }
    }

    /// Sweeps expired rows at most once an hour; best-effort, so errors are dropped.
    async fn maybe_purge(&self) {
        let now = now_ms();
        if self.purge.due(now) {
            let _ = retry_busy(|| purge_expired(&self.pool, now)).await;
        }
    }

    pub(crate) async fn kv_set(
        &self,
        key: String,
        value: Vec<u8>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<()> {
        let expires_at = expiry_at(now_ms(), ttl_ms);
        sqlx::query("INSERT OR REPLACE INTO lur_kv (key, value, expires_at) VALUES (?, ?, ?)")
            .bind(key)
            .bind(value)
            .bind(expires_at)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.set: {e}")))?;
        self.maybe_purge().await;
        Ok(())
    }

    pub(crate) async fn kv_delete(&self, key: String) -> mlua::Result<()> {
        sqlx::query("DELETE FROM lur_kv WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.delete: {e}")))?;
        Ok(())
    }

    /// Insert unless a live row exists; an expired row is taken over.
    async fn insert_absent(
        &self,
        voice: &str,
        key: &str,
        value: &[u8],
        ttl_ms: Option<i64>,
    ) -> mlua::Result<bool> {
        let now = now_ms();
        let expires_at = expiry_at(now, ttl_ms);
        let res = retry_busy(|| async {
            sqlx::query(
                "INSERT INTO lur_kv (key, value, expires_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(key) DO UPDATE SET \
                   value = excluded.value, expires_at = excluded.expires_at \
                 WHERE lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= ?4",
            )
            .bind(key)
            .bind(value)
            .bind(expires_at)
            .bind(now)
            .execute(&self.pool)
            .await
        })
        .await
        .map_err(|e| Error::runtime(format!("{voice}: {e}")))?;
        Ok(res.rows_affected() == 1)
    }

    pub(crate) async fn kv_add(
        &self,
        key: String,
        value: Vec<u8>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<bool> {
        let added = self
            .insert_absent("lur.kv.add", &key, &value, ttl_ms)
            .await?;
        self.maybe_purge().await;
        Ok(added)
    }

    /// `ttl_ms` of `None` keeps the row's existing expiry.
    pub(crate) async fn kv_cas(
        &self,
        key: String,
        expected: Option<Vec<u8>>,
        new: Option<Vec<u8>>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<bool> {
        let now = now_ms();
        let applied = match (expected, new) {
            (None, Some(v)) => self.insert_absent("lur.kv.cas", &key, &v, ttl_ms).await?,
            (None, None) => {
                let r = sqlx::query(
                    "SELECT 1 FROM lur_kv WHERE key = ?1 \
                     AND (expires_at IS NULL OR expires_at > ?2)",
                )
                .bind(key)
                .bind(now)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?;
                r.is_none()
            }
            (Some(e), Some(v)) => {
                let expires_at = expiry_at(now, ttl_ms);
                retry_busy(|| async {
                    sqlx::query(
                        "UPDATE lur_kv SET value = ?1, expires_at = COALESCE(?2, expires_at) \
                         WHERE key = ?3 AND value = ?4 \
                         AND (expires_at IS NULL OR expires_at > ?5)",
                    )
                    .bind(v.clone())
                    .bind(expires_at)
                    .bind(key.clone())
                    .bind(e.clone())
                    .bind(now)
                    .execute(&self.pool)
                    .await
                })
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
            (Some(e), None) => {
                retry_busy(|| async {
                    sqlx::query(
                        "DELETE FROM lur_kv WHERE key = ?1 AND value = ?2 \
                         AND (expires_at IS NULL OR expires_at > ?3)",
                    )
                    .bind(key.clone())
                    .bind(e.clone())
                    .bind(now)
                    .execute(&self.pool)
                    .await
                })
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
        };
        Ok(applied)
    }

    /// Atomic upsert-add; a non-integer live value yields no row → error. An
    /// expired row restarts from `delta`. The expiry is set when the row has
    /// none (or `renew`), so a counter that predates TTLs heals itself.
    pub(crate) async fn kv_incr(
        &self,
        voice: &'static str,
        key: String,
        delta: i64,
        ttl: Option<Ttl>,
    ) -> mlua::Result<i64> {
        let now = now_ms();
        let expires_at = expiry_at(now, ttl.map(|t| t.ms));
        let renew = i64::from(ttl.is_some_and(|t| t.renew));
        let row = retry_busy(|| async {
            sqlx::query(
                "INSERT INTO lur_kv (key, value, expires_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(key) DO UPDATE SET \
                   value = CASE WHEN lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= ?4 \
                                THEN excluded.value ELSE lur_kv.value + excluded.value END, \
                   expires_at = CASE \
                     WHEN lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= ?4 \
                       THEN excluded.expires_at \
                     WHEN ?5 = 1 OR lur_kv.expires_at IS NULL \
                       THEN COALESCE(excluded.expires_at, lur_kv.expires_at) \
                     ELSE lur_kv.expires_at END \
                 WHERE (lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= ?4) \
                    OR typeof(lur_kv.value) = 'integer' \
                 RETURNING value",
            )
            .bind(key.clone())
            .bind(delta)
            .bind(expires_at)
            .bind(now)
            .bind(renew)
            .fetch_optional(&self.pool)
            .await
        })
        .await
        .map_err(|e| Error::runtime(format!("{voice}: {e}")))?;
        let n = match row {
            Some(r) => r
                .try_get::<i64, usize>(0)
                .map_err(|e| Error::runtime(format!("{voice}: {e}")))?,
            None => {
                return Err(Error::runtime(format!(
                    "{voice}: existing value is not an integer"
                )));
            }
        };
        self.maybe_purge().await;
        Ok(n)
    }

    pub(crate) async fn kv_expire(&self, key: String, ttl_ms: i64) -> mlua::Result<bool> {
        let now = now_ms();
        let res = retry_busy(|| async {
            sqlx::query(
                "UPDATE lur_kv SET expires_at = ?1 WHERE key = ?2 \
                 AND (expires_at IS NULL OR expires_at > ?3)",
            )
            .bind(now.saturating_add(ttl_ms))
            .bind(key.clone())
            .bind(now)
            .execute(&self.pool)
            .await
        })
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.expire: {e}")))?;
        Ok(res.rows_affected() == 1)
    }

    pub(crate) async fn kv_ttl(&self, key: String) -> mlua::Result<Option<Option<i64>>> {
        let now = now_ms();
        let row = sqlx::query(
            "SELECT expires_at FROM lur_kv WHERE key = ?1 \
             AND (expires_at IS NULL OR expires_at > ?2)",
        )
        .bind(key)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.ttl: {e}")))?;
        row.map(|r| {
            r.try_get::<Option<i64>, usize>(0)
                .map(|exp| exp.map(|e| e - now))
                .map_err(|e| Error::runtime(format!("lur.kv.ttl: {e}")))
        })
        .transpose()
    }

    /// `lur.kv.update` read-modify-write inside `BEGIN IMMEDIATE`. The result is
    /// bound as BLOB (not via `bind_one`, which would store TEXT) so it stays
    /// comparable under `lur.kv.cas`; `SQLite` never equates TEXT and BLOB.
    pub(crate) async fn kv_update(
        &self,
        lua: &Lua,
        key: String,
        func: Function,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<Value> {
        let mut tx = retry_busy(|| self.pool.begin_with("BEGIN IMMEDIATE"))
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.update: begin: {e}")))?;

        // An error return or a cancellation anywhere in here drops `tx`, which
        // queues a rollback before the connection is used again.
        async {
            // An expired row reads as absent; a live one keeps its expiry
            // unless `ttl_ms` replaces it.
            let (cur, cur_expiry): (Value, Option<i64>) = match sqlx::query(
                "SELECT value, expires_at FROM lur_kv WHERE key = ?1 \
                 AND (expires_at IS NULL OR expires_at > ?2)",
            )
            .bind(&key)
            .bind(now_ms())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?
            {
                None => (Value::Nil, None),
                Some(r) => {
                    let expiry = r
                        .try_get::<Option<i64>, usize>(1)
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                    let value = match value_to_bytes(&r)? {
                        None => Value::Nil,
                        Some(bytes) => Value::String(lua.create_string(bytes)?),
                    };
                    (value, expiry)
                }
            };

            let new = func.call_async::<Value>(cur).await?;

            match &new {
                Value::Nil => {
                    sqlx::query("DELETE FROM lur_kv WHERE key = ?")
                        .bind(&key)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                Value::String(s) => {
                    let expires_at = expiry_at(now_ms(), ttl_ms).or(cur_expiry);
                    sqlx::query(
                        "INSERT OR REPLACE INTO lur_kv (key, value, expires_at) VALUES (?, ?, ?)",
                    )
                    .bind(&key)
                    .bind(s.as_bytes().to_vec())
                    .bind(expires_at)
                    .execute(&mut *tx)
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
            tx.commit()
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: commit: {e}")))?;
            Ok(new)
        }
        .await
    }
}

/// Write transaction held across Lua calls; calls after commit/rollback error.
/// Dropping it (e.g. on cancellation) rolls back via sqlx.
pub(crate) struct SqliteTransaction {
    tx: tokio::sync::Mutex<Option<sqlx::Transaction<'static, Sqlite>>>,
}

impl SqliteTransaction {
    pub(crate) async fn exec(
        &self,
        _lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<super::ExecResult> {
        let mut guard = self.tx.lock().await;
        let tx = guard
            .as_mut()
            .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
        let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .execute(&mut **tx)
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
        let mut guard = self.tx.lock().await;
        let tx = guard
            .as_mut()
            .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
        let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx query: {e}")))?;
        let out = lua.create_table()?;
        for (i, row) in rows.iter().enumerate() {
            out.raw_set(i as i64 + 1, read_row(lua, row)?)?;
        }
        Ok(out)
    }

    pub(crate) async fn commit(&self) -> mlua::Result<()> {
        let mut guard = self.tx.lock().await;
        if let Some(tx) = guard.take() {
            // A failed commit drops `tx`, which rolls back.
            tx.commit()
                .await
                .map_err(|e| Error::runtime(format!("lur.db.tx: commit: {e}")))?;
        }
        Ok(())
    }

    pub(crate) async fn rollback(&self) {
        let mut guard = self.tx.lock().await;
        if let Some(tx) = guard.take() {
            let _ = tx.rollback().await;
        }
    }
}

// One-connection pool: the next acquire must reuse the cancelled tx's
// connection, exposing one returned mid-transaction. Shared with db.rs tests.
#[cfg(test)]
pub(super) async fn max1_backend(dir: &std::path::Path) -> SqliteBackend {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
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
    ensure_kv_schema(&pool).await.unwrap();
    SqliteBackend {
        pool,
        purge: PurgeClock::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    // busy_timeout=0 plus a held write lock yields a genuine SQLITE_BUSY.
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

            // Reuse `b`: the pool is exhausted, so `&pool` would block on acquire.
            let syntax = sqlx::query("NOT VALID SQL")
                .execute(&mut *b)
                .await
                .unwrap_err();
            assert!(!is_busy(&syntax), "syntax error wrongly classified busy");
        });
    }

    // The lur_kv DDL needs the write lock; open must wait out a holder.
    #[test]
    fn open_pool_waits_out_a_held_write_lock() {
        const HOLD_MS: u64 = 500;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("held.db");

            // Raw pool without lur_kv, so open_pool must run the DDL.
            let opts = SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .busy_timeout(std::time::Duration::from_millis(200))
                .journal_mode(SqliteJournalMode::Wal);
            let holder = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(opts)
                .await
                .unwrap();
            let mut conn = holder.acquire().await.unwrap();
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *conn)
                .await
                .unwrap();

            let releaser = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(HOLD_MS)).await;
                sqlx::query("COMMIT").execute(&mut *conn).await.unwrap();
            });

            let pool = open_pool(&path)
                .await
                .expect("open waits out a held write lock instead of erroring busy");
            releaser.await.unwrap();
            sqlx::query("SELECT key FROM lur_kv")
                .fetch_optional(&pool)
                .await
                .expect("lur_kv exists after the retried open");
        });
    }

    // Dropping an unfinished tx rolls back and frees the sole connection.
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

            // Only succeeds once the detached rollback releases the connection.
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
            assert_eq!(
                rows.raw_len(),
                0,
                "row from the cancelled tx must be rolled back"
            );
            tx2.rollback().await;
        });
    }

    // Cancelling kv.update mid-transform rolls back and frees the connection.
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

            // Transform signals entry, then parks forever.
            let entered = std::sync::Arc::new(tokio::sync::Notify::new());
            let entered2 = entered.clone();
            let parking = lua
                .create_async_function(move |_, _cur: Value| {
                    let entered2 = entered2.clone();
                    async move {
                        entered2.notify_one();
                        std::future::pending::<mlua::Result<Value>>().await
                    }
                })
                .unwrap();

            let mut fut = Box::pin(backend.kv_update(&lua, "k".to_string(), parking, None));
            tokio::select! {
                _ = &mut fut => panic!("kv_update should park in the transform"),
                () = entered.notified() => {}
            }
            drop(fut); // cancel mid-transform → dropping the sqlx Transaction rolls back

            // Blocks until the detached rollback frees the sole connection.
            let got = backend.kv_get(&lua, "k".to_string()).await.unwrap();
            assert_eq!(got, Value::Nil, "cancelled update must not commit");
            let tx = backend
                .begin()
                .await
                .expect("connection must be reusable after a cancelled update");
            tx.rollback().await;
        });
    }
}
