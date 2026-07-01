//! `SQLite` storage backend: owns the `sqlx` `SQLite` pool and all
//! `SQLite`-specific SQL, `?` binding, row→Lua type mapping, WAL/busy
//! handling, and retry.

use std::future::Future;
use std::path::Path;

use mlua::{Error, Lua, Table, Value};
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteRow,
};
use sqlx::{Column, Row, TypeInfo, ValueRef};

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
