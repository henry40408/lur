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

use super::{argcheck, raise};
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
) -> mlua::Result<Value> {
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

/// Concurrency limits shared by every combinator.
#[derive(Clone)]
struct Limits {
    /// `max_concurrency` permits, when capped.
    sem: Option<Arc<Semaphore>>,
    /// A call can never run more than `max_concurrency` tasks, so that is also
    /// the most futures worth keeping alive for it.
    cap: usize,
}

/// `max_concurrency` caps concurrently running combinator tasks per VM.
/// `factory` is the VM's [`raise::factory`].
pub fn install(
    lua: &Lua,
    lur: &Table,
    factory: &Function,
    max_concurrency: Option<usize>,
) -> Result<(), RunError> {
    let async_tbl = build(lua, factory, max_concurrency).map_err(RunError::Init)?;
    lur.set("async", async_tbl).map_err(RunError::Init)
}

fn build(lua: &Lua, factory: &Function, max_concurrency: Option<usize>) -> mlua::Result<Table> {
    // Zero would park every task until the timeout; clamp for library callers
    // (the CLI rejects it earlier).
    let max_concurrency = max_concurrency.map(|n| n.max(1));
    let limits = Limits {
        sem: max_concurrency.map(|n| Arc::new(Semaphore::new(n))),
        cap: max_concurrency.unwrap_or(usize::MAX),
    };
    // Arc: cloned into every call, and a `Table` clone is a registry ref.
    let sentinel = Arc::new(raise::Sentinel::new(lua)?);

    let async_tbl = lua.create_table()?;
    async_tbl.set("sleep", sleep_fn(lua)?)?;
    let all = all_fn(lua, &limits, &sentinel)?;
    async_tbl.set("all", raise::raising_hot(factory, all, &sentinel)?)?;
    async_tbl.set("settled", settled_fn(lua, &limits)?)?;
    let race = race_fn(lua, &limits, &sentinel)?;
    async_tbl.set("race", raise::raising_hot(factory, race, &sentinel)?)?;
    async_tbl.set("any", any_fn(lua, &limits)?)?;
    let map = map_fn(lua, &limits, &sentinel)?;
    async_tbl.set("map", raise::raising_hot(factory, map, &sentinel)?)?;
    Ok(async_tbl)
}

/// No Lua runs while parked, so only the wall-clock timeout (not the
/// interrupt) can cut this short.
fn sleep_fn(lua: &Lua) -> mlua::Result<Function> {
    lua.create_async_function(|_, ms: u64| async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(())
    })
}

