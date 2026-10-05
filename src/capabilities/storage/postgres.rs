//! `PostgreSQL` storage backend: pool, SQL, `$n` binding, row→Lua mapping (core
//! types only), and the `kind`-discriminated kv schema. No retry layer: single
//! statements block rather than fail busy, and `SERIALIZABLE` APIs are
//! documented as fallible.

use std::str::FromStr;

use mlua::{Error, Function, Lua, Table, Value};
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Column, Postgres, Row, TypeInfo, ValueRef};

use super::{PurgeClock, Ttl, expiry_at, now_ms};
use crate::capabilities::null;

/// SQLSTATE `40001`. Matched by code: the message is localized, the code isn't.
fn is_serialization_failure(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "40001")
}

/// `"{ctx}: {e}"`, except a serialization failure gets a stable message that a
/// `pcall` retry loop can match.
fn map_pg_error(ctx: &str, e: &sqlx::Error) -> Error {
    if is_serialization_failure(e) {
        Error::runtime(format!(
            "{ctx}: serialization failure (SQLSTATE 40001): concurrent transaction conflict, retry the transaction"
        ))
    } else {
        Error::runtime(format!("{ctx}: {e}"))
    }
}

/// A dynamically-bound Postgres query.
pub(crate) type PgQuery<'q> = sqlx::query::Query<'q, Postgres, PgArguments>;

/// Bind each Lua value as a positional (`$n`) parameter.
pub(crate) fn bind_all<'q>(mut q: PgQuery<'q>, params: &[Value]) -> mlua::Result<PgQuery<'q>> {
    for v in params {
        q = bind_one(q, v)?;
    }
    Ok(q)
}

fn bind_one<'q>(q: PgQuery<'q>, v: &Value) -> mlua::Result<PgQuery<'q>> {
    Ok(match v {
        // Bound as a text NULL; non-text columns may need an explicit `$1::int`.
        Value::Nil => q.bind(None::<String>),
        Value::UserData(_) if null::is_null(v) => q.bind(None::<String>),
        Value::Boolean(b) => q.bind(*b),
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

/// Convert a row to a Lua table keyed by column name. Non-core types error with
/// a cast-to-text hint: `sqlx` reads binary wire format, so `lur` can't render
/// arbitrary types.
pub(crate) fn read_row(lua: &Lua, row: &PgRow) -> mlua::Result<Table> {
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
                "INT2" => Value::Integer(i64::from(get::<i16>(row, i)?)),
                "INT4" => Value::Integer(i64::from(get::<i32>(row, i)?)),
                "INT8" => Value::Integer(get::<i64>(row, i)?),
                "FLOAT4" => Value::Number(f64::from(get::<f32>(row, i)?)),
                "FLOAT8" => Value::Number(get::<f64>(row, i)?),
                "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => {
                    Value::String(lua.create_string(get::<String>(row, i)?)?)
                }
                "BYTEA" => Value::String(lua.create_string(get::<Vec<u8>>(row, i)?)?),
                other => {
                    let name = col.name();
                    return Err(Error::runtime(format!(
                        "lur.db: unsupported column type '{other}' in column '{name}'; \
                         CAST it to text (e.g. {name}::text)"
                    )));
                }
            }
        };
        t.set(col.name(), value)?;
    }
    Ok(t)
}

fn get<'r, T>(row: &'r PgRow, i: usize) -> mlua::Result<T>
where
    T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get::<T, usize>(i)
        .map_err(|e| Error::runtime(format!("lur.db: decoding column {i}: {e}")))
}

/// `SELECT kind, bytes, num` row → bytes; a `kind=1` counter renders as decimal.
fn kv_row_to_bytes(row: &PgRow) -> mlua::Result<Vec<u8>> {
    let kind: i16 = row
        .try_get::<i16, usize>(0)
        .map_err(|e| Error::runtime(format!("lur.kv: decoding kind: {e}")))?;
    if kind == 1 {
        let n: i64 = row
            .try_get::<i64, usize>(2)
            .map_err(|e| Error::runtime(format!("lur.kv: decoding counter: {e}")))?;
        Ok(n.to_string().into_bytes())
    } else {
        let b: Option<Vec<u8>> = row
            .try_get::<Option<Vec<u8>>, usize>(1)
            .map_err(|e| Error::runtime(format!("lur.kv: decoding value: {e}")))?;
        Ok(b.unwrap_or_default())
    }
}

