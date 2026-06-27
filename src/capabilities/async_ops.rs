//! `lur.async` — the concurrency API (spec §7): `sleep` plus the four
//! combinators `all` / `race` / `settled` / `any`, mirroring JS
//! `Promise.all` / `race` / `allSettled` / `any`.
//!
//! Each combinator wraps a `{ fn1, fn2, … }` array of zero-arg Lua functions
//! into futures driven concurrently on the one VM — Lua still runs one piece at
//! a time, interleaving only at I/O await points (probe-verified). Cancellation
//! is by drop: when a combinator settles early, the remaining futures are
//! dropped, aborting their coroutines.

use std::time::Duration;

use futures_util::future::{join_all, select_all, try_join_all};
use mlua::{Function, Lua, Table, Value};

use crate::runtime::RunError;

/// Collect the `{ fn1, fn2, … }` array part into a list of handler functions.
fn task_list(tasks: &Table) -> mlua::Result<Vec<Function>> {
    tasks.clone().sequence_values::<Function>().collect()
}

/// Install `lur.async.sleep` and the `all` / `race` / `settled` / `any` combinators.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let async_tbl = lua.create_table().map_err(RunError::Init)?;

    // `lur.async.sleep(ms)` parks on the tokio timer; while parked no Lua runs,
    // so this is the path the wall-clock timeout layer (not the interrupt)
    // guards (§5).
    let sleep = lua
        .create_async_function(|_, ms: u64| async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(())
        })
        .map_err(RunError::Init)?;
    async_tbl.set("sleep", sleep).map_err(RunError::Init)?;

    // all: await every task, results in argument order; the first error
    // re-raises (fail-fast) and the rest are cancelled.
    let all = lua
        .create_async_function(|lua, tasks: Table| async move {
            let funcs = task_list(&tasks)?;
            let futs = funcs.iter().map(|f| f.call_async::<Value>(()));
            let results: Vec<Value> = try_join_all(futs).await?;
            let arr = lua.create_table()?;
            for (i, v) in results.into_iter().enumerate() {
                arr.set(i + 1, v)?;
            }
            Ok(arr)
        })
        .map_err(RunError::Init)?;
    async_tbl.set("all", all).map_err(RunError::Init)?;

    // settled: await every task but never raise; a per-task array of
    // { ok = true, value } / { ok = false, err }.
    let settled = lua
        .create_async_function(|lua, tasks: Table| async move {
            let funcs = task_list(&tasks)?;
            let futs = funcs.iter().map(|f| f.call_async::<Value>(()));
            let results = join_all(futs).await;
            let arr = lua.create_table()?;
            for (i, result) in results.into_iter().enumerate() {
                let entry = lua.create_table()?;
                match result {
                    Ok(value) => {
                        entry.set("ok", true)?;
                        entry.set("value", value)?;
                    }
                    Err(e) => {
                        entry.set("ok", false)?;
                        entry.set("err", e.to_string())?;
                    }
                }
                arr.set(i + 1, entry)?;
            }
            Ok(arr)
        })
        .map_err(RunError::Init)?;
    async_tbl.set("settled", settled).map_err(RunError::Init)?;

    // race: return as soon as the FIRST task settles (success or failure); its
    // value, or re-raise its error. The rest are cancelled.
    let race = lua
        .create_async_function(|_, tasks: Table| async move {
            let funcs = task_list(&tasks)?;
            if funcs.is_empty() {
                return Err(mlua::Error::RuntimeError("lur.async.race: no tasks".into()));
            }
            let futs = funcs
                .iter()
                .map(|f| Box::pin(f.call_async::<Value>(())))
                .collect::<Vec<_>>();
            let (result, _index, _rest) = select_all(futs).await;
            result
        })
        .map_err(RunError::Init)?;
    async_tbl.set("race", race).map_err(RunError::Init)?;

    // any: return the first task to SUCCEED; if every task fails, raise an
    // aggregate error. The rest are cancelled once one succeeds.
    let any = lua
        .create_async_function(|_, tasks: Table| async move {
            let funcs = task_list(&tasks)?;
            if funcs.is_empty() {
                return Err(mlua::Error::RuntimeError("lur.async.any: no tasks".into()));
            }
            let mut futs = funcs
                .iter()
                .map(|f| Box::pin(f.call_async::<Value>(())))
                .collect::<Vec<_>>();
            let mut errors: Vec<String> = Vec::new();
            while !futs.is_empty() {
                let (result, _index, rest) = select_all(futs).await;
                match result {
                    Ok(value) => return Ok(value),
                    Err(e) => {
                        errors.push(e.to_string());
                        futs = rest;
                    }
                }
            }
            Err(mlua::Error::RuntimeError(format!(
                "lur.async.any: all {} tasks failed: {}",
                errors.len(),
                errors.join("; ")
            )))
        })
        .map_err(RunError::Init)?;
    async_tbl.set("any", any).map_err(RunError::Init)?;

    lur.set("async", async_tbl).map_err(RunError::Init)?;
    Ok(())
}
