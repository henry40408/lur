//! `lur.db` — long-term `SQLite` storage. This file only wires the Lua `lur.db`
//! table to the backend-neutral `storage::Backend`; all SQL/dialect lives in
//! `storage`.

use mlua::{Error, Function, Lua, MultiValue, Table, Value, Variadic};
use std::path::PathBuf;
use std::sync::{Arc, Weak};

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
///
/// `run_tx` holds the sole strong `Arc<Transaction>`; the `exec`/`query`
/// closures capture `Weak` refs. On cancellation mid-transform the strong ref
/// drops with this frame, firing `Transaction::Drop` (the detached rollback)
/// synchronously rather than deferring it to Luau GC.
async fn run_tx(lua: Lua, shared: &Shared, func: Function) -> mlua::Result<()> {
    let backend = shared.ensure().await?;
    let tx = Arc::new(backend.begin().await?);

    let handle = lua.create_table()?;
    {
        let tx: Weak<crate::capabilities::storage::Transaction> = Arc::downgrade(&tx);
        let exec =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let tx = tx.clone();
                async move {
                    let tx = tx.upgrade().ok_or_else(|| {
                        Error::runtime(
                            "lur.db.tx: transaction handle used after the transaction ended",
                        )
                    })?;
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
        let tx: Weak<crate::capabilities::storage::Transaction> = Arc::downgrade(&tx);
        let query =
            lua.create_async_function(move |lua, (sql, params): (String, Variadic<Value>)| {
                let tx = tx.clone();
                async move {
                    let tx = tx.upgrade().ok_or_else(|| {
                        Error::runtime(
                            "lur.db.tx: transaction handle used after the transaction ended",
                        )
                    })?;
                    tx.query(&lua, sql, params.into_iter().collect()).await
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::storage::{Shared, sqlite_max1_backend};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    // A db.tx whose body writes a row then parks, cancelled mid-transform, must
    // roll back synchronously: the sole pooled connection is released promptly
    // (a fresh begin does not hang) and the written row is gone. Before the
    // Weak-ref fix the two Arc clones held in the Lua exec/query closures kept
    // the transaction alive until GC, so the connection stayed checked out and
    // this second begin would block until the pool acquire timeout.
    #[test]
    fn db_tx_cancellation_rolls_back_synchronously() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let backend = sqlite_max1_backend(dir.path()).await;
            let shared = Shared::from_backend(backend.clone());
            let lua = Lua::new();

            // func(tx): insert a row, signal entry, then park forever.
            let entered = Arc::new(Notify::new());
            let entered2 = entered.clone();
            let func = lua
                .create_async_function(move |_lua, handle: Table| {
                    let entered2 = entered2.clone();
                    async move {
                        let exec: Function = handle.get("exec")?;
                        exec.call_async::<Table>((
                            "INSERT INTO lur_kv (key, value) VALUES ('k', 'v')".to_string(),
                        ))
                        .await?;
                        entered2.notify_one();
                        std::future::pending::<mlua::Result<()>>().await
                    }
                })
                .unwrap();

            let mut fut = Box::pin(run_tx(lua.clone(), &shared, func));
            tokio::select! {
                _ = &mut fut => panic!("run_tx should park in the transform"),
                _ = entered.notified() => {}
            }
            drop(fut); // cancel mid-transform → strong Arc drops → rollback fires

            // Bounded so an unfixed regression fails fast instead of hanging the
            // suite for the full pool-acquire timeout.
            let tx2 = tokio::time::timeout(Duration::from_secs(5), backend.begin())
                .await
                .expect("second begin must not hang: the connection must be released")
                .expect("second begin must succeed after the cancelled tx rolled back");
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
                "row from the cancelled db.tx must be rolled back"
            );
            tx2.rollback().await;
        });
    }
}
