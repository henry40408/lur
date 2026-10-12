//! `lur.async`: `sleep` plus `all`/`race`/`settled`/`any`, mirroring
//! JS `Promise.*`, and `map` (concurrent `fn(item, index)` over an array).
//! Tasks interleave on one VM only at await points; when a combinator
//! settles early the rest are dropped (cancelled).
//!
//! Every combinator runs through [`drive`]: a `FuturesUnordered` holding at
//! most a *window* of futures (`--max-concurrency`, plus `map`'s per-call
//! `limit`), refilled as items finish. A long list therefore never parks
//! thousands of futures on the global semaphore (quadratic in list length),
//! and the first error returns immediately regardless of list length.
//!
//! Error tables (`lur.validate`, `lur.serve.abort`) raised in a task reach
//! the combinator flattened to a string; [`fail`] recovers the table
//! ([`raise::recover`]) and `all`/`race`/`map` re-raise it through a
//! [`raise::raising_hot`] wrapper, so `pcall` around the combinator sees the
//! table. While tasks succeed the only extra cost is that wrapper's frame.

use std::future::Future;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use mlua::{Function, Lua, Table, Value};
use tokio::sync::Semaphore;

use super::{argcheck, raise, state};
use crate::runtime::RunError;

/// A task's failure: the error table it raised (recovered), or any other error.
enum Failure {
    Table(Table),
    Other(mlua::Error),
}

/// Classify a task error; call it right after the task fails (see
/// [`raise::recover`]).
fn fail(lua: &Lua, e: mlua::Error) -> Failure {
    match raise::recover(lua, &e) {
        Some(t) => Failure::Table(t),
        None => Failure::Other(e),
    }
}

/// What a wrapped combinator's inner function returns on failure: the
/// sentinel carrying the error table for the wrapper to raise, or a plain `Err`.
fn outcome(sentinel: &raise::Sentinel, failure: Failure) -> mlua::Result<Value> {
    match failure {
        Failure::Table(t) => sentinel.raise(t),
        Failure::Other(e) => Err(e),
    }
}

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

/// Poll `items` with at most `window` futures alive, creating the next only
/// when one finishes. `on_result(index, result)` sees results in completion
/// order; `Break(b)` stops early, dropping (cancelling) everything in flight
/// and queued. Returns the break value, or `None` once every item finished.
async fn drive<T, F, B>(
    items: Vec<T>,
    window: usize,
    mut make: impl FnMut(usize, T) -> F,
    mut on_result: impl FnMut(usize, mlua::Result<Value>) -> ControlFlow<B>,
) -> Option<B>
where
    F: Future<Output = mlua::Result<Value>>,
{
    let window = window.max(1);
    let mut queue = items.into_iter().enumerate();
    let mut pending = FuturesUnordered::new();
    loop {
        while pending.len() < window {
            let Some((i, item)) = queue.next() else { break };
            let fut = make(i, item);
            pending.push(async move { (i, fut.await) });
        }
        let (i, result) = pending.next().await?;
        if let ControlFlow::Break(b) = on_result(i, result) {
            return Some(b);
        }
    }
}

/// One `map` item. The per-call `limit` is enforced by the caller's window,
/// so only the global `max_concurrency` permit is taken here.
async fn map_task(
    f: Function,
    item: Value,
    index: usize,
    global: Option<Arc<Semaphore>>,
    group: Option<&state::TaskGroup>,
) -> mlua::Result<Value> {
    let _global = match global {
        Some(s) => Some(s.acquire_owned().await.expect("semaphore never closed")),
        None => None,
    };
    // Dropped after `call` (reverse declaration order): see `state::Task`.
    let task = state::Task::start(group);
    let mut call = std::pin::pin!(f.call_async::<Value>((item, index)));
    std::future::poll_fn(|cx| {
        let _polling = state::Task::enter(task.as_ref());
        call.as_mut().poll(cx)
    })
    .await
}

fn task_list(tasks: &Table) -> mlua::Result<Vec<Function>> {
    tasks.clone().sequence_values::<Function>().collect()
}

