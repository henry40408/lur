//! `lur.db` — long-term `SQLite` storage over sqlx (spec §6).
//!
//! The pool is opened lazily on first use (WAL mode, file auto-created) so a
//! script that never touches the DB pays nothing. Parameters use positional `?`
//! placeholders bound from varargs.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use mlua::{Error, Function, Lua, MultiValue, Table, Value, Variadic};
use sqlx::sqlite::SqlitePool;

use crate::capabilities::storage::sqlite::{bind_all, open_pool, read_row, retry_busy};
use crate::runtime::RunError;

/// Shared, lazily-opened `SQLite` pool plus its configured path, handed from
/// `db::install` to `kv::install` so both capabilities use one pool.
pub struct SqliteShared {
    pub(crate) cell: Arc<OnceLock<SqlitePool>>,
    pub(crate) path: Arc<Option<PathBuf>>,
}

/// Install `lur.db`. `db_path` of `None` makes every call raise a clear error.
/// Returns the shared pool handle so `kv::install` can reuse the same pool.
pub fn install(lua: &Lua, lur: &Table, db_path: Option<PathBuf>) -> Result<SqliteShared, RunError> {
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
                    // Surface non-retryable bind errors (bad Lua value types) once,
                    // before the retry loop — a logic error must never be retried.
                    // After this succeeds the in-loop `bind_all` cannot fail, so its
                    // Result is unwrapped.
                    let _ = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?;
                    let res = retry_busy(|| async {
                        bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)
                            .expect("params validated before retry loop")
                            .execute(&pool)
                            .await
                    })
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
    Ok(SqliteShared {
        cell: Arc::clone(&cell),
        path: Arc::clone(&path),
    })
}

/// Acquire a pooled connection and open a write transaction with `BEGIN
/// IMMEDIATE`, so the write lock is taken up front (no read→write upgrade
/// busy/deadlock) and the caller's body runs exactly once. The caller MUST
/// finish with `COMMIT` or `ROLLBACK`.
pub(crate) async fn begin_immediate(
    pool: &SqlitePool,
) -> mlua::Result<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
    let conn = retry_busy(|| async {
        let mut conn = pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        Ok(conn)
    })
    .await
    .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
    Ok(conn)
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
    let conn = begin_immediate(&pool).await?;
    let shared = Arc::new(tokio::sync::Mutex::new(Some(conn)));

    let handle = lua.create_table()?;
    {
        let shared = Arc::clone(&shared);
        let exec =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = Arc::clone(&shared);
                async move {
                    let mut guard = shared.lock().await;
                    let conn = guard
                        .as_mut()
                        .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
                    let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?
                        .execute(&mut **conn)
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
                    let conn = guard
                        .as_mut()
                        .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
                    let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql)), &params)?
                        .fetch_all(&mut **conn)
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
    let conn = shared.lock().await.take();
    match result {
        Ok(_) => {
            if let Some(mut conn) = conn
                && let Err(e) = sqlx::query("COMMIT").execute(&mut *conn).await
            {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Err(Error::runtime(format!("lur.db.tx: commit: {e}")));
            }
            Ok(())
        }
        Err(e) => {
            if let Some(mut conn) = conn {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            }
            Err(e)
        }
    }
}

/// Get the pool, opening it on first use. Errors clearly when no `--db` is set.
pub(crate) async fn ensure_pool(
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
