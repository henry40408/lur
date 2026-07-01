//! `lur.kv` — key/value storage over the shared `lur_kv(key TEXT, value BLOB)`
//! table (spec §6). Keys are strings, values raw bytes. Atomic operations
//! (add/cas/incr/decr/update) use `SQLite`'s own atomicity; see the design spec.

use std::cell::Cell;

use mlua::{Error, Lua, Table, Value};
use sqlx::{Row, TypeInfo, ValueRef};

use super::db::{self, SqliteShared};
use crate::capabilities::argcheck;
use crate::runtime::RunError;

thread_local! {
    /// Set while a kv.update transform runs, so a nested lur.kv/lur.db call on
    /// the same stack raises a clear error instead of deadlocking on the pinned
    /// transaction connection.
    static IN_KV_UPDATE: Cell<bool> = const { Cell::new(false) };
}

fn reject_kv_reentry(fname: &str) -> mlua::Result<()> {
    if IN_KV_UPDATE.with(Cell::get) {
        return Err(Error::runtime(format!(
            "{fname}: cannot re-enter lur.kv from inside update()"
        )));
    }
    Ok(())
}

/// Install `lur.kv` sharing `db`'s lazily-opened pool.
pub fn install(lua: &Lua, lur: &Table, shared: &SqliteShared) -> Result<(), RunError> {
    let kv = lua.create_table().map_err(RunError::Init)?;

    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let get = lua
            .create_async_function(move |lua, key: String| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.get")?;
                    let pool = db::ensure_pool(&cell, &path).await?;
                    let row = sqlx::query("SELECT value FROM lur_kv WHERE key = ?")
                        .bind(key)
                        .fetch_optional(&pool)
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
            })
            .map_err(RunError::Init)?;
        kv.set("get", get).map_err(RunError::Init)?;
    }
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let set = lua
            .create_async_function(move |_, (key, value): (String, mlua::String)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.set")?;
                    let pool = db::ensure_pool(&cell, &path).await?;
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
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let delete = lua
            .create_async_function(move |_, key: String| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.delete")?;
                    let pool = db::ensure_pool(&cell, &path).await?;
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
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let add = lua
            .create_async_function(move |_, (key, value): (String, mlua::String)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.add")?;
                    let pool = db::ensure_pool(&cell, &path).await?;
                    let res = db::retry_busy(|| async {
                        sqlx::query(
                            "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                             ON CONFLICT(key) DO NOTHING",
                        )
                        .bind(key.clone())
                        .bind(value.as_bytes().to_vec())
                        .execute(&pool)
                        .await
                    })
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.add: {e}")))?;
                    Ok(res.rows_affected() == 1)
                }
            })
            .map_err(RunError::Init)?;
        kv.set("add", add).map_err(RunError::Init)?;
    }
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let cas = lua
            .create_async_function(
                move |_,
                      (key, expected, new): (
                    String,
                    Option<mlua::String>,
                    Option<mlua::String>,
                )| {
                    let cell = std::sync::Arc::clone(&cell);
                    let path = std::sync::Arc::clone(&path);
                    async move {
                        reject_kv_reentry("lur.kv.cas")?;
                        let pool = db::ensure_pool(&cell, &path).await?;
                        let exp = expected.map(|s| s.as_bytes().to_vec());
                        let neu = new.map(|s| s.as_bytes().to_vec());
                        let applied = match (exp, neu) {
                            // expect absent, set new
                            (None, Some(v)) => {
                                db::retry_busy(|| async {
                                    sqlx::query(
                                        "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                                         ON CONFLICT(key) DO NOTHING",
                                    )
                                    .bind(key.clone())
                                    .bind(v.clone())
                                    .execute(&pool)
                                    .await
                                })
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                .rows_affected()
                                    == 1
                            }
                            // expect absent, want absent: succeeds iff already absent
                            (None, None) => {
                                let r = sqlx::query("SELECT 1 FROM lur_kv WHERE key = ?")
                                    .bind(key)
                                    .fetch_optional(&pool)
                                    .await
                                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?;
                                r.is_none()
                            }
                            // expect value, set new
                            (Some(e), Some(v)) => {
                                db::retry_busy(|| async {
                                    sqlx::query(
                                        "UPDATE lur_kv SET value = ? WHERE key = ? AND value = ?",
                                    )
                                    .bind(v.clone())
                                    .bind(key.clone())
                                    .bind(e.clone())
                                    .execute(&pool)
                                    .await
                                })
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                .rows_affected()
                                    == 1
                            }
                            // expect value, delete
                            (Some(e), None) => {
                                db::retry_busy(|| async {
                                    sqlx::query("DELETE FROM lur_kv WHERE key = ? AND value = ?")
                                        .bind(key.clone())
                                        .bind(e.clone())
                                        .execute(&pool)
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
                },
            )
            .map_err(RunError::Init)?;
        kv.set("cas", cas).map_err(RunError::Init)?;
    }
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let incr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.incr")?;
                    let n = argcheck::integer_arg(n, "lur.kv.incr", 2)?;
                    incr_by("lur.kv.incr", &cell, &path, key, n.unwrap_or(1)).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("incr", incr).map_err(RunError::Init)?;
    }
    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let decr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.decr")?;
                    let n = argcheck::integer_arg(n, "lur.kv.decr", 2)?;
                    let delta = n
                        .unwrap_or(1)
                        .checked_neg()
                        .ok_or_else(|| Error::runtime("lur.kv.decr: step too large"))?;
                    incr_by("lur.kv.decr", &cell, &path, key, delta).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("decr", decr).map_err(RunError::Init)?;
    }

    {
        let cell = std::sync::Arc::clone(&shared.cell);
        let path = std::sync::Arc::clone(&shared.path);
        let update = lua
            .create_async_function(move |lua, (key, func): (String, mlua::Function)| {
                let cell = std::sync::Arc::clone(&cell);
                let path = std::sync::Arc::clone(&path);
                async move {
                    reject_kv_reentry("lur.kv.update")?;
                    let pool = db::ensure_pool(&cell, &path).await?;
                    let mut conn = db::begin_immediate(&pool).await?;

                    // Run the full read → transform → write → commit sequence.
                    // Every `?` propagates an Err out of this inner block; the
                    // mutable borrow of `conn` ends when this Future is awaited
                    // and dropped, freeing `conn` for the rollback below.
                    let result: mlua::Result<Value> = async {
                        // read current value as bytes|nil (type-aware, same as get)
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

                        IN_KV_UPDATE.with(|f| f.set(true));
                        let transform_result = func.call_async::<Value>(cur).await;
                        IN_KV_UPDATE.with(|f| f.set(false));

                        let new = match transform_result {
                            Ok(v) => v,
                            Err(e) => return Err(e),
                        };

                        match &new {
                            Value::Nil => {
                                sqlx::query("DELETE FROM lur_kv WHERE key = ?")
                                    .bind(&key)
                                    .execute(&mut *conn)
                                    .await
                                    .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                            }
                            Value::String(s) => {
                                sqlx::query(
                                    "INSERT OR REPLACE INTO lur_kv (key, value) VALUES (?, ?)",
                                )
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
            })
            .map_err(RunError::Init)?;
        kv.set("update", update).map_err(RunError::Init)?;
    }

    lur.set("kv", kv).map_err(RunError::Init)?;
    Ok(())
}

/// Atomically add `delta` to an integer counter `key`, creating it at `delta`
/// when absent. Returns the new value, or errors if the key holds a
/// non-integer (the `WHERE typeof(value)='integer'` guard returns no row).
async fn incr_by(
    voice: &str,
    cell: &std::sync::Arc<std::sync::OnceLock<sqlx::sqlite::SqlitePool>>,
    path: &std::sync::Arc<Option<std::path::PathBuf>>,
    key: String,
    delta: i64,
) -> mlua::Result<i64> {
    let pool = db::ensure_pool(cell, path).await?;
    let row = db::retry_busy(|| async {
        sqlx::query(
            "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = value + excluded.value \
             WHERE typeof(lur_kv.value) = 'integer' \
             RETURNING value",
        )
        .bind(key.clone())
        .bind(delta)
        .fetch_optional(&pool)
        .await
    })
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

/// Decode column 0 of a single-column row into bytes, returning `None` for
/// NULL. INTEGER and REAL values are rendered as their decimal string; TEXT/BLOB
/// come back as raw bytes. Never panics on a type mismatch.
///
/// A later task (`kv.update`) reuses this helper for the same type dispatch.
fn value_to_bytes(row: &sqlx::sqlite::SqliteRow) -> mlua::Result<Option<Vec<u8>>> {
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
fn decode<'r, T>(row: &'r sqlx::sqlite::SqliteRow) -> mlua::Result<T>
where
    T: sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get::<T, usize>(0)
        .map_err(|e| Error::runtime(format!("lur.kv: decoding value: {e}")))
}
