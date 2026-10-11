//! `lur.async`: `sleep` plus `all`/`race`/`settled`/`any`, mirroring
//! JS `Promise.*`, and `map` (concurrent `fn(item, index)` over an array).
//! Tasks interleave on one VM only at await points; when a combinator
//! settles early the rest are dropped (cancelled).
//!
//! `map` takes an optional per-call `limit` semaphore *before* the global
//! `--max-concurrency` one, and drives a `FuturesUnordered` so the first
//! error returns immediately regardless of list length.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::{join_all, select_all, try_join_all};
use futures_util::stream::FuturesUnordered;
use mlua::{Function, Lua, Table, Value};
use tokio::sync::Semaphore;

use super::argcheck;
use crate::runtime::RunError;

/// Elements of a dense array table, or `None` when it has non-sequence keys.
fn array_items(t: &Table) -> mlua::Result<Option<Vec<Value>>> {
    let len = t.raw_len();
    let mut count = 0usize;
    for pair in t.clone().pairs::<Value, Value>() {
        pair?;
        count += 1;
    }
    if count != len {
        return Ok(None);
    }
    (1..=len)
        .map(|i| t.raw_get(i))
        .collect::<mlua::Result<Vec<Value>>>()
        .map(Some)
}

/// One `map` item: take a per-call `limit` permit first, then the global
/// `max_concurrency` permit, so queued items never hoard global permits.
async fn map_task(
    f: Function,
    item: Value,
    index: usize,
    local: Option<Arc<Semaphore>>,
    global: Option<Arc<Semaphore>>,
) -> mlua::Result<Value> {
    let _local = match local {
        Some(s) => Some(s.acquire_owned().await.expect("semaphore never closed")),
        None => None,
    };
    let _global = match global {
        Some(s) => Some(s.acquire_owned().await.expect("semaphore never closed")),
        None => None,
    };
    f.call_async::<Value>((item, index)).await
}

fn task_list(tasks: &Table) -> mlua::Result<Vec<Function>> {
    tasks.clone().sequence_values::<Function>().collect()
}

/// Run one task, holding a `max_concurrency` permit (if capped) until it settles.
async fn run_task(f: Function, sem: Option<Arc<Semaphore>>) -> mlua::Result<Value> {
    let _permit = match sem {
        Some(s) => Some(s.acquire_owned().await.expect("semaphore never closed")),
        None => None,
    };
    f.call_async::<Value>(()).await
}