/// all: results in order; the first error re-raises and cancels the rest.
fn all_fn(lua: &Lua, limits: &Limits, sentinel: &Arc<raise::Sentinel>) -> mlua::Result<Function> {
    let limits = limits.clone();
    let sentinel = Arc::clone(sentinel);
    lua.create_async_function(move |lua, tasks: Table| {
        let sem = limits.sem.clone();
        let cap = limits.cap;
        let sentinel = sentinel.clone();
        async move {
            let funcs = task_list(&tasks)?;
            let mut slots = vec![Value::Nil; funcs.len()];
            let failed = drive(
                funcs,
                cap,
                |_, f| run_task(f, sem.clone()),
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
}

/// settled: never raises; { ok = true, value } / { ok = false, err } per task.
fn settled_fn(lua: &Lua, limits: &Limits) -> mlua::Result<Function> {
    let limits = limits.clone();
    lua.create_async_function(move |lua, tasks: Table| {
        let sem = limits.sem.clone();
        let cap = limits.cap;
        async move {
            let funcs = task_list(&tasks)?;
            let mut slots: Vec<Option<Result<Value, Failure>>> =
                std::iter::repeat_with(|| None).take(funcs.len()).collect();
            drive(
                funcs,
                cap,
                |_, f| run_task(f, sem.clone()),
                |i, result| {
                    slots[i] = Some(result.map_err(|e| fail(&lua, e)));
                    ControlFlow::<()>::Continue(())
                },
            )
            .await;
            let arr = lua.create_table()?;
            for (i, slot) in slots.into_iter().enumerate() {
                let entry = settled_entry(&lua, slot.expect("every task settles"))?;
                arr.set(i + 1, entry)?;
            }
            Ok(arr)
        }
    })
}

fn settled_entry(lua: &Lua, settled: Result<Value, Failure>) -> mlua::Result<Table> {
    let entry = lua.create_table()?;
    match settled {
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
    Ok(entry)
}

/// race: the first task to settle wins, success or failure.
fn race_fn(lua: &Lua, limits: &Limits, sentinel: &Arc<raise::Sentinel>) -> mlua::Result<Function> {
    let limits = limits.clone();
    let sentinel = Arc::clone(sentinel);
    lua.create_async_function(move |lua, tasks: Table| {
        let sem = limits.sem.clone();
        let cap = limits.cap;
        let sentinel = sentinel.clone();
        async move {
            let funcs = task_list(&tasks)?;
            if funcs.is_empty() {
                return Err(mlua::Error::RuntimeError("lur.async.race: no tasks".into()));
            }
            let first = drive(
                funcs,
                cap,
                |_, f| run_task(f, sem.clone()),
                |_, result| ControlFlow::Break(result.map_err(|e| fail(&lua, e))),
            )
            .await;
            match first.expect("a non-empty race settles") {
                Ok(v) => Ok(v),
                Err(f) => outcome(&sentinel, f),
            }
        }
    })
}

/// any: the first success wins; raises an aggregate error if all fail.
fn any_fn(lua: &Lua, limits: &Limits) -> mlua::Result<Function> {
    let limits = limits.clone();
    lua.create_async_function(move |lua, tasks: Table| {
        let sem = limits.sem.clone();
        let cap = limits.cap;
        async move {
            let funcs = task_list(&tasks)?;
            if funcs.is_empty() {
                return Err(mlua::Error::RuntimeError("lur.async.any: no tasks".into()));
            }
            let mut errors: Vec<String> = Vec::new();
            let won = drive(
                funcs,
                cap,
                |_, f| run_task(f, sem.clone()),
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
}

const MAP: &str = "lur.async.map";

/// `map`'s third argument.
struct MapOpts {
    /// Bounds this call's in-flight tasks.
    limit: Option<i64>,
    /// Omit failed items instead of failing fast.
    drop_errors: bool,
}

fn map_opts(opts: Value) -> mlua::Result<MapOpts> {
    let o = match opts {
        Value::Nil => {
            return Ok(MapOpts {
                limit: None,
                drop_errors: false,
            });
        }
        Value::Table(o) => o,
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "{MAP}: argument #3 must be table, got {}",
                other.type_name()
            )));
        }
    };
    let raw: Value = o.get("limit")?;
    let got = raw.type_name();
    let limit = argcheck::integer_arg(raw, MAP, 3).map_err(|_e| {
        mlua::Error::RuntimeError(format!(
            "{MAP}: argument #3 opts.limit must be an integer >= 1, got {got}"
        ))
    })?;
    if let Some(n) = limit
        && n < 1
    {
        return Err(mlua::Error::RuntimeError(format!(
            "{MAP}: argument #3 opts.limit must be >= 1, got {n}"
        )));
    }
    let drop_errors = match o.get::<Value>("drop_errors")? {
        Value::Nil => false,
        Value::Boolean(b) => b,
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "{MAP}: argument #3 opts.drop_errors must be boolean, got {}",
                other.type_name()
            )));
        }
    };
    Ok(MapOpts { limit, drop_errors })
}

/// map: fn(item, index) over an array, results in input order.
fn map_fn(lua: &Lua, limits: &Limits, sentinel: &Arc<raise::Sentinel>) -> mlua::Result<Function> {
    let limits = limits.clone();
    let sentinel = Arc::clone(sentinel);
    lua.create_async_function(move |lua, args: (Value, Value, Value)| {
        let limits = limits.clone();
        let sentinel = sentinel.clone();
        async move { run_map(&lua, &limits, &sentinel, args).await }
    })
}

/// A checked `map` call: the array's items, the mapper and the options.
struct MapCall {
    items: Vec<Value>,
    func: Function,
    opts: MapOpts,
}

fn map_call(lua: &Lua, args: (Value, Value, Value)) -> mlua::Result<MapCall> {
    let (list, func, opts) = args;
    let list: Table = argcheck::arg(
        lua,
        list,
        argcheck::ArgSpec {
            fname: MAP,
            n: 1,
            expected: "table",
        },
    )?;
    let func: Function = argcheck::arg(
        lua,
        func,
        argcheck::ArgSpec {
            fname: MAP,
            n: 2,
            expected: "function",
        },
    )?;
    let opts = map_opts(opts)?;
    let items = array_items(&list)?.ok_or_else(|| {
        mlua::Error::RuntimeError(format!(
            "{MAP}: argument #1 must be an array, got a table with holes or non-array keys"
        ))
    })?;
    Ok(MapCall { items, func, opts })
}

async fn run_map(
    lua: &Lua,
    limits: &Limits,
    sentinel: &raise::Sentinel,
    args: (Value, Value, Value),
) -> mlua::Result<Value> {
    let MapCall {
        items,
        func,
        opts: MapOpts { limit, drop_errors },
    } = map_call(lua, args)?;
    let window = limit
        .map_or(usize::MAX, |n| usize::try_from(n).unwrap_or(usize::MAX))
        .min(limits.cap);
    let mut slots: Vec<Option<Value>> = vec![None; items.len()];
    let failed = drive(
        items,
        window,
        |i, item| map_task(func.clone(), item, i + 1, limits.sem.clone()),
        |i, result| match result {
            Ok(v) => {
                slots[i] = Some(v);
                ControlFlow::Continue(())
            }
            Err(e) if !drop_errors => ControlFlow::Break(fail(lua, e)),
            Err(e) => {
                raise::recover(lua, &e);
                ControlFlow::Continue(())
            }
        },
    )
    .await;
    if let Some(f) = failed {
        return outcome(sentinel, f);
    }
    let arr = lua.create_table()?;
    for (n, v) in slots.into_iter().flatten().enumerate() {
        arr.set(n + 1, v)?;
    }
    Ok(Value::Table(arr))
}
