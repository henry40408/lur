//! `lur.json` — JSON encode/decode (spec §4).
//!
//! JSON is the one place lur assumes UTF-8: `encode` requires string values to
//! be valid UTF-8 and errors otherwise (binary must be base64-encoded first).
//! `lur.null` ↔ JSON `null`; a Lua `nil` means "absent".

use mlua::{Error, Lua, Table, Value};
use serde_json::Value as Json;

use super::null;
use crate::runtime::RunError;

/// Install `lur.json.encode` / `lur.json.decode`.
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let json = lua.create_table().map_err(RunError::Init)?;

    let encode = lua
        .create_function(|_, value: Value| {
            let json = lua_to_json(&value)?;
            serde_json::to_string(&json).map_err(|e| Error::runtime(e.to_string()))
        })
        .map_err(RunError::Init)?;
    json.set("encode", encode).map_err(RunError::Init)?;

    let decode = lua
        .create_function(|lua, text: mlua::String| {
            let parsed: Json = serde_json::from_slice(&text.as_bytes())
                .map_err(|e| Error::runtime(e.to_string()))?;
            json_to_lua(lua, &parsed)
        })
        .map_err(RunError::Init)?;
    json.set("decode", decode).map_err(RunError::Init)?;

    lur.set("json", json).map_err(RunError::Init)?;
    Ok(())
}

/// Convert a `serde_json::Value` to a Lua value (JSON `null` → `lur.null`).
pub(crate) fn json_to_lua(lua: &Lua, value: &Json) -> mlua::Result<Value> {
    match value {
        Json::Null => null::value(lua),
        Json::Bool(b) => Ok(Value::Boolean(*b)),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else {
                // u64 above i64::MAX or a non-integral number — use f64.
                Ok(Value::Number(n.as_f64().expect("json number is f64")))
            }
        }
        Json::String(s) => Ok(Value::String(lua.create_string(s)?)),
        Json::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.raw_set(i as i64 + 1, json_to_lua(lua, item)?)?;
            }
            Ok(Value::Table(t))
        }
        Json::Object(fields) => {
            let t = lua.create_table()?;
            for (k, v) in fields {
                t.raw_set(lua.create_string(k)?, json_to_lua(lua, v)?)?;
            }
            Ok(Value::Table(t))
        }
    }
}

/// Convert a Lua value to a `serde_json::Value`.
pub(crate) fn lua_to_json(value: &Value) -> mlua::Result<Json> {
    match value {
        Value::Nil => Ok(Json::Null),
        Value::Boolean(b) => Ok(Json::Bool(*b)),
        Value::Integer(i) => Ok(Json::Number((*i).into())),
        Value::Number(f) => number_to_json(*f),
        Value::String(s) => match std::str::from_utf8(&s.as_bytes()) {
            Ok(text) => Ok(Json::String(text.to_owned())),
            Err(_) => Err(Error::runtime(
                "lur.json.encode: string is not valid UTF-8 (base64-encode binary first)",
            )),
        },
        Value::UserData(_) if null::is_null(value) => Ok(Json::Null),
        Value::Table(t) => table_to_json(t),
        other => Err(Error::runtime(format!(
            "lur.json.encode: cannot encode a {} value",
            other.type_name()
        ))),
    }
}

/// JSON has a single number type. Represent an integral `f64` as an integer
/// (Luau numbers are all `f64`, so `1` arrives as `1.0` and must encode as `1`).
fn number_to_json(f: f64) -> mlua::Result<Json> {
    if !f.is_finite() {
        return Err(Error::runtime(
            "lur.json.encode: cannot encode NaN or infinity",
        ));
    }
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        return Ok(Json::Number((f as i64).into()));
    }
    serde_json::Number::from_f64(f)
        .map(Json::Number)
        .ok_or_else(|| Error::runtime("lur.json.encode: number is not representable in JSON"))
}

/// A Lua table becomes a JSON array when its keys are exactly `1..#t`, and a
/// JSON object otherwise. An empty table encodes as `{}`.
fn table_to_json(t: &Table) -> mlua::Result<Json> {
    let len = t.raw_len();
    let mut count = 0usize;
    let mut all_int_keys = true;
    for pair in t.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        count += 1;
        if !matches!(k, Value::Integer(_) | Value::Number(_)) {
            all_int_keys = false;
        }
    }

    if len > 0 && all_int_keys && count == len {
        let mut arr = Vec::with_capacity(len);
        for i in 1..=len {
            arr.push(lua_to_json(&t.raw_get::<Value>(i as i64)?)?);
        }
        return Ok(Json::Array(arr));
    }

    let mut map = serde_json::Map::with_capacity(count);
    for pair in t.clone().pairs::<Value, Value>() {
        let (k, v) = pair?;
        let key = match &k {
            Value::String(s) => match std::str::from_utf8(&s.as_bytes()) {
                Ok(text) => text.to_owned(),
                Err(_) => return Err(Error::runtime("lur.json.encode: object key is not UTF-8")),
            },
            _ => {
                return Err(Error::runtime(
                    "lur.json.encode: object keys must be strings",
                ));
            }
        };
        map.insert(key, lua_to_json(&v)?);
    }
    Ok(Json::Object(map))
}