/// `max_concurrency` caps concurrently running combinator tasks per VM.
pub fn install(lua: &Lua, lur: &Table, max_concurrency: Option<usize>) -> Result<(), RunError> {
    let sem = max_concurrency.map(|n| Arc::new(Semaphore::new(n)));
    let async_tbl = lua.create_table().map_err(RunError::Init)?;

    // No Lua runs while parked, so only the wall-clock timeout (not the
    // interrupt) can cut this short.
    let sleep = lua
        .create_async_function(|_, ms: u64| async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(())
        })
        .map_err(RunError::Init)?;
    async_tbl.set("sleep", sleep).map_err(RunError::Init)?;

    // all: results in order; the first error re-raises and cancels the rest.
    let sem_all = sem.clone();
    let all = lua
        .create_async_function(move |lua, tasks: Table| {
            let sem = sem_all.clone();
            async move {
                let funcs = task_list(&tasks)?;
                let futs = funcs.into_iter().map(|f| run_task(f, sem.clone()));
                let results: Vec<Value> = try_join_all(futs).await?;
                let arr = lua.create_table()?;
                for (i, v) in results.into_iter().enumerate() {
                    arr.set(i + 1, v)?;
                }
                Ok(arr)
            }
        })
        .map_err(RunError::Init)?;
    async_tbl.set("all", all).map_err(RunError::Init)?;

    // settled: never raises; { ok = true, value } / { ok = false, err } per task.
    let sem_settled = sem.clone();
    let settled = lua
        .create_async_function(move |lua, tasks: Table| {
            let sem = sem_settled.clone();
            async move {
                let funcs = task_list(&tasks)?;
                let futs = funcs.into_iter().map(|f| run_task(f, sem.clone()));
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
            }
        })
        .map_err(RunError::Init)?;
    async_tbl.set("settled", settled).map_err(RunError::Init)?;

    // race: the first task to settle wins, success or failure.
    let sem_race = sem.clone();
    let race = lua
        .create_async_function(move |_, tasks: Table| {
            let sem = sem_race.clone();
            async move {
                let funcs = task_list(&tasks)?;
                if funcs.is_empty() {
                    return Err(mlua::Error::RuntimeError("lur.async.race: no tasks".into()));
                }
                let futs = funcs
                    .into_iter()
                    .map(|f| Box::pin(run_task(f, sem.clone())))
                    .collect::<Vec<_>>();
                let (result, _index, _rest) = select_all(futs).await;
                result
            }
        })
        .map_err(RunError::Init)?;
    async_tbl.set("race", race).map_err(RunError::Init)?;

    // any: the first success wins; raises an aggregate error if all fail.
    let sem_any = sem.clone();
    let any = lua
        .create_async_function(move |_, tasks: Table| {
            let sem = sem_any.clone();
            async move {
                let funcs = task_list(&tasks)?;
                if funcs.is_empty() {
                    return Err(mlua::Error::RuntimeError("lur.async.any: no tasks".into()));
                }
                let mut futs = funcs
                    .into_iter()
                    .map(|f| Box::pin(run_task(f, sem.clone())))
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
            }
        })
        .map_err(RunError::Init)?;
    async_tbl.set("any", any).map_err(RunError::Init)?;

    // map: fn(item, index) over an array, results in input order. `limit`
    // bounds this call's in-flight tasks; `drop_errors` omits failed items
    // instead of failing fast.
    let sem_map = sem.clone();
    let map = lua
        .create_async_function(move |lua, (list, func, opts): (Value, Value, Value)| {
            let global = sem_map.clone();
            async move {
                const F: &str = "lur.async.map";
                let list: Table = argcheck::arg(&lua, list, F, 1, "table")?;
                let func: Function = argcheck::arg(&lua, func, F, 2, "function")?;
                let (mut limit, mut drop_errors) = (None, false);
                match opts {
                    Value::Nil => {}
                    Value::Table(o) => {
                        let raw: Value = o.get("limit")?;
                        let got = raw.type_name();
                        limit = argcheck::integer_arg(raw, F, 3).map_err(|_e| {
                            mlua::Error::RuntimeError(format!(
                                "{F}: argument #3 opts.limit must be an integer >= 1, got {got}"
                            ))
                        })?;
                        if let Some(n) = limit
                            && n < 1
                        {
                            return Err(mlua::Error::RuntimeError(format!(
                                "{F}: argument #3 opts.limit must be >= 1, got {n}"
                            )));
                        }
                        match o.get::<Value>("drop_errors")? {
                            Value::Nil => {}
                            Value::Boolean(b) => drop_errors = b,
                            other => {
                                return Err(mlua::Error::RuntimeError(format!(
                                    "{F}: argument #3 opts.drop_errors must be boolean, got {}",
                                    other.type_name()
                                )));
                            }
                        }
                    }
                    other => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "{F}: argument #3 must be table, got {}",
                            other.type_name()
                        )));
                    }
                }
                let items = array_items(&list)?.ok_or_else(|| {
                    mlua::Error::RuntimeError(format!(
                        "{F}: argument #1 must be an array, got a table with holes or non-array keys"
                    ))
                })?;
                let local = limit.map(|n| {
                    let permits = usize::try_from(n).map_or(Semaphore::MAX_PERMITS, |n| {
                        n.min(Semaphore::MAX_PERMITS)
                    });
                    Arc::new(Semaphore::new(permits))
                });
                // FuturesUnordered (not try_join_all, which becomes
                // FuturesOrdered past 30 futures and would delay an early
                // error behind earlier items): return on the first Err and
                // drop the rest, placing results by index.
                let count = items.len();
                let mut pending = FuturesUnordered::new();
                for (i, item) in items.into_iter().enumerate() {
                    let (f, l, g) = (func.clone(), local.clone(), global.clone());
                    pending.push(async move { (i, map_task(f, item, i + 1, l, g).await) });
                }
                let mut slots: Vec<Option<Value>> = vec![None; count];
                while let Some((i, result)) = pending.next().await {
                    match result {
                        Ok(v) => slots[i] = Some(v),
                        Err(e) if !drop_errors => return Err(e),
                        Err(_) => {}
                    }
                }
                let arr = lua.create_table()?;
                for (n, v) in slots.into_iter().flatten().enumerate() {
                    arr.set(n + 1, v)?;
                }
                Ok(arr)
            }
        })
        .map_err(RunError::Init)?;
    async_tbl.set("map", map).map_err(RunError::Init)?;

    lur.set("async", async_tbl).map_err(RunError::Init)?;
    Ok(())
}
