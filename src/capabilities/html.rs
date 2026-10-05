//! `lur.html` — parse HTML and query it with CSS selectors.
//!
//! `scraper::Html` is `!Send` (tendril uses non-atomic refcounts) but mlua's
//! `send` mode needs `Send` userdata. A [`Node`] therefore holds only the source
//! text and an `ego_tree::NodeId`; the parsed tree lives in a small per-thread
//! LRU and is re-parsed on a miss. Parsing is deterministic, so a `NodeId`
//! stays valid across re-parses — the cache affects speed, never results.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ego_tree::NodeId;
use mlua::{Error, Lua, Table, UserData, UserDataMethods, Value};
use scraper::{ElementRef, Html, Selector};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

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
struct Node {
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
        methods.add_method("text", |_, this, ()| {
            this.with_element(|html, el| {
                let el = el.unwrap_or_else(|| html.root_element());
                el.text().collect::<String>()
            })
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
        methods.add_method("attr", |lua, this, name: Value| {
            let name: mlua::LuaString = argcheck::arg(lua, name, "lur.html.attr", 1, "string")?;
            let name = name
                .to_str()
                .map_err(|e| Error::runtime(format!("lur.html.attr: {e}")))?;
            this.with_element(|_, el| el.and_then(|e| e.value().attr(&name).map(str::to_owned)))
        });
        methods.add_method("attrs", |lua, this, ()| {
            let pairs: Vec<(String, String)> = this.with_element(|_, el| {
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
        });
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

fn css_arg(lua: &Lua, value: Value, fname: &str) -> mlua::Result<String> {
    let s: mlua::LuaString = argcheck::arg(lua, value, fname, 1, "string")?;
    s.to_str()
        .map(|s| s.to_string())
        .map_err(|e| Error::runtime(format!("{fname}: {e}")))
}

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let html = lua.create_table().map_err(RunError::Init)?;

    // Input is often a raw HTTP body, so invalid UTF-8 is replaced, not rejected.
    let parse = lua
        .create_function(|lua, text: Value| {
            let text: mlua::LuaString = argcheck::arg(lua, text, "lur.html.parse", 1, "string")?;
            let text = String::from_utf8_lossy(&text.as_bytes()).into_owned();
            let source = Arc::new(Source {
                id: NEXT_DOC_ID.fetch_add(1, Ordering::Relaxed),
                text: text.into_boxed_str(),
            });
            let root = source.tree().tree.root().id();
            Ok(Node { source, id: root })
        })
        .map_err(RunError::Init)?;
    html.set("parse", parse).map_err(RunError::Init)?;

    lur.set("html", html).map_err(RunError::Init)?;
    Ok(())
}
