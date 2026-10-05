//! `lur.url` — parse, resolve and build URLs (WHATWG, via the `url` crate).
//! Pure compute: nothing here touches the network, so it is not policy-gated.

use std::collections::BTreeMap;

use mlua::{Error, Lua, Table, Value};
use url::Url;
use url::form_urlencoded;

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let url = lua.create_table().map_err(RunError::Init)?;
    install_parse(lua, &url)?;
    install_join(lua, &url)?;
    install_encode_query(lua, &url)?;
    install_decode_query(lua, &url)?;
    lur.set("url", url).map_err(RunError::Init)?;
    Ok(())
}

fn str_arg(lua: &Lua, value: Value, fname: &str, n: usize) -> mlua::Result<String> {
    let s: mlua::LuaString = argcheck::arg(lua, value, fname, n, "string")?;
    s.to_str()
        .map(|s| s.to_string())
        .map_err(|e| Error::runtime(format!("{fname}: {e}")))
}

fn install_parse(lua: &Lua, url: &Table) -> Result<(), RunError> {
    let parse = lua
        .create_function(|lua, text: Value| {
            let text = str_arg(lua, text, "lur.url.parse", 1)?;
            let u = Url::parse(&text)
                .map_err(|e| Error::runtime(format!("lur.url.parse: {e}: '{text}'")))?;
            let out = lua.create_table()?;
            out.set("href", u.as_str())?;
            out.set("scheme", u.scheme())?;
            if !u.username().is_empty() {
                out.set("username", u.username())?;
            }
            if let Some(p) = u.password() {
                out.set("password", p)?;
            }
            if let Some(h) = u.host_str() {
                out.set("host", h)?;
            }
            // Explicit, non-default port only (`https://x:443/` has none).
            if let Some(p) = u.port() {
                out.set("port", i64::from(p))?;
            }
            out.set("path", u.path())?;
            if let Some(q) = u.query() {
                out.set("query", q)?;
            }
            if let Some(f) = u.fragment() {
                out.set("fragment", f)?;
            }
            Ok(out)
        })
        .map_err(RunError::Init)?;
    url.set("parse", parse).map_err(RunError::Init)?;
    Ok(())
}

fn install_join(lua: &Lua, url: &Table) -> Result<(), RunError> {
    let join = lua
        .create_function(|lua, (base, rel): (Value, Value)| {
            let base = str_arg(lua, base, "lur.url.join", 1)?;
            let rel = str_arg(lua, rel, "lur.url.join", 2)?;
            let base = Url::parse(&base)
                .map_err(|e| Error::runtime(format!("lur.url.join: base: {e}: '{base}'")))?;
            let joined = base
                .join(&rel)
                .map_err(|e| Error::runtime(format!("lur.url.join: {e}: '{rel}'")))?;
            Ok(joined.to_string())
        })
        .map_err(RunError::Init)?;
    url.set("join", join).map_err(RunError::Init)?;
    Ok(())
}

/// A query value: string, number or boolean.
fn scalar(value: &Value, key: &str) -> mlua::Result<String> {
    match value {
        Value::String(s) => s
            .to_str()
            .map(|s| s.to_string())
            .map_err(|e| Error::runtime(format!("lur.url.encode_query: '{key}': {e}"))),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Boolean(b) => Ok(b.to_string()),
        other => Err(Error::runtime(format!(
            "lur.url.encode_query: '{key}' must be string, number, boolean or an array of those, got {}",
            other.type_name()
        ))),
    }
}

/// `{ a = "1", b = { "x", "y" } }` → `a=1&b=x&b=y`. Keys are sorted so the output is
/// deterministic; arrays repeat the key; `application/x-www-form-urlencoded` escaping.
fn install_encode_query(lua: &Lua, url: &Table) -> Result<(), RunError> {
    let encode = lua
        .create_function(|lua, params: Value| {
            let params: Table = argcheck::arg(lua, params, "lur.url.encode_query", 1, "table")?;
            let mut pairs: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for entry in params.pairs::<Value, Value>() {
                let (k, v) = entry?;
                let Value::String(k) = k else {
                    return Err(Error::runtime("lur.url.encode_query: keys must be strings"));
                };
                let k = k
                    .to_str()
                    .map_err(|e| Error::runtime(format!("lur.url.encode_query: key: {e}")))?
                    .to_string();
                let values = match &v {
                    Value::Table(list) => list
                        .sequence_values::<Value>()
                        .map(|item| scalar(&item?, &k))
                        .collect::<mlua::Result<Vec<_>>>()?,
                    other => vec![scalar(other, &k)?],
                };
                pairs.insert(k, values);
            }
            let mut ser = form_urlencoded::Serializer::new(String::new());
            for (k, values) in &pairs {
                for v in values {
                    ser.append_pair(k, v);
                }
            }
            Ok(ser.finish())
        })
        .map_err(RunError::Init)?;
    url.set("encode_query", encode).map_err(RunError::Init)?;
    Ok(())
}

/// `"?a=1&b=2"` (leading `?` optional) → `{ a = "1", b = "2" }`; the last duplicate
/// wins, like `req.query`.
fn install_decode_query(lua: &Lua, url: &Table) -> Result<(), RunError> {
    let decode = lua
        .create_function(|lua, text: Value| {
            let text = str_arg(lua, text, "lur.url.decode_query", 1)?;
            let text = text.strip_prefix('?').unwrap_or(&text);
            let out = lua.create_table()?;
            for (k, v) in form_urlencoded::parse(text.as_bytes()) {
                out.set(k.as_ref(), v.as_ref())?;
            }
            Ok(out)
        })
        .map_err(RunError::Init)?;
    url.set("decode_query", decode).map_err(RunError::Init)?;
    Ok(())
}
