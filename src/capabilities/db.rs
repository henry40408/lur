//! `lur.db` — long-term `SQLite` storage. This file only wires the Lua `lur.db`
//! table to the backend-neutral `storage::Backend`; all SQL/dialect lives in
//! `storage`.

use mlua::{Function, Lua, MultiValue, Table, Value, Variadic};
use std::path::PathBuf;
use std::sync::Arc;

use crate::capabilities::storage::Shared;
use crate::runtime::RunError;

/// Install `lur.db`. Returns the shared backend handle so `kv::install` reuses it.
pub(crate) fn install(
    lua: &Lua,
    lur: &Table,
    db_path: Option<PathBuf>,
) -> Result<Shared, RunError> {
    let shared = Shared::new(db_path);
    let db = lua.create_table().map_err(RunError::Init)?;

    {
        let shared = shared.clone();
        let exec = lua
            .create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = shared.clone();
                async move {
                    let backend = shared.ensure().await?;
                    let res = backend
                        .exec(&lua, sql, params.into_iter().collect())
                        .await?;
                    let t = lua.create_table()?;
                    t.set("rows_affected", res.rows_affected)?;
                    t.set("last_insert_id", res.last_insert_id)?;
                    Ok(t)
                }
            })
            .map_err(RunError::Init)?;
        db.set("exec", exec).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let query = lua
            .create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let shared = shared.clone();
                async move {
                    let backend = shared.ensure().await?;
                    backend.query(&lua, sql, params.into_iter().collect()).await
                }
            })
            .map_err(RunError::Init)?;
        db.set("query", query).map_err(RunError::Init)?;
    }
    {
        let shared = shared.clone();
        let tx = lua
            .create_async_function(move |lua, func: Function| {
                let shared = shared.clone();
                async move { run_tx(lua, &shared, func).await }
            })
            .map_err(RunError::Init)?;
        db.set("tx", tx).map_err(RunError::Init)?;
    }

    lur.set("db", db).map_err(RunError::Init)?;
    Ok(shared)
}

/// Build the Lua `tx` handle over a pinned-connection transaction, run
/// `func(tx)`, then commit on normal return / roll back and re-raise on error.
async fn run_tx(lua: Lua, shared: &Shared, func: Function) -> mlua::Result<()> {
    let backend = shared.ensure().await?;
    let tx = Arc::new(backend.begin().await?);

    let handle = lua.create_table()?;
    {
        let tx = Arc::clone(&tx);
        let exec =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let tx = Arc::clone(&tx);
                async move {
                    let res = tx.exec(&lua, sql, params.into_iter().collect()).await?;
                    let t = lua.create_table()?;
                    t.set("rows_affected", res.rows_affected)?;
                    t.set("last_insert_id", res.last_insert_id)?;
                    Ok(t)
                }
            })?;
        handle.set("exec", exec)?;
    }
    {
        let tx = Arc::clone(&tx);
        let query =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let tx = Arc::clone(&tx);
                async move { tx.query(&lua, sql, params.into_iter().collect()).await }
            })?;
        handle.set("query", query)?;
    }

    match func.call_async::<MultiValue>(handle).await {
        Ok(_) => tx.commit().await,
        Err(e) => {
            tx.rollback().await;
            Err(e)
        }
    }
}
