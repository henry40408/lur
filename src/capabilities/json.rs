//! `lur.json` — JSON encode/decode. The one place lur requires UTF-8
//! (base64 binary first). `lur.null` ↔ JSON `null`; `nil` means absent.
//!
//! ## Design notes
//!
//! **`null = "drop"` removes array elements and compacts** rather than leaving holes: a Lua
//! table with holes has an undefined `#`, so `{1, nil, 2}` would break `ipairs`/`#`. The
//! catch is that indices shift; use the default `"keep"` when positions matter. A bare
//! top-level `null` stays `lur.null` (no container to drop it from). Unknown values raise.
//! Compaction can turn `[null]` into an empty table, which `lur.json.encode` writes as `{}`.

use mlua::{Error, Lua, Table, Value};
use serde_json::Value as Json;

use super::null;
use crate::capabilities::argcheck;
use crate::runtime::RunError;

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
        .create_function(|lua, (text, opts): (Value, Value)| {
            let text: mlua::LuaString = argcheck::arg(
                lua,
                text,
                argcheck::ArgSpec {
                    fname: "lur.json.decode",
                    n: 1,
                    expected: "string",
                },
            )?;
            let drop_null = parse_decode_opts(lua, opts, "lur.json.decode", 2)?;
            let parsed: Json = serde_json::from_slice(&text.as_bytes())
                .map_err(|e| Error::runtime(e.to_string()))?;
            json_to_lua(lua, &parsed, drop_null)
        })
        .map_err(RunError::Init)?;
    json.set("decode", decode).map_err(RunError::Init)?;

    lur.set("json", json).map_err(RunError::Init)?;
    Ok(())
}

/// `opts.null`: `"keep"` (default, JSON `null` becomes `lur.null`) or `"drop"`.
/// Returns whether nulls are dropped. Shared by `lur.json.decode` and `res.json`.
pub(crate) fn parse_decode_opts(
    lua: &Lua,
    opts: Value,
    fname: &str,
    pos: usize,
) -> mlua::Result<bool> {
    let Some(opts) = argcheck::arg::<Option<Table>>(
        lua,
        opts,
        argcheck::ArgSpec {
            fname,
            n: pos,
            expected: "table",
        },
    )?
    else {
        return Ok(false);
    };
    match opts.get::<Value>("null")? {
        Value::Nil => Ok(false),
        Value::String(s) => match &*s.as_bytes() {
            b"keep" => Ok(false),
            b"drop" => Ok(true),
            _ => Err(Error::runtime(format!(
                "{fname}: opts.null must be \"keep\" or \"drop\""
            ))),
        },
        other => Err(Error::runtime(format!(
            "{fname}: opts.null must be \"keep\" or \"drop\", got {}",
            other.type_name()
        ))),
    }
}

/// With `drop_null`, object fields and array elements that are `null` are
/// omitted (arrays compact); a top-level `null` has no container, so it stays.
pub(crate) fn json_to_lua(lua: &Lua, value: &Json, drop_null: bool) -> mlua::Result<Value> {
    match value {
        Json::Null => null::value(lua),
        Json::Bool(b) => Ok(Value::Boolean(*b)),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Value::Integer(i))
            } else {
                // Beyond i64 or fractional.
                Ok(Value::Number(n.as_f64().expect("json number is f64")))
            }
        }
        Json::String(s) => Ok(Value::String(lua.create_string(s)?)),
        Json::Array(items) => {
            let t = lua.create_table()?;
            let mut n = 0i64;
            for item in items {
                if drop_null && item.is_null() {
                    continue;
                }
                n += 1;
                t.raw_set(n, json_to_lua(lua, item, drop_null)?)?;
            }
            Ok(Value::Table(t))
        }
        Json::Object(fields) => {
            let t = lua.create_table()?;
            for (k, v) in fields {
                if drop_null && v.is_null() {
                    continue;
                }
                t.raw_set(lua.create_string(k)?, json_to_lua(lua, v, drop_null)?)?;
            }
            Ok(Value::Table(t))
        }
    }
}

/// Nesting cap for encoding. A cyclic table would otherwise recurse until the
/// native stack overflows and kills the process, so a cycle simply hits this.
const MAX_ENCODE_DEPTH: usize = 128;

pub(crate) fn lua_to_json(value: &Value) -> mlua::Result<Json> {
    encode_value(value, 0)
}

fn encode_value(value: &Value, depth: usize) -> mlua::Result<Json> {
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
        Value::Table(t) => {
            if depth >= MAX_ENCODE_DEPTH {
                return Err(Error::runtime(format!(
                    "lur.json.encode: value is cyclic or nested deeper than {MAX_ENCODE_DEPTH} levels"
                )));
            }
            table_to_json(t, depth + 1)
        }
        other => Err(Error::runtime(format!(
            "lur.json.encode: cannot encode a {} value",
            other.type_name()
        ))),
    }
}

/// Whole-number floats encode as integers (`1`, not `1.0`).
fn number_to_json(f: f64) -> mlua::Result<Json> {
    if !f.is_finite() {
        return Err(Error::runtime(
            "lur.json.encode: cannot encode NaN or infinity",
        ));
    }
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64 {
        return Ok(Json::Number((f as i64).into()));
    }
    serde_json::Number::from_f64(f)
        .map(Json::Number)
        .ok_or_else(|| Error::runtime("lur.json.encode: number is not representable in JSON"))
}

/// Array iff the keys are exactly `1..#t`; otherwise (and when empty) an object.
fn table_to_json(t: &Table, depth: usize) -> mlua::Result<Json> {
    let len = t.raw_len();
    let mut count = 0usize;
    let mut in_sequence = true;
    for pair in t.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        count += 1;
        // Distinct keys, all in `1..=len`, and `count == len` ⇒ exactly `1..#t`.
        // A bare count check is fooled by a hole plus an out-of-range key.
        let idx = match k {
            Value::Integer(i) => Some(i as f64),
            Value::Number(n) => Some(n),
            _ => None,
        };
        if !idx.is_some_and(|n| n.fract() == 0.0 && n >= 1.0 && n <= len as f64) {
            in_sequence = false;
        }
    }

    if len > 0 && in_sequence && count == len {
        let mut arr = Vec::with_capacity(len);
        for i in 1..=len {
            arr.push(encode_value(&t.raw_get::<Value>(i as i64)?, depth)?);
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
        map.insert(key, encode_value(&v, depth)?);
    }
    Ok(Json::Object(map))
}