async fn purge_expired(pool: &PgPool, now: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM lur_kv WHERE expires_at IS NOT NULL AND expires_at <= $1")
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

/// Postgres backend; cloning clones the pool handle.
#[derive(Clone)]
pub(crate) struct PgBackend {
    pool: PgPool,
    purge: PurgeClock,
}

impl PgBackend {
    /// Connect to an existing database and ensure the internal `lur_kv` table.
    pub(crate) async fn open(url: &str) -> mlua::Result<Self> {
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| Error::runtime(format!("lur.db: invalid postgres url: {e}")))?;
        let pool = PgPoolOptions::new()
            .connect_with(opts)
            .await
            .map_err(|e| Error::runtime(format!("lur.db: connecting to postgres: {e}")))?;
        let ensure = |e: sqlx::Error| Error::runtime(format!("lur.db: ensuring lur_kv: {e}"));
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS lur_kv (\
             key TEXT PRIMARY KEY, kind SMALLINT NOT NULL, bytes BYTEA, num BIGINT, \
             expires_at BIGINT)",
        )
        .execute(&pool)
        .await
        .map_err(ensure)?;
        // Tables created before TTLs lack the column.
        sqlx::query("ALTER TABLE lur_kv ADD COLUMN IF NOT EXISTS expires_at BIGINT")
            .execute(&pool)
            .await
            .map_err(ensure)?;
        sqlx::query("CREATE INDEX IF NOT EXISTS lur_kv_expires_at ON lur_kv (expires_at)")
            .execute(&pool)
            .await
            .map_err(ensure)?;
        purge_expired(&pool, now_ms()).await.map_err(ensure)?;
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
        let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .execute(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.exec: {e}")))?;
        // Postgres has no last_insert_rowid(); generated keys come via RETURNING.
        Ok(super::ExecResult {
            rows_affected: res.rows_affected(),
            last_insert_id: 0,
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

    pub(crate) async fn kv_get(&self, lua: &Lua, key: String) -> mlua::Result<Value> {
        let row = sqlx::query(
            "SELECT kind, bytes, num FROM lur_kv WHERE key = $1 \
             AND (expires_at IS NULL OR expires_at > $2)",
        )
        .bind(key)
        .bind(now_ms())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
        match row {
            None => Ok(Value::Nil),
            Some(r) => Ok(Value::String(lua.create_string(kv_row_to_bytes(&r)?)?)),
        }
    }

    /// Sweeps expired rows at most once an hour; best-effort, so errors are dropped.
    async fn maybe_purge(&self) {
        let now = now_ms();
        if self.purge.due(now) {
            let _ = purge_expired(&self.pool, now).await;
        }
    }

    pub(crate) async fn kv_set(
        &self,
        key: String,
        value: Vec<u8>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<()> {
        let expires_at = expiry_at(now_ms(), ttl_ms);
        sqlx::query(
            "INSERT INTO lur_kv (key, kind, bytes, num, expires_at) VALUES ($1, 0, $2, NULL, $3) \
             ON CONFLICT (key) DO UPDATE SET kind = 0, bytes = excluded.bytes, num = NULL, \
               expires_at = excluded.expires_at",
        )
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
        sqlx::query("DELETE FROM lur_kv WHERE key = $1")
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
        key: String,
        value: Vec<u8>,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<bool> {
        let now = now_ms();
        let res = sqlx::query(
            "INSERT INTO lur_kv (key, kind, bytes, expires_at) VALUES ($1, 0, $2, $3) \
             ON CONFLICT (key) DO UPDATE SET kind = 0, bytes = excluded.bytes, num = NULL, \
               expires_at = excluded.expires_at \
             WHERE lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= $4",
        )
        .bind(key)
        .bind(value)
        .bind(expiry_at(now, ttl_ms))
        .bind(now)
        .execute(&self.pool)
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
        let added = self.insert_absent("lur.kv.add", key, value, ttl_ms).await?;
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
            (None, Some(v)) => self.insert_absent("lur.kv.cas", key, v, ttl_ms).await?,
            (None, None) => {
                let r = sqlx::query(
                    "SELECT 1 FROM lur_kv WHERE key = $1 \
                     AND (expires_at IS NULL OR expires_at > $2)",
                )
                .bind(key)
                .bind(now)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?;
                r.is_none()
            }
            (Some(e), Some(v)) => {
                sqlx::query(
                    "UPDATE lur_kv SET kind = 0, bytes = $1, num = NULL, \
                       expires_at = COALESCE($2, expires_at) \
                     WHERE key = $3 AND kind = 0 AND bytes = $4 \
                     AND (expires_at IS NULL OR expires_at > $5)",
                )
                .bind(v)
                .bind(expiry_at(now, ttl_ms))
                .bind(key)
                .bind(e)
                .bind(now)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
            (Some(e), None) => {
                sqlx::query(
                    "DELETE FROM lur_kv WHERE key = $1 AND kind = 0 AND bytes = $2 \
                     AND (expires_at IS NULL OR expires_at > $3)",
                )
                .bind(key)
                .bind(e)
                .bind(now)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
        };
        Ok(applied)
    }

    /// Atomic upsert-add; a non-counter live value yields no row → error. An
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
        let row = sqlx::query(
            "INSERT INTO lur_kv (key, kind, num, expires_at) VALUES ($1, 1, $2, $3) \
             ON CONFLICT (key) DO UPDATE SET \
               kind = 1, bytes = NULL, \
               num = CASE WHEN lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= $4 \
                          THEN excluded.num ELSE lur_kv.num + excluded.num END, \
               expires_at = CASE \
                 WHEN lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= $4 \
                   THEN excluded.expires_at \
                 WHEN $5::boolean OR lur_kv.expires_at IS NULL \
                   THEN COALESCE(excluded.expires_at, lur_kv.expires_at) \
                 ELSE lur_kv.expires_at END \
             WHERE (lur_kv.expires_at IS NOT NULL AND lur_kv.expires_at <= $4) \
                OR lur_kv.kind = 1 \
             RETURNING num",
        )
        .bind(key)
        .bind(delta)
        .bind(expiry_at(now, ttl.map(|t| t.ms)))
        .bind(now)
        .bind(ttl.is_some_and(|t| t.renew))
        .fetch_optional(&self.pool)
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
        let res = sqlx::query(
            "UPDATE lur_kv SET expires_at = $1 WHERE key = $2 \
             AND (expires_at IS NULL OR expires_at > $3)",
        )
        .bind(now.saturating_add(ttl_ms))
        .bind(key)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.expire: {e}")))?;
        Ok(res.rows_affected() == 1)
    }

    pub(crate) async fn kv_ttl(&self, key: String) -> mlua::Result<Option<Option<i64>>> {
        let now = now_ms();
        let row = sqlx::query(
            "SELECT expires_at FROM lur_kv WHERE key = $1 \
             AND (expires_at IS NULL OR expires_at > $2)",
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

    /// `SERIALIZABLE` transaction on a pinned connection. Conflicts surface as
    /// 40001 (at a statement or at COMMIT) and are not retried: the body may have
    /// external side effects.
    pub(crate) async fn begin(&self) -> mlua::Result<PgTransaction> {
        let tx = self
            .pool
            .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        Ok(PgTransaction {
            tx: tokio::sync::Mutex::new(Some(tx)),
        })
    }

    /// `lur.kv.update` read-modify-write inside `SERIALIZABLE`; conflicts surface
    /// as 40001, not retried. Writes `kind=0` bytes so `kv_cas` can match it.
    pub(crate) async fn kv_update(
        &self,
        lua: &Lua,
        key: String,
        func: Function,
        ttl_ms: Option<i64>,
    ) -> mlua::Result<Value> {
        let mut tx = self
            .pool
            .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.update: begin: {e}")))?;

        // Cancellation or an error anywhere in here drops `tx`, which rolls back.
        async {
            // An expired row reads as absent; a live one keeps its expiry
            // unless `ttl_ms` replaces it.
            let (cur, cur_expiry): (Value, Option<i64>) = match sqlx::query(
                "SELECT kind, bytes, num, expires_at FROM lur_kv WHERE key = $1 \
                 AND (expires_at IS NULL OR expires_at > $2)",
            )
            .bind(&key)
            .bind(now_ms())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| map_pg_error("lur.kv.update", &e))?
            {
                None => (Value::Nil, None),
                Some(r) => {
                    let expiry = r
                        .try_get::<Option<i64>, usize>(3)
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                    (
                        Value::String(lua.create_string(kv_row_to_bytes(&r)?)?),
                        expiry,
                    )
                }
            };

            let new = func.call_async::<Value>(cur).await?;

            match &new {
                Value::Nil => {
                    sqlx::query("DELETE FROM lur_kv WHERE key = $1")
                        .bind(&key)
                        .execute(&mut *tx)
                        .await
                        .map_err(|e| map_pg_error("lur.kv.update", &e))?;
                }
                Value::String(s) => {
                    let expires_at = expiry_at(now_ms(), ttl_ms).or(cur_expiry);
                    sqlx::query(
                        "INSERT INTO lur_kv (key, kind, bytes, num, expires_at) \
                         VALUES ($1, 0, $2, NULL, $3) \
                         ON CONFLICT (key) DO UPDATE SET kind = 0, bytes = excluded.bytes, \
                           num = NULL, expires_at = excluded.expires_at",
                    )
                    .bind(&key)
                    .bind(s.as_bytes().to_vec())
                    .bind(expires_at)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| map_pg_error("lur.kv.update", &e))?;
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
                .map_err(|e| map_pg_error("lur.kv.update: commit", &e))?;
            Ok(new)
        }
        .await
    }
}

/// Write transaction; calls after commit/rollback error. Dropping it (e.g. on
/// cancellation) rolls back via sqlx.
pub(crate) struct PgTransaction {
    tx: tokio::sync::Mutex<Option<sqlx::Transaction<'static, Postgres>>>,
}

impl PgTransaction {
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
            .map_err(|e| map_pg_error("lur.db.tx exec", &e))?;
        Ok(super::ExecResult {
            rows_affected: res.rows_affected(),
            last_insert_id: 0,
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
            .map_err(|e| map_pg_error("lur.db.tx query", &e))?;
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
                .map_err(|e| map_pg_error("lur.db.tx: commit", &e))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    // Unique within this process only (nextest runs one test per process).
    fn unique_table() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        format!("lur_cancel_{}", N.fetch_add(1, Ordering::Relaxed))
    }

    // One-connection pool; None (skip) when Postgres is unreachable, except in CI.
    async fn pg_max1() -> Option<PgBackend> {
        use std::time::Duration;
        let url = std::env::var("LUR_TEST_PG_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".to_string());
        let Ok(opts) = PgConnectOptions::from_str(&url) else {
            return None;
        };
        let Ok(pool) = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect_with(opts)
            .await
        else {
            assert!(
                std::env::var("CI").is_err(),
                "CI: Postgres unreachable but CI must provision it"
            );
            eprintln!("skipping PG test: Postgres unreachable (start it: docker compose up -d)");
            return None;
        };
        Some(PgBackend {
            pool,
            purge: PurgeClock::default(),
        })
    }

    // Dropping an unfinished tx rolls back before the connection is reused.
    #[test]
    fn pg_dropped_tx_rolls_back() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let Some(backend) = pg_max1().await else {
                return;
            };
            let lua = Lua::new();
            let t = unique_table();

            // A failed earlier run may have left the table.
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

            // Unfixed, the reused connection is still in the tx and sees the row.
            let rows = backend
                .query(&lua, format!("SELECT x FROM {t}"), vec![])
                .await
                .unwrap();
            assert_eq!(
                rows.raw_len(),
                0,
                "row from the cancelled tx must be rolled back"
            );

            backend
                .exec(&lua, format!("DROP TABLE {t}"), vec![])
                .await
                .unwrap();
        });
    }
}
