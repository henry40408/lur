//! `lur.state` — short-term, host-side, cross-VM shared state (spec §6).
//!
//! A process-scoped concurrent KV holding **primitives only** (nil / boolean /
//! number / string-bytes), shared by every VM in the pool. Because many VMs
//! touch it concurrently it offers atomic `incr` and an optimistic,
//! version-stamped `update` (the Clojure-`atom`/`swap!` model) — no host lock is
//! ever held across user code.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mlua::{Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// A stored primitive value (nil is represented by absence).
#[derive(Debug, Clone, PartialEq)]
enum Prim {
    Bool(bool),
    Num(f64),
    Str(Vec<u8>),
}

/// A value plus its monotonic per-key version (bumped on every write, including
/// deletes, so conflict detection never compares values — sidestepping f64
/// equality traps and the ABA problem).
#[derive(Debug, Clone)]
struct Versioned {
    value: Option<Prim>,
    version: u64,
}

/// Why an integer counter operation failed.
enum IncrError {
    NotInteger,
    Overflow,
}

/// The host-side store shared across all VMs in a runtime/pool.
#[derive(Debug, Default)]
pub struct StateStore {
    map: Mutex<HashMap<Vec<u8>, Versioned>>,
}

impl StateStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Vec<u8>, Versioned>> {
        self.map.lock().expect("state mutex poisoned")
    }

    fn get(&self, key: &[u8]) -> Option<Prim> {
        self.lock().get(key).and_then(|v| v.value.clone())
    }

    fn set(&self, key: Vec<u8>, value: Option<Prim>) {
        let mut map = self.lock();
        let version = map.get(&key).map_or(0, |v| v.version) + 1;
        map.insert(key, Versioned { value, version });
    }

    /// Atomic integer `+n` fast path. Errors if the existing value is not a
    /// whole number, or on i64 overflow.
    fn incr(&self, key: Vec<u8>, n: i64) -> Result<i64, IncrError> {
        let mut map = self.lock();
        let base: i64 = match map.get(&key).and_then(|v| v.value.as_ref()) {
            None => 0,
            Some(Prim::Num(x))
                if x.fract() == 0.0 && *x >= i64::MIN as f64 && *x <= i64::MAX as f64 =>
            {
                *x as i64
            }
            Some(_) => return Err(IncrError::NotInteger),
            // Some(Prim::Num(non-whole)) also falls through to NotInteger.
        };
        let new = base.checked_add(n).ok_or(IncrError::Overflow)?;
        let version = map.get(&key).map_or(0, |v| v.version) + 1;
        map.insert(
            key,
            Versioned {
                value: Some(Prim::Num(new as f64)),
                version,
            },
        );
        Ok(new)
    }

    /// Snapshot `(value, version)` under a brief lock for the optimistic loop.
    fn snapshot(&self, key: &[u8]) -> (Option<Prim>, u64) {
        match self.lock().get(key) {
            Some(v) => (v.value.clone(), v.version),
            None => (None, 0),
        }
    }

    /// Store `value` iff the key's version is still `expected`; returns whether
    /// it applied (else the caller retries from a fresh snapshot).
    fn compare_and_set(&self, key: &[u8], expected: u64, value: Option<Prim>) -> bool {
        let mut map = self.lock();
        let current = map.get(key).map_or(0, |v| v.version);
        if current != expected {
            return false;
        }
        map.insert(
            key.to_vec(),
            Versioned {
                value,
                version: current + 1,
            },
        );
        true
    }

    /// Value-based compare-and-set: succeeds iff the current value equals
    /// `expected` (by value, not version). Avoids float-equality traps for
    /// strings and booleans; the caller accepts those semantics for numbers.
    /// `None` means "absent". Returns true if the swap was applied.
    #[allow(
        clippy::ref_option,
        reason = "trait method; `&Option<Prim>` matches the CAS call convention across all state backends"
    )]
    fn cas_value(&self, key: &[u8], expected: &Option<Prim>, new: Option<Prim>) -> bool {
        let (current, version) = self.snapshot(key);
        if &current != expected {
            return false;
        }
        self.compare_and_set(key, version, new)
    }
}

thread_local! {
    /// Set while an `update` transform runs, so a re-entrant `lur.state` call on
    /// the same call stack raises a clear error instead of deadlocking.
    static IN_UPDATE: Cell<bool> = const { Cell::new(false) };
}

fn reject_reentry() -> mlua::Result<()> {
    if IN_UPDATE.with(Cell::get) {
        return Err(mlua::Error::RuntimeError(
            "lur.state cannot be re-entered from inside update()".into(),
        ));
    }
    Ok(())
}

/// Convert a stored primitive (or absence) into a Lua value.
fn to_lua(lua: &Lua, p: Option<Prim>) -> mlua::Result<Value> {
    Ok(match p {
        None => Value::Nil,
        Some(Prim::Bool(b)) => Value::Boolean(b),
        Some(Prim::Num(n)) => Value::Number(n),
        Some(Prim::Str(s)) => Value::String(lua.create_string(&s)?),
    })
}

/// Convert a Lua value into a storable primitive (nil → delete). Tables,
/// functions, and other non-primitives are rejected (spec §6).
fn from_lua(value: &Value) -> mlua::Result<Option<Prim>> {
    Ok(match value {
        Value::Nil => None,
        Value::Boolean(b) => Some(Prim::Bool(*b)),
        Value::Integer(i) => Some(Prim::Num(*i as f64)),
        Value::Number(n) => Some(Prim::Num(*n)),
        Value::String(s) => Some(Prim::Str(s.as_bytes().to_vec())),
        _ => {
            return Err(mlua::Error::RuntimeError(
                "lur.state stores only nil/boolean/number/string (lur.json.encode tables yourself)"
                    .into(),
            ));
        }
    })
}

