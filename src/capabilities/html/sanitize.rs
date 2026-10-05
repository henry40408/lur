//! `lur.html.sanitize` — strip scripts, event handlers and unsafe URLs from
//! untrusted HTML with `ammonia`'s allowlist (not hand-rolled on purpose).

use std::collections::HashSet;

use ammonia::{Builder, UrlRelative};
use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, html: &Table) -> Result<(), RunError> {
    let f = lua
        .create_function(|lua, (text, opts): (Value, Option<Table>)| {
            let text: mlua::LuaString = argcheck::arg(lua, text, "lur.html.sanitize", 1, "string")?;
            let text = String::from_utf8_lossy(&text.as_bytes()).into_owned();
            let base = match opts {
                Some(opts) => opts
                    .get::<Option<String>>("base")
                    .map_err(|e| Error::runtime(format!("lur.html.sanitize: base: {e}")))?,
                None => None,
            };
            let mut builder = Builder::default();
            builder.url_schemes(HashSet::from(["http", "https", "mailto"]));
            if let Some(base) = base {
                let base = url::Url::parse(&base).map_err(|e| {
                    Error::runtime(format!("lur.html.sanitize: invalid base {base:?}: {e}"))
                })?;
                builder.url_relative(UrlRelative::RewriteWithBase(base));
            }
            Ok(builder.clean(&text).to_string())
        })
        .map_err(RunError::Init)?;
    html.set("sanitize", f).map_err(RunError::Init)
}
