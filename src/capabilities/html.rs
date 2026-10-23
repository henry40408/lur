//! `lur.html` — parse HTML and query it with CSS selectors.
//!
//! `scraper::Html` is `!Send` (tendril uses non-atomic refcounts) but mlua's
//! `send` mode needs `Send` userdata. A [`Node`] therefore holds only the source
//! text and an `ego_tree::NodeId`; the parsed tree lives in a small per-thread
//! LRU and is re-parsed on a miss. Parsing is deterministic, so a `NodeId`
//! stays valid across re-parses — the cache affects speed, never results.
//!
//! ## Design notes
//!
//! - `scraper::Html` (and `dom_query`) are `!Send` because tendril uses non-atomic
//!   refcounts, while mlua's `send` mode needs `Send` userdata. A node therefore stores the
//!   source text plus an `ego_tree::NodeId`; parsed trees live in an 8-slot per-thread LRU
//!   and are re-parsed on a miss. Parsing is deterministic, so ids stay valid and the cache
//!   only affects speed. Known cost: holding many live docs on one thread re-parses on access.
//! - Rejected: `unsafe impl Send` (denied by lint, unsound); eager conversion to Lua tables
//!   (loses `select` on sub-nodes; fragment re-parse drops `<td>`).
//! - Invalid UTF-8 input is replaced rather than rejected, since it is usually a raw HTTP
//!   body.
//! - `node:text({ trim = true })` is opt-in so existing output is unchanged; it trims Unicode
//!   whitespace at both ends only (inner whitespace is untouched). `lur.xml` nodes do not
//!   take it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ego_tree::NodeId;
use mlua::{Error, Lua, Table, UserData, UserDataMethods, Value, Variadic};
use scraper::{ElementRef, Html, Selector};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

mod sanitize;

/// Parsed documents kept per thread.
const CACHE_SLOTS: usize = 8;

static NEXT_DOC_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CACHE: RefCell<Vec<(u64, Rc<Html>)>> = const { RefCell::new(Vec::new()) };
}

/// One parsed input; shared by every [`Node`] derived from it.
struct Source {
    id: u64,
    text: Box<str>,
}

impl Source {
    fn tree(&self) -> Rc<Html> {
        CACHE.with_borrow_mut(|cache| {
            if let Some(pos) = cache.iter().position(|(id, _)| *id == self.id) {
                let entry = cache.remove(pos);
                let html = Rc::clone(&entry.1);
                cache.insert(0, entry);
                return html;
            }
            let html = Rc::new(Html::parse_document(&self.text));
            cache.insert(0, (self.id, Rc::clone(&html)));
            cache.truncate(CACHE_SLOTS);
            html
        })
    }
}

/// A document root or an element.
#[derive(Clone)]
pub(crate) struct Node {
    source: Arc<Source>,
    id: NodeId,
}

impl Node {
    fn derive(&self, id: NodeId) -> Self {
        Self {
            source: Arc::clone(&self.source),
            id,
        }
    }

    /// Run `f` with the element this node points at (`None` for the document root).
    fn with_element<R>(
        &self,
        f: impl FnOnce(&Html, Option<ElementRef<'_>>) -> R,
    ) -> mlua::Result<R> {
        let html = self.source.tree();
        let node = html
            .tree
            .get(self.id)
            .ok_or_else(|| Error::runtime("lur.html: stale node"))?;
        Ok(f(&html, ElementRef::wrap(node)))
    }

    fn select(&self, css: &str, first_only: bool) -> mlua::Result<Vec<Node>> {
        let selector = Selector::parse(css)
            .map_err(|e| Error::runtime(format!("lur.html: invalid selector '{css}': {e}")))?;
        self.with_element(|html, el| {
            let ids = |iter: &mut dyn Iterator<Item = ElementRef<'_>>| -> Vec<Node> {
                let found = iter.map(|e| self.derive(e.id()));
                if first_only {
                    found.take(1).collect()
                } else {
                    found.collect()
                }
            };
            match el {
                Some(el) => ids(&mut el.select(&selector)),
                None => ids(&mut html.select(&selector)),
            }
        })
    }
}

impl Node {
    /// `node:text()`, `node:text({ trim = true })` or `node:text(css)`.
    fn text(&self, lua: &Lua, args: Variadic<Value>) -> mlua::Result<Option<String>> {
        if args.len() > 1 {
            return Err(Error::runtime(format!(
                "lur.html.text: expected at most 1 argument, got {}",
                args.len()
            )));
        }
        let opts = args.into_iter().next().unwrap_or(Value::Nil);
        // `text(css)`: shorthand for `select_one(css)` then `:text()`;
        // `nil` when nothing matches, `""` when the match has no text.
        if matches!(opts, Value::String(_)) {
            let css = css_arg(lua, opts, "lur.html.text")?;
            let Some(found) = self.select(&css, true)?.into_iter().next() else {
                return Ok(None);
            };
            return found
                .with_element(|html, el| {
                    el.unwrap_or_else(|| html.root_element())
                        .text()
                        .collect::<String>()
                })
                .map(Some);
        }
        let trim = text_opts(lua, opts)?;
        self.with_element(|html, el| {
            let el = el.unwrap_or_else(|| html.root_element());
            let text = el.text().collect::<String>();
            if trim { text.trim().to_owned() } else { text }
        })
        .map(Some)
    }

