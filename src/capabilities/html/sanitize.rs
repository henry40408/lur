//! `lur.html.sanitize` — strip scripts, event handlers and unsafe URLs from
//! untrusted HTML with `ammonia`'s allowlist (not hand-rolled on purpose).
//!
//! ## Design notes
//!
//! **`lur.html.sanitize`**: delegated to `ammonia` rather than hand-rolled, since an
//! allowlist sanitizer is security code where the long tail (mXSS, scheme tricks) is the whole
//! job. Cost measured: +275 KB release (ammonia 4.1.4 shares `html5ever` 0.39 with `scraper`,
//! so no second parser). Schemes are narrowed to `http`/`https`/`mailto` (ammonia's default
//! also allows `ftp`, `tel`, `magnet`, …). Not done: custom tag/attribute allowlists and
//! returning a node instead of a string.

use std::collections::HashSet;

use ammonia::{Builder, UrlRelative};
use mlua::{Error, Lua, Table, Value};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

pub fn install(lua: &Lua, html: &Table) -> Result<(), RunError> {
    let f = lua
        .create_function(|lua, (text, opts): (Value, Option<Table>)| {
            let text: mlua::LuaString = argcheck::arg(
                lua,
                text,
                argcheck::ArgSpec {
                    fname: "lur.html.sanitize",
                    n: 1,
                    expected: "string",
                },
            )?;
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
