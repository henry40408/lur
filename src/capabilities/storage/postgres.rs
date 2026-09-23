//! `PostgreSQL` storage backend: pool, SQL, `$n` binding, row→Lua mapping (core
//! types only), and the `kind`-discriminated kv schema. No retry layer: single
//! statements block rather than fail busy, and `SERIALIZABLE` APIs are
//! documented as fallible.

use std::str::FromStr;

use mlua::{Error, Function, Lua, Table, Value};
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Column, Postgres, Row, TypeInfo, ValueRef};

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

/// Postgres backend; cloning clones the pool handle.
#[derive(Clone)]
pub(crate) struct PgBackend {
    pool: PgPool,
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
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS lur_kv (\
             key TEXT PRIMARY KEY, kind SMALLINT NOT NULL, bytes BYTEA, num BIGINT)",
        )
        .execute(&pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.db: ensuring lur_kv: {e}")))?;
        Ok(Self { pool })
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
        let row = sqlx::query("SELECT kind, bytes, num FROM lur_kv WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
        match row {
            None => Ok(Value::Nil),
            Some(r) => Ok(Value::String(lua.create_string(kv_row_to_bytes(&r)?)?)),
        }
    }

    pub(crate) async fn kv_set(&self, key: String, value: Vec<u8>) -> mlua::Result<()> {
        sqlx::query(
            "INSERT INTO lur_kv (key, kind, bytes, num) VALUES ($1, 0, $2, NULL) \
             ON CONFLICT (key) DO UPDATE SET kind = 0, bytes = excluded.bytes, num = NULL",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.set: {e}")))?;
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

    pub(crate) async fn kv_add(&self, key: String, value: Vec<u8>) -> mlua::Result<bool> {
        let res = sqlx::query(
            "INSERT INTO lur_kv (key, kind, bytes) VALUES ($1, 0, $2) \
             ON CONFLICT (key) DO NOTHING",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.add: {e}")))?;
        Ok(res.rows_affected() == 1)
    }

    pub(crate) async fn kv_cas(
        &self,
        key: String,
        expected: Option<Vec<u8>>,
        new: Option<Vec<u8>>,
    ) -> mlua::Result<bool> {
        let applied = match (expected, new) {
            (None, Some(v)) => {
                sqlx::query(
                    "INSERT INTO lur_kv (key, kind, bytes) VALUES ($1, 0, $2) \
                     ON CONFLICT (key) DO NOTHING",
                )
                .bind(key)
                .bind(v)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
            (None, None) => {
                let r = sqlx::query("SELECT 1 FROM lur_kv WHERE key = $1")
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?;
                r.is_none()
            }
            (Some(e), Some(v)) => {
                sqlx::query(
                    "UPDATE lur_kv SET kind = 0, bytes = $1, num = NULL \
                     WHERE key = $2 AND kind = 0 AND bytes = $3",
                )
                .bind(v)
                .bind(key)
                .bind(e)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
            (Some(e), None) => {
                sqlx::query("DELETE FROM lur_kv WHERE key = $1 AND kind = 0 AND bytes = $2")
                    .bind(key)
                    .bind(e)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                    .rows_affected()
                    == 1
            }
        };
        Ok(applied)
    }

    /// Atomic upsert-add; a non-counter existing value yields no row → error.
    pub(crate) async fn kv_incr(
        &self,
        voice: &'static str,
        key: String,
        delta: i64,
    ) -> mlua::Result<i64> {
        let row = sqlx::query(
            "INSERT INTO lur_kv (key, kind, num) VALUES ($1, 1, $2) \
             ON CONFLICT (key) DO UPDATE SET num = lur_kv.num + excluded.num \
             WHERE lur_kv.kind = 1 \
             RETURNING num",
        )
        .bind(key)
        .bind(delta)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("{voice}: {e}")))?;
        match row {
            Some(r) => r
                .try_get::<i64, usize>(0)
                .map_err(|e| Error::runtime(format!("{voice}: {e}"))),
            None => Err(Error::runtime(format!(
                "{voice}: existing value is not an integer"
            ))),
        }
    }

    /// `SERIALIZABLE` transaction on a pinned connection. Conflicts surface as
    /// 40001 (at a statement or at COMMIT) and are not retried: the body may have
    /// external side effects.
    pub(crate) async fn begin(&self) -> mlua::Result<PgTransaction> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        sqlx::query("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut *conn)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        Ok(PgTransaction {
            conn: tokio::sync::Mutex::new(Some(conn)),
        })
    }

    /// `lur.kv.update` read-modify-write inside `SERIALIZABLE`; conflicts surface
    /// as 40001, not retried. Writes `kind=0` bytes so `kv_cas` can match it.
    pub(crate) async fn kv_update(
        &self,
        lua: &Lua,
        key: String,
        func: Function,
    ) -> mlua::Result<Value> {
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.update: begin: {e}")))?;
        let mut tx = PinnedTx::new(conn);
        sqlx::query("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut **tx.conn())
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.update: begin: {e}")))?;

        // Cancellation anywhere in here drops `tx`, which rolls back.
        let result: mlua::Result<Value> = async {
            let cur: Value = match sqlx::query("SELECT kind, bytes, num FROM lur_kv WHERE key = $1")
                .bind(&key)
                .fetch_optional(&mut **tx.conn())
                .await
                .map_err(|e| map_pg_error("lur.kv.update", &e))?
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
                        .map_err(|e| map_pg_error("lur.kv.update", &e))?;
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
                    .map_err(|e| map_pg_error("lur.kv.update", &e))?;
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
                .map_err(|e| map_pg_error("lur.kv.update: commit", &e))?;
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
    }
}

/// Detached best-effort ROLLBACK for a transaction left open by cancellation,
/// so the connection doesn't sit idle-in-transaction holding locks. Without a
/// runtime, closes the connection instead.
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

/// Pinned connection with an open transaction; rolls back on drop unless
/// `disarm`ed after an explicit COMMIT/ROLLBACK.
struct PinnedTx {
    conn: Option<PoolConnection<Postgres>>,
}

impl PinnedTx {
    fn new(conn: PoolConnection<Postgres>) -> Self {
        Self { conn: Some(conn) }
    }

    fn conn(&mut self) -> &mut PoolConnection<Postgres> {
        self.conn.as_mut().expect("connection present until disarm")
    }

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

/// Pinned-connection write transaction; calls after commit/rollback error.
pub(crate) struct PgTransaction {
    conn: tokio::sync::Mutex<Option<PoolConnection<Postgres>>>,
}

impl PgTransaction {
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
        let mut guard = self.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
        let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .fetch_all(&mut **conn)
            .await
            .map_err(|e| map_pg_error("lur.db.tx query", &e))?;
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
            return Err(map_pg_error("lur.db.tx: commit", &e));
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

impl Drop for PgTransaction {
    /// Roll back a transaction abandoned by cancellation.
    fn drop(&mut self) {
        if let Some(conn) = self.conn.get_mut().take() {
            spawn_rollback(conn);
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
        Some(PgBackend { pool })
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