    fn attr(&self, lua: &Lua, name: Value) -> mlua::Result<Option<String>> {
        let name: mlua::LuaString = argcheck::arg(
            lua,
            name,
            argcheck::ArgSpec {
                fname: "lur.html.attr",
                n: 1,
                expected: "string",
            },
        )?;
        let name = name
            .to_str()
            .map_err(|e| Error::runtime(format!("lur.html.attr: {e}")))?;
        self.with_element(|_, el| el.and_then(|e| e.value().attr(&name).map(str::to_owned)))
    }

    fn attrs(&self, lua: &Lua) -> mlua::Result<Table> {
        let pairs: Vec<(String, String)> = self.with_element(|_, el| {
            el.map(|e| {
                e.value()
                    .attrs()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect()
            })
            .unwrap_or_default()
        })?;
        let out = lua.create_table()?;
        for (k, v) in pairs {
            out.set(k, v)?;
        }
        Ok(out)
    }
}

impl UserData for Node {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("select", |lua, this, css: Value| {
            let css = css_arg(lua, css, "lur.html.select")?;
            this.select(&css, false)
        });
        methods.add_method("select_one", |lua, this, css: Value| {
            let css = css_arg(lua, css, "lur.html.select_one")?;
            Ok(this.select(&css, true)?.into_iter().next())
        });
        methods.add_method("text", |lua, this, args: Variadic<Value>| {
            this.text(lua, args)
        });
        methods.add_method("html", |_, this, ()| {
            this.with_element(|html, el| el.map_or_else(|| html.html(), |e| e.html()))
        });
        methods.add_method("inner_html", |_, this, ()| {
            this.with_element(|html, el| el.unwrap_or_else(|| html.root_element()).inner_html())
        });
        methods.add_method("tag", |_, this, ()| {
            this.with_element(|_, el| el.map(|e| e.value().name().to_owned()))
        });
        methods.add_method("attr", |lua, this, name: Value| this.attr(lua, name));
        methods.add_method("attrs", |lua, this, ()| this.attrs(lua));
        methods.add_method("parent", |_, this, ()| {
            this.with_element(|_, el| {
                el.and_then(|e| e.parent())
                    .and_then(ElementRef::wrap)
                    .map(|p| this.derive(p.id()))
            })
        });
        methods.add_method("children", |_, this, ()| {
            this.with_element(|html, el| {
                let el = el.unwrap_or_else(|| html.root_element());
                el.child_elements()
                    .map(|c| this.derive(c.id()))
                    .collect::<Vec<_>>()
            })
        });
    }
}

/// `node:text({ trim = true })`; returns whether to trim.
fn text_opts(lua: &Lua, opts: Value) -> mlua::Result<bool> {
    let Some(opts) = argcheck::arg::<Option<Table>>(
        lua,
        opts,
        argcheck::ArgSpec {
            fname: "lur.html.text",
            n: 1,
            expected: "string or table",
        },
    )?
    else {
        return Ok(false);
    };
    match opts.get::<Value>("trim")? {
        Value::Nil => Ok(false),
        Value::Boolean(b) => Ok(b),
        other => Err(Error::runtime(format!(
            "lur.html.text: opts.trim must be boolean, got {}",
            other.type_name()
        ))),
    }
}

fn css_arg(lua: &Lua, value: Value, fname: &str) -> mlua::Result<String> {
    let s: mlua::LuaString = argcheck::arg(
        lua,
        value,
        argcheck::ArgSpec {
            fname,
            n: 1,
            expected: "string",
        },
    )?;
    s.to_str()
        .map(|s| s.to_string())
        .map_err(|e| Error::runtime(format!("{fname}: {e}")))
}

/// The document node `lur.html.parse` returns; also used by `res.html()`.
pub(crate) fn parse_document(text: String) -> Node {
    let source = Arc::new(Source {
        id: NEXT_DOC_ID.fetch_add(1, Ordering::Relaxed),
        text: text.into_boxed_str(),
    });
    let root = source.tree().tree.root().id();
    Node { source, id: root }
}

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let html = lua.create_table().map_err(RunError::Init)?;

    // Input is often a raw HTTP body, so invalid UTF-8 is replaced, not rejected.
    let parse = lua
        .create_function(|lua, text: Value| {
            let text: mlua::LuaString = argcheck::arg(
                lua,
                text,
                argcheck::ArgSpec {
                    fname: "lur.html.parse",
                    n: 1,
                    expected: "string",
                },
            )?;
            let text = String::from_utf8_lossy(&text.as_bytes()).into_owned();
            Ok(parse_document(text))
        })
        .map_err(RunError::Init)?;
    html.set("parse", parse).map_err(RunError::Init)?;
    sanitize::install(lua, &html)?;

    lur.set("html", html).map_err(RunError::Init)?;
    Ok(())
}
