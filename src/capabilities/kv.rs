//! `lur.kv` — key/value storage over the shared `lur_kv(key TEXT, value BLOB)`
//! table (spec §6). Keys are strings, values raw bytes. Atomic operations
//! (add/cas/incr/decr/update) use `SQLite`'s own atomicity; see the design spec.

use std::cell::Cell;

use mlua::{Error, Function, Lua, Table, Value};
use sqlx::Row;

use crate::capabilities::argcheck;
use crate::capabilities::null;
use crate::capabilities::storage::Shared;
use crate::capabilities::storage::sqlite::{retry_busy, value_to_bytes};
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

/// Reconstruct the byte-string `lur.kv` value model from a `value` field read
/// back through the backend-neutral `Transaction::query` row mapping (which
/// classifies `SQLite` columns as `Integer`/`Number`/`String`). This mirrors
/// `value_to_bytes`'s stringification of INTEGER/REAL so a value written and
/// then read back through either path is byte-identical.
fn field_to_kv_value(lua: &Lua, v: Value) -> mlua::Result<Value> {
    match v {
        Value::Integer(i) => Ok(Value::String(lua.create_string(i.to_string())?)),
        Value::Number(n) => Ok(Value::String(lua.create_string(n.to_string())?)),
        Value::String(_) => Ok(v),
        other if null::is_null(&other) => Ok(Value::Nil),
        other => Err(Error::runtime(format!(
            "lur.kv.update: unexpected stored value type {}",
            other.type_name()
        ))),
    }
}

