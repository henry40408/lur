//! `lur.db` — long-term `SQLite` storage over sqlx (spec §6).
//!
//! The pool is opened lazily on first use (WAL mode, file auto-created) so a
//! script that never touches the DB pays nothing. Parameters use positional `?`
//! placeholders bound from varargs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use mlua::{Error, Function, Lua, MultiValue, Table, Value, Variadic};
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteRow,
};
use sqlx::{Column, Row, TypeInfo, ValueRef};

use super::null;
use crate::runtime::RunError;

/// A dynamically-bound `SQLite` query.
type Query<'q> = sqlx::query::Query<'q, sqlx::Sqlite, SqliteArguments>;

/// Install `lur.db`. `db_path` of `None` makes every call raise a clear error.
pub fn install(lua: &Lua, lur: &Table, db_path: Option<PathBuf>) -> Result<(), RunError> {
    let cell: Arc<OnceLock<SqlitePool>> = Arc::new(OnceLock::new());
    let path = Arc::new(db_path);
    let db = lua.create_table().map_err(RunError::Init)?;

    {
        let cell = Arc::clone(&cell);
        let path = Arc::clone(&path);
        let exec = lua
            .create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let cell = Arc::clone(&cell);
                let path = Arc::clone(&path);
                async move {
                    let pool = ensure_pool(&cell, &path).await?;
                    let q = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?;
                    let res = q
                        .execute(&pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.db.exec: {e}")))?;
                    let t = lua.create_table()?;
                    t.set("rows_affected", res.rows_affected())?;
                    t.set("last_insert_id", res.last_insert_rowid())?;
                    Ok(t)
                }
            })
            .map_err(RunError::Init)?;
        db.set("exec", exec).map_err(RunError::Init)?;
    }

    {
        let cell = Arc::clone(&cell);
        let path = Arc::clone(&path);
        let query = lua
            .create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let cell = Arc::clone(&cell);
                let path = Arc::clone(&path);
                async move {
                    let pool = ensure_pool(&cell, &path).await?;
                    let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.db.query: {e}")))?;
                    let out = lua.create_table()?;
                    for (i, row) in rows.iter().enumerate() {
                        out.raw_set(i as i64 + 1, read_row(&lua, row)?)?;
                    }
                    Ok(out)
                }
            })
            .map_err(RunError::Init)?;
        db.set("query", query).map_err(RunError::Init)?;
    }

    {
        let cell = Arc::clone(&cell);
        let path = Arc::clone(&path);
        let tx = lua
            .create_async_function(move |lua, func: Function| {
                let cell = Arc::clone(&cell);
                let path = Arc::clone(&path);
                async move { run_tx(lua, &cell, &path, func).await }
            })
            .map_err(RunError::Init)?;
        db.set("tx", tx).map_err(RunError::Init)?;
    }

    lur.set("db", db).map_err(RunError::Init)?;

    install_kv(lua, lur, &cell, &path)?;
    Ok(())
}

/// Pinned-connection transaction: build a `tx` handle whose exec/query run on
/// one connection, call `func(tx)`, then commit on a normal return or roll back
/// and re-raise on error (spec §6).
async fn run_tx(
    lua: Lua,
    cell: &OnceLock<SqlitePool>,
    path: &Option<PathBuf>,
    func: Function,
) -> mlua::Result<()> {
    let pool = ensure_pool(cell, path).await?;
    let txn = pool
        .begin()
        .await
        .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
    let shared = Arc::new(tokio::sync::Mutex::new(Some(txn)));

    let handle = lua.create_table()?;
    {
        let shared = Arc::clone(&shared);
        let exec =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = Arc::clone(&shared);
                async move {
                    let mut guard = shared.lock().await;
                    let txn = guard
                        .as_mut()
                        .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
                    let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?
                        .execute(&mut **txn)
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
                    let txn = guard
                        .as_mut()
                        .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
                    let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?
                        .fetch_all(&mut **txn)
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
    let txn = shared.lock().await.take();
    match result {
        Ok(_) => {
            if let Some(txn) = txn {
                txn.commit()
                    .await
                    .map_err(|e| Error::runtime(format!("lur.db.tx: commit: {e}")))?;
            }
            Ok(())
        }
        Err(e) => {
            if let Some(txn) = txn {
                let _ = txn.rollback().await;
            }
            Err(e)
        }
    }
}

/// Install `lur.kv` over the internal `lur_kv(key TEXT, value BLOB)` table,
/// sharing the same lazily-opened pool. Keys are strings, values raw bytes.
fn install_kv(
    lua: &Lua,
    lur: &Table,
    cell: &Arc<OnceLock<SqlitePool>>,
    path: &Arc<Option<PathBuf>>,
) -> Result<(), RunError> {
    let kv = lua.create_table().map_err(RunError::Init)?;

    {
        let cell = Arc::clone(cell);
        let path = Arc::clone(path);
        let get = lua
            .create_async_function(move |lua, key: String| {
                let cell = Arc::clone(&cell);
                let path = Arc::clone(&path);
                async move {
                    let pool = ensure_pool(&cell, &path).await?;
                    let row = sqlx::query("SELECT value FROM lur_kv WHERE key = ?")
                        .bind(key)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
                    match row {
                        Some(r) => Ok(Value::String(lua.create_string(get::<Vec<u8>>(&r, 0)?)?)),
                        None => Ok(Value::Nil),
                    }
                }
            })
            .map_err(RunError::Init)?;
        kv.set("get", get).map_err(RunError::Init)?;
    }
    {
        let cell = Arc::clone(cell);
        let path = Arc::clone(path);
        let set = lua
            .create_async_function(move |_, (key, value): (String, mlua::String)| {
                let cell = Arc::clone(&cell);
                let path = Arc::clone(&path);
                async move {
                    let pool = ensure_pool(&cell, &path).await?;
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
        let cell = Arc::clone(cell);
        let path = Arc::clone(path);
        let delete = lua
            .create_async_function(move |_, key: String| {
                let cell = Arc::clone(&cell);
                let path = Arc::clone(&path);
                async move {
                    let pool = ensure_pool(&cell, &path).await?;
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

/// Convert a result row to a Lua table keyed by column name (spec §6 read map).
fn read_row(lua: &Lua, row: &SqliteRow) -> mlua::Result<Table> {
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

/// Get the pool, opening it on first use. Errors clearly when no `--db` is set.
async fn ensure_pool(
    cell: &OnceLock<SqlitePool>,
    path: &Option<PathBuf>,
) -> mlua::Result<SqlitePool> {
    if let Some(p) = cell.get() {
        return Ok(p.clone());
    }
    let path = path
        .as_ref()
        .ok_or_else(|| Error::runtime("lur.db: no database configured; pass --db <path>"))?;
    let pool = open_pool(path)
        .await
        .map_err(|e| Error::runtime(format!("lur.db: opening {}: {e}", path.display())))?;
    let _ = cell.set(pool);
    Ok(cell.get().expect("pool just set").clone())
}

/// Open the `SQLite` pool in WAL mode and ensure the internal `lur_kv` table.
async fn open_pool(path: &Path) -> sqlx::Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
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
