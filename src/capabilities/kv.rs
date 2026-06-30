//! `lur.kv` — key/value storage over the shared `lur_kv(key TEXT, value BLOB)`
//! table (spec §6). Keys are strings, values raw bytes. Atomic operations
//! (add/cas/incr/decr/update) use `SQLite`'s own atomicity; see the design spec.

use mlua::{Error, Lua, Table, Value};
use sqlx::{Row, TypeInfo, ValueRef};

use super::db::{self, SqliteShared};
use crate::runtime::RunError;

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

    lur.set("kv", kv).map_err(RunError::Init)?;
    Ok(())
}

/// Decode column 0 of a single-column row into bytes, returning `None` for
/// NULL. INTEGER and REAL values are rendered as their decimal string; TEXT/BLOB
/// come back as raw bytes. Never panics on a type mismatch.
///
/// A later task (`kv.update`) reuses this helper for the same type dispatch.
fn value_to_bytes(row: &sqlx::sqlite::SqliteRow) -> mlua::Result<Option<Vec<u8>>> {
    let raw = row
        .try_get_raw(0)
        .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
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
        .map_err(|e| Error::runtime(format!("lur.kv.get: decoding value: {e}")))
}