/// Install `lur.kv` sharing `db`'s lazily-opened backend.
pub(crate) fn install(lua: &Lua, lur: &Table, shared: &Shared) -> Result<(), RunError> {
    let kv = lua.create_table().map_err(RunError::Init)?;

    {
        let shared = shared.clone();
        let get = lua
            .create_async_function(move |lua, key: String| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.get")?;
                    let backend = shared.ensure().await?;
                    let pool = backend.as_sqlite_pool();
                    let row = sqlx::query("SELECT value FROM lur_kv WHERE key = ?")
                        .bind(key)
                        .fetch_optional(pool)
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
        let shared = shared.clone();
        let set = lua
            .create_async_function(move |_, (key, value): (String, mlua::String)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.set")?;
                    let backend = shared.ensure().await?;
                    let pool = backend.as_sqlite_pool();
                    sqlx::query("INSERT OR REPLACE INTO lur_kv (key, value) VALUES (?, ?)")
                        .bind(key)
                        .bind(value.as_bytes().to_vec())
                        .execute(pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.set: {e}")))?;
                    Ok(())
                }
            })
            .map_err(RunError::Init)?;
        kv.set("set", set).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let delete = lua
            .create_async_function(move |_, key: String| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.delete")?;
                    let backend = shared.ensure().await?;
                    let pool = backend.as_sqlite_pool();
                    sqlx::query("DELETE FROM lur_kv WHERE key = ?")
                        .bind(key)
                        .execute(pool)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.delete: {e}")))?;
                    Ok(())
                }
            })
            .map_err(RunError::Init)?;
        kv.set("delete", delete).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let add = lua
            .create_async_function(move |_, (key, value): (String, mlua::String)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.add")?;
                    let backend = shared.ensure().await?;
                    let pool = backend.as_sqlite_pool();
                    let res = retry_busy(|| async {
                        sqlx::query(
                            "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                             ON CONFLICT(key) DO NOTHING",
                        )
                        .bind(key.clone())
                        .bind(value.as_bytes().to_vec())
                        .execute(pool)
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
        let shared = shared.clone();
        let cas = lua
            .create_async_function(
                move |_,
                      (key, expected, new): (
                    String,
                    Option<mlua::String>,
                    Option<mlua::String>,
                )| {
                    let shared = shared.clone();
                    async move {
                        reject_kv_reentry("lur.kv.cas")?;
                        let backend = shared.ensure().await?;
                        let pool = backend.as_sqlite_pool();
                        let exp = expected.map(|s| s.as_bytes().to_vec());
                        let neu = new.map(|s| s.as_bytes().to_vec());
                        let applied = match (exp, neu) {
                            // expect absent, set new
                            (None, Some(v)) => {
                                retry_busy(|| async {
                                    sqlx::query(
                                        "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
                                         ON CONFLICT(key) DO NOTHING",
                                    )
                                    .bind(key.clone())
                                    .bind(v.clone())
                                    .execute(pool)
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
                                    .fetch_optional(pool)
                                    .await
                                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?;
                                r.is_none()
                            }
                            // expect value, set new
                            (Some(e), Some(v)) => {
                                retry_busy(|| async {
                                    sqlx::query(
                                        "UPDATE lur_kv SET value = ? WHERE key = ? AND value = ?",
                                    )
                                    .bind(v.clone())
                                    .bind(key.clone())
                                    .bind(e.clone())
                                    .execute(pool)
                                    .await
                                })
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                                .rows_affected()
                                    == 1
                            }
                            // expect value, delete
                            (Some(e), None) => {
                                retry_busy(|| async {
                                    sqlx::query("DELETE FROM lur_kv WHERE key = ? AND value = ?")
                                        .bind(key.clone())
                                        .bind(e.clone())
                                        .execute(pool)
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
        let shared = shared.clone();
        let incr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.incr")?;
                    let n = argcheck::integer_arg(n, "lur.kv.incr", 2)?;
                    incr_by("lur.kv.incr", &shared, key, n.unwrap_or(1)).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("incr", incr).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let decr = lua
            .create_async_function(move |_, (key, n): (String, Value)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.decr")?;
                    let n = argcheck::integer_arg(n, "lur.kv.decr", 2)?;
                    let delta = n
                        .unwrap_or(1)
                        .checked_neg()
                        .ok_or_else(|| Error::runtime("lur.kv.decr: step too large"))?;
                    incr_by("lur.kv.decr", &shared, key, delta).await
                }
            })
            .map_err(RunError::Init)?;
        kv.set("decr", decr).map_err(RunError::Init)?;
    }

    {
        let shared = shared.clone();
        let update = lua
            .create_async_function(move |lua, (key, func): (String, Function)| {
                let shared = shared.clone();
                async move {
                    reject_kv_reentry("lur.kv.update")?;
                    let backend = shared.ensure().await?;
                    let tx = backend.begin().await?;

                    // Run the full read → transform → write sequence, then
                    // commit/roll back below based on its outcome.
                    let result: mlua::Result<Value> = async {
                        // read current value as bytes|nil (type-aware, same as get)
                        let rows = tx
                            .query(
                                &lua,
                                "SELECT value FROM lur_kv WHERE key = ?".to_string(),
                                vec![Value::String(lua.create_string(&key)?)],
                            )
                            .await
                            .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                        let cur: Value = if rows.raw_len() == 0 {
                            Value::Nil
                        } else {
                            let row: Table = rows.raw_get(1)?;
                            let v: Value = row.raw_get("value")?;
                            field_to_kv_value(&lua, v)?
                        };

                        IN_KV_UPDATE.with(|f| f.set(true));
                        let transform_result = func.call_async::<Value>(cur).await;
                        IN_KV_UPDATE.with(|f| f.set(false));

                        let new = transform_result?;

                        match &new {
                            Value::Nil => {
                                tx.exec(
                                    &lua,
                                    "DELETE FROM lur_kv WHERE key = ?".to_string(),
                                    vec![Value::String(lua.create_string(&key)?)],
                                )
                                .await
                                .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                            }
                            Value::String(s) => {
                                tx.exec(
                                    &lua,
                                    "INSERT OR REPLACE INTO lur_kv (key, value) VALUES (?, ?)"
                                        .to_string(),
                                    vec![
                                        Value::String(lua.create_string(&key)?),
                                        Value::String(s.clone()),
                                    ],
                                )
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
                        Ok(new)
                    }
                    .await;

                    // On any error from the sequence above, roll back the open
                    // transaction and return the original error unchanged.
                    match result {
                        Ok(v) => {
                            tx.commit().await.map_err(|e| {
                                Error::runtime(format!("lur.kv.update: commit: {e}"))
                            })?;
                            Ok(v)
                        }
                        Err(e) => {
                            tx.rollback().await;
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
async fn incr_by(voice: &str, shared: &Shared, key: String, delta: i64) -> mlua::Result<i64> {
    let backend = shared.ensure().await?;
    let pool = backend.as_sqlite_pool();
    let row = retry_busy(|| async {
        sqlx::query(
            "INSERT INTO lur_kv (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = value + excluded.value \
             WHERE typeof(lur_kv.value) = 'integer' \
             RETURNING value",
        )
        .bind(key.clone())
        .bind(delta)
        .fetch_optional(pool)
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