/// Install `lur.state` backed by the shared `store`.
pub fn install(lua: &Lua, lur: &Table, store: Arc<StateStore>) -> Result<(), RunError> {
    let state = lua.create_table().map_err(RunError::Init)?;

    let s = store.clone();
    let get = lua
        .create_function(move |lua, key: Value| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.get", 1, "string")?;
            reject_reentry()?;
            to_lua(lua, s.get(&key.as_bytes()))
        })
        .map_err(RunError::Init)?;
    state.set("get", get).map_err(RunError::Init)?;

    let s = store.clone();
    let set = lua
        .create_function(move |lua, (key, value): (Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.set", 1, "string")?;
            reject_reentry()?;
            s.set(key.as_bytes().to_vec(), from_lua(&value)?);
            Ok(())
        })
        .map_err(RunError::Init)?;
    state.set("set", set).map_err(RunError::Init)?;

    let s = store.clone();
    let incr = lua
        .create_function(move |lua, (key, n): (Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.incr", 1, "string")?;
            let n = argcheck::integer_arg(n, "lur.state.incr", 2)?;
            reject_reentry()?;
            s.incr(key.as_bytes().to_vec(), n.unwrap_or(1))
                .map_err(|e| match e {
                    IncrError::NotInteger => mlua::Error::RuntimeError(
                        "lur.state.incr: existing value is not an integer".into(),
                    ),
                    IncrError::Overflow => {
                        mlua::Error::RuntimeError("lur.state.incr: counter overflow".into())
                    }
                })
        })
        .map_err(RunError::Init)?;
    state.set("incr", incr).map_err(RunError::Init)?;

    let s = store.clone();
    let decr = lua
        .create_function(move |lua, (key, n): (Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.decr", 1, "string")?;
            let n = argcheck::integer_arg(n, "lur.state.decr", 2)?;
            reject_reentry()?;
            let delta = n.unwrap_or(1).checked_neg().ok_or_else(|| {
                mlua::Error::RuntimeError("lur.state.decr: step too large".into())
            })?;
            s.incr(key.as_bytes().to_vec(), delta).map_err(|e| match e {
                IncrError::NotInteger => mlua::Error::RuntimeError(
                    "lur.state.decr: existing value is not an integer".into(),
                ),
                IncrError::Overflow => {
                    mlua::Error::RuntimeError("lur.state.decr: counter overflow".into())
                }
            })
        })
        .map_err(RunError::Init)?;
    state.set("decr", decr).map_err(RunError::Init)?;

    let s = store.clone();
    let update = lua
        .create_function(move |lua, (key, func): (Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.update", 1, "string")?;
            let func: mlua::Function = argcheck::arg(lua, func, "lur.state.update", 2, "function")?;
            reject_reentry()?;
            let key = key.as_bytes().to_vec();
            loop {
                let (old, version) = s.snapshot(&key);
                let old_lua = to_lua(lua, old)?;
                // The transform runs with NO host lock held; the guard makes a
                // re-entrant lur.state call error rather than deadlock.
                IN_UPDATE.with(|f| f.set(true));
                let result = func.call::<Value>(old_lua);
                IN_UPDATE.with(|f| f.set(false));
                let new_lua = result?;
                let new = from_lua(&new_lua)?;
                if s.compare_and_set(&key, version, new) {
                    return Ok(new_lua);
                }
                // version moved under contention → retry from a fresh snapshot.
            }
        })
        .map_err(RunError::Init)?;
    state.set("update", update).map_err(RunError::Init)?;

    let s = store.clone();
    let cas = lua
        .create_function(move |lua, (key, expected, new): (Value, Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.cas", 1, "string")?;
            reject_reentry()?;
            let expected_prim = from_lua(&expected)?;
            let new_prim = from_lua(&new)?;
            Ok(s.cas_value(&key.as_bytes(), &expected_prim, new_prim))
        })
        .map_err(RunError::Init)?;
    state.set("cas", cas).map_err(RunError::Init)?;

    let s = store.clone();
    let add = lua
        .create_function(move |lua, (key, value): (Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.add", 1, "string")?;
            reject_reentry()?;
            let new_prim = from_lua(&value)?;
            Ok(s.cas_value(&key.as_bytes(), &None, new_prim))
        })
        .map_err(RunError::Init)?;
    state.set("add", add).map_err(RunError::Init)?;

    lur.set("state", state).map_err(RunError::Init)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_and_set_respects_versions() {
        let store = StateStore::default();
        let (_, v0) = store.snapshot(b"k");
        assert_eq!(v0, 0, "absent key starts at version 0");

        assert!(store.compare_and_set(b"k", 0, Some(Prim::Num(1.0))));
        let (_, v1) = store.snapshot(b"k");
        assert_eq!(v1, 1);

        // A stale expected-version is rejected (someone else wrote).
        assert!(!store.compare_and_set(b"k", 0, Some(Prim::Num(9.0))));
        // The current version applies and bumps.
        assert!(store.compare_and_set(b"k", 1, Some(Prim::Num(2.0))));
        assert_eq!(store.snapshot(b"k").1, 2);
    }

    #[test]
    fn delete_keeps_a_bumped_version_to_avoid_aba() {
        let store = StateStore::default();
        store.set(b"k".to_vec(), Some(Prim::Num(5.0)));
        store.set(b"k".to_vec(), None); // delete
        assert!(store.get(b"k").is_none());
        // Version persisted past the delete, so a CAS against version 0 fails.
        assert!(!store.compare_and_set(b"k", 0, Some(Prim::Num(5.0))));
    }
}
