//! `lur.state` — process-wide primitive KV shared by every pooled VM (spec §6).
//! `update` is optimistic (version-checked retry), so no lock is held across
//! user code.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mlua::{Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// A stored primitive; nil is absence.
#[derive(Debug, Clone, PartialEq)]
enum Prim {
    Bool(bool),
    Num(f64),
    Str(Vec<u8>),
}

/// Value plus per-key version, bumped on every write including deletes, so
/// `update` detects conflicts without comparing values (no ABA).
#[derive(Debug, Clone)]
struct Versioned {
    value: Option<Prim>,
    version: u64,
}

enum IncrError {
    NotInteger,
    Overflow,
}

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

    fn incr(&self, key: Vec<u8>, n: i64) -> Result<i64, IncrError> {
        let mut map = self.lock();
        let base: i64 = match map.get(&key).and_then(|v| v.value.as_ref()) {
            None => 0,
            Some(Prim::Num(x))
                if x.fract() == 0.0 && *x >= i64::MIN as f64 && *x < i64::MAX as f64 =>
            {
                *x as i64
            }
            Some(_) => return Err(IncrError::NotInteger),
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

    fn snapshot(&self, key: &[u8]) -> (Option<Prim>, u64) {
        match self.lock().get(key) {
            Some(v) => (v.value.clone(), v.version),
            None => (None, 0),
        }
    }

    /// Store `value` iff the key's version is still `expected`.
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

    /// Compare-and-set by value (`None` = absent); numbers compare as f64.
    fn cas_value(&self, key: &[u8], expected: Option<&Prim>, new: Option<Prim>) -> bool {
        let (current, version) = self.snapshot(key);
        if current.as_ref() != expected {
            return false;
        }
        self.compare_and_set(key, version, new)
    }
}

thread_local! {
    /// Set during an `update` transform so re-entrant `lur.state` calls error.
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

fn to_lua(lua: &Lua, p: Option<Prim>) -> mlua::Result<Value> {
    Ok(match p {
        None => Value::Nil,
        Some(Prim::Bool(b)) => Value::Boolean(b),
        Some(Prim::Num(n)) => Value::Number(n),
        Some(Prim::Str(s)) => Value::String(lua.create_string(&s)?),
    })
}

/// Lua value → primitive (nil → delete); non-primitives are rejected.
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
                // No host lock is held while the transform runs.
                IN_UPDATE.with(|f| f.set(true));
                let result = func.call::<Value>(old_lua);
                IN_UPDATE.with(|f| f.set(false));
                let new_lua = result?;
                let new = from_lua(&new_lua)?;
                if s.compare_and_set(&key, version, new) {
                    return Ok(new_lua);
                }
                // Lost a race; retry from a fresh snapshot.
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
            Ok(s.cas_value(&key.as_bytes(), expected_prim.as_ref(), new_prim))
        })
        .map_err(RunError::Init)?;
    state.set("cas", cas).map_err(RunError::Init)?;

    let s = store.clone();
    let add = lua
        .create_function(move |lua, (key, value): (Value, Value)| {
            let key: mlua::LuaString = argcheck::arg(lua, key, "lur.state.add", 1, "string")?;
            reject_reentry()?;
            let new_prim = from_lua(&value)?;
            Ok(s.cas_value(&key.as_bytes(), None, new_prim))
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

        assert!(!store.compare_and_set(b"k", 0, Some(Prim::Num(9.0))));
        assert!(store.compare_and_set(b"k", 1, Some(Prim::Num(2.0))));
        assert_eq!(store.snapshot(b"k").1, 2);
    }

    #[test]
    fn delete_keeps_a_bumped_version_to_avoid_aba() {
        let store = StateStore::default();
        store.set(b"k".to_vec(), Some(Prim::Num(5.0)));
        store.set(b"k".to_vec(), None); // delete
        assert!(store.get(b"k").is_none());
        assert!(!store.compare_and_set(b"k", 0, Some(Prim::Num(5.0))));
    }

    #[test]
    fn incr_rejects_two_pow_63_instead_of_saturating() {
        let store = StateStore::default();
        store.set(b"k".to_vec(), Some(Prim::Num(2f64.powi(63))));
        assert!(matches!(
            store.incr(b"k".to_vec(), -1),
            Err(IncrError::NotInteger)
        ));
    }
}