/// Run one task, holding a `max_concurrency` permit (if capped) until it settles.
async fn run_task(
    f: Function,
    sem: Option<Arc<Semaphore>>,
    group: Option<&state::TaskGroup>,
) -> mlua::Result<Value> {
    let _permit = match sem {
        Some(s) => Some(s.acquire_owned().await.expect("semaphore never closed")),
        None => None,
    };
    // Dropped after `call` (reverse declaration order): see `state::Task`.
    let task = state::Task::start(group);
    let mut call = std::pin::pin!(f.call_async::<Value>(()));
    std::future::poll_fn(|cx| {
        let _polling = state::Task::enter(task.as_ref());
        call.as_mut().poll(cx)
    })
    .await
}

/// `max_concurrency` caps concurrently running combinator tasks per VM.
/// `factory` is the VM's [`raise::factory`].
pub fn install(
    lua: &Lua,
    lur: &Table,
    factory: &Function,
    max_concurrency: Option<usize>,
) -> Result<(), RunError> {
    // Zero would park every task until the timeout; clamp for library callers
    // (the CLI rejects it earlier).
    let max_concurrency = max_concurrency.map(|n| n.max(1));
    let sem = max_concurrency.map(|n| Arc::new(Semaphore::new(n)));
    // A call can never run more than `max_concurrency` tasks, so that is also
    // the most futures worth keeping alive for it.
    let cap = max_concurrency.unwrap_or(usize::MAX);
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
    // Arc: cloned into every call, and a `Table` clone is a registry ref.
    let sentinel = Arc::new(raise::Sentinel::new(lua).map_err(RunError::Init)?);
    let sem_all = sem.clone();
    let fail_all = sentinel.clone();
    let all = lua
        .create_async_function(move |lua, tasks: Table| {
            let sem = sem_all.clone();
            let sentinel = fail_all.clone();
            async move {
                let funcs = task_list(&tasks)?;
                let group = state::TaskGroup::new(&lua);
                let mut slots = vec![Value::Nil; funcs.len()];
                let failed = drive(
                    funcs,
                    cap,
                    |_, f| run_task(f, sem.clone(), group.as_ref()),
                    |i, result| match result {
                        Ok(v) => {
                            slots[i] = v;
                            ControlFlow::Continue(())
                        }
                        Err(e) => ControlFlow::Break(fail(&lua, e)),
                    },
                )
                .await;
                if let Some(f) = failed {
                    return outcome(&sentinel, f);
                }
                let arr = lua.create_table()?;
                for (i, v) in slots.into_iter().enumerate() {
                    arr.set(i + 1, v)?;
                }
                Ok(Value::Table(arr))
            }
        })
        .map_err(RunError::Init)?;
    let all = raise::raising_hot(factory, all, &sentinel).map_err(RunError::Init)?;
    async_tbl.set("all", all).map_err(RunError::Init)?;

    // settled: never raises; { ok = true, value } / { ok = false, err } per task.
    let sem_settled = sem.clone();
    let settled = lua
        .create_async_function(move |lua, tasks: Table| {
            let sem = sem_settled.clone();
            async move {
                let funcs = task_list(&tasks)?;
                let group = state::TaskGroup::new(&lua);
                let mut slots: Vec<Option<Result<Value, Failure>>> =
                    std::iter::repeat_with(|| None).take(funcs.len()).collect();
                drive(
                    funcs,
                    cap,
                    |_, f| run_task(f, sem.clone(), group.as_ref()),
                    |i, result| {
                        slots[i] = Some(result.map_err(|e| fail(&lua, e)));
                        ControlFlow::<()>::Continue(())
                    },
                )
                .await;
                let arr = lua.create_table()?;
                for (i, slot) in slots.into_iter().enumerate() {
                    let entry = lua.create_table()?;
                    match slot.expect("every task settles") {
                        Ok(value) => {
                            entry.set("ok", true)?;
                            entry.set("value", value)?;
                        }
                        Err(Failure::Table(t)) => {
                            entry.set("ok", false)?;
                            entry.set("err", t)?;
                        }
                        Err(Failure::Other(e)) => {
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
    let fail_race = sentinel.clone();
    let race = lua
        .create_async_function(move |lua, tasks: Table| {
            let sem = sem_race.clone();
            let sentinel = fail_race.clone();
            async move {
                let funcs = task_list(&tasks)?;
                let group = state::TaskGroup::new(&lua);
                if funcs.is_empty() {
                    return Err(mlua::Error::RuntimeError("lur.async.race: no tasks".into()));
                }
                let first = drive(
                    funcs,
                    cap,
                    |_, f| run_task(f, sem.clone(), group.as_ref()),
                    |_, result| ControlFlow::Break(result.map_err(|e| fail(&lua, e))),
                )
                .await;
                match first.expect("a non-empty race settles") {
                    Ok(v) => Ok(v),
                    Err(f) => outcome(&sentinel, f),
                }
            }
        })
        .map_err(RunError::Init)?;
    let race = raise::raising_hot(factory, race, &sentinel).map_err(RunError::Init)?;
    async_tbl.set("race", race).map_err(RunError::Init)?;

    // any: the first success wins; raises an aggregate error if all fail.
    let sem_any = sem.clone();
    let any = lua
        .create_async_function(move |lua, tasks: Table| {
            let sem = sem_any.clone();
            async move {
                let funcs = task_list(&tasks)?;
                let group = state::TaskGroup::new(&lua);
                if funcs.is_empty() {
                    return Err(mlua::Error::RuntimeError("lur.async.any: no tasks".into()));
                }
                let mut errors: Vec<String> = Vec::new();
                let won = drive(
                    funcs,
                    cap,
                    |_, f| run_task(f, sem.clone(), group.as_ref()),
                    |_, result| match result {
                        Ok(value) => ControlFlow::Break(value),
                        Err(e) => {
                            // Aggregated as text; still empty the slot.
                            raise::recover(&lua, &e);
                            errors.push(e.to_string());
                            ControlFlow::Continue(())
                        }
                    },
                )
                .await;
                match won {
                    Some(value) => Ok(value),
                    None => Err(mlua::Error::RuntimeError(format!(
                        "lur.async.any: all {} tasks failed: {}",
                        errors.len(),
                        errors.join("; ")
                    ))),
                }
            }
        })
        .map_err(RunError::Init)?;
    async_tbl.set("any", any).map_err(RunError::Init)?;

    // map: fn(item, index) over an array, results in input order. `limit`
    // bounds this call's in-flight tasks; `drop_errors` omits failed items
    // instead of failing fast.
    let sem_map = sem.clone();
    let fail_map = sentinel.clone();
    let map = lua
        .create_async_function(move |lua, (list, func, opts): (Value, Value, Value)| {
            let global = sem_map.clone();
            let sentinel = fail_map.clone();
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
                let window = limit
                    .map_or(usize::MAX, |n| usize::try_from(n).unwrap_or(usize::MAX))
                    .min(cap);
                let mut slots: Vec<Option<Value>> = vec![None; items.len()];
                let group = state::TaskGroup::new(&lua);
                let failed = drive(
                    items,
                    window,
                    |i, item| map_task(func.clone(), item, i + 1, global.clone(), group.as_ref()),
                    |i, result| match result {
                        Ok(v) => {
                            slots[i] = Some(v);
                            ControlFlow::Continue(())
                        }
                        Err(e) if !drop_errors => ControlFlow::Break(fail(&lua, e)),
                        Err(e) => {
                            raise::recover(&lua, &e);
                            ControlFlow::Continue(())
                        }
                    },
                )
                .await;
                if let Some(f) = failed {
                    return outcome(&sentinel, f);
                }
                let arr = lua.create_table()?;
                for (n, v) in slots.into_iter().flatten().enumerate() {
                    arr.set(n + 1, v)?;
                }
                Ok(Value::Table(arr))
            }
        })
        .map_err(RunError::Init)?;
    let map = raise::raising_hot(factory, map, &sentinel).map_err(RunError::Init)?;
    async_tbl.set("map", map).map_err(RunError::Init)?;

    lur.set("async", async_tbl).map_err(RunError::Init)?;
    Ok(())
}
