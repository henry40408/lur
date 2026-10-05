//! `lur.xml` — parse XML (RSS, Atom, sitemaps, …) into a tree you walk with
//! slash paths. The tree is plain owned data behind an `Arc`, so unlike
//! `lur.html` a node is just a document handle plus an index: `Send`, no
//! re-parsing, no cache.
//!
//! Names are matched exactly as written (`dc:creator`); namespaces are not
//! resolved. Entities other than the predefined five and numeric references are
//! kept as literal text instead of failing, because feeds in the wild use
//! `&nbsp;` without declaring it. `DOCTYPE` is ignored, so nothing expands.

use std::sync::Arc;

use encoding_rs::{Encoding, UTF_8};
use mlua::{Error, Lua, Table, UserData, UserDataMethods, Value};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::capabilities::argcheck;
use crate::runtime::RunError;

/// Index 0 is the document itself; it has no name and holds the root element.
struct Doc {
    nodes: Vec<Elem>,
}

struct Elem {
    name: String,
    attrs: Vec<(String, String)>,
    parent: Option<u32>,
    kids: Vec<Kid>,
}

enum Kid {
    Text(String),
    Elem(u32),
}

impl Doc {
    fn push_text(&mut self, at: u32, text: &str) {
        if text.is_empty() {
            return;
        }
        let kids = &mut self.nodes[at as usize].kids;
        if let Some(Kid::Text(last)) = kids.last_mut() {
            last.push_str(text);
        } else {
            kids.push(Kid::Text(text.to_owned()));
        }
    }

    fn open(&mut self, parent: u32, start: &BytesStart<'_>) -> Result<u32, String> {
        let mut attrs = Vec::new();
        for attr in start.attributes() {
            let attr = attr.map_err(|e| e.to_string())?;
            attrs.push((attr.key.as_ref().to_owned(), unescape(&attr.value)));
        }
        let id = u32::try_from(self.nodes.len()).map_err(|e| format!("document too large: {e}"))?;
        self.nodes.push(Elem {
            name: start.name().as_ref().to_owned(),
            attrs,
            parent: Some(parent),
            kids: Vec::new(),
        });
        self.nodes[parent as usize].kids.push(Kid::Elem(id));
        Ok(id)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let mut doc = Doc {
            nodes: vec![Elem {
                name: String::new(),
                attrs: Vec::new(),
                parent: None,
                kids: Vec::new(),
            }],
        };
        let mut reader = Reader::from_str(text);
        let mut stack: Vec<u32> = vec![0];
        loop {
            let at = *stack.last().expect("the document root is never popped");
            let event = reader
                .read_event()
                .map_err(|e| format!("{e} at {}", line_col(text, reader.buffer_position())))?;
            // `Empty` (`<a/>`) has no matching `End`, so it must not be pushed.
            let is_start = matches!(event, Event::Start(_));
            match event {
                Event::Eof => break,
                Event::Start(e) | Event::Empty(e) => {
                    if at == 0 && !doc.nodes[0].kids.is_empty() {
                        return Err(format!(
                            "more than one root element at {}",
                            line_col(text, reader.buffer_position())
                        ));
                    }
                    let id = doc.open(at, &e)?;
                    if is_start {
                        stack.push(id);
                    }
                }
                Event::End(_) => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                }
                // Text outside the root element is whitespace or garbage.
                Event::Text(t) if at != 0 => doc.push_text(at, &t.xml10_content()),
                Event::CData(c) if at != 0 => doc.push_text(at, &c),
                Event::GeneralRef(r) if at != 0 => doc.push_text(at, &resolve_ref(&r)),
                _ => {}
            }
        }
        if stack.len() > 1 {
            let open = &doc.nodes[*stack.last().expect("non-empty") as usize].name;
            return Err(format!("unclosed element <{open}>"));
        }
        if doc.nodes[0].kids.is_empty() {
            return Err("no root element".to_owned());
        }
        Ok(doc)
    }
}

fn resolve_ref(r: &quick_xml::events::BytesRef<'_>) -> String {
    let name: &str = r;
    entity(name).unwrap_or_else(|| format!("&{name};"))
}

/// The predefined five and numeric references; `None` for anything else.
fn entity(name: &str) -> Option<String> {
    match name {
        "lt" => Some("<".into()),
        "gt" => Some(">".into()),
        "amp" => Some("&".into()),
        "quot" => Some("\"".into()),
        "apos" => Some("'".into()),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)
                .filter(|c| *c != '\0')
                .map(String::from)
        }
    }
}

/// Attribute values arrive escaped; unknown entities stay as written.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(end) = rest.find(';').filter(|end| *end <= 32) {
            match entity(&rest[1..end]) {
                Some(text) => out.push_str(&text),
                None => out.push_str(&rest[..=end]),
            }
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// `line N, column M` (1-based) for a byte offset.
fn line_col(text: &str, pos: u64) -> String {
    let end = usize::try_from(pos).unwrap_or(usize::MAX).min(text.len());
    let end = (0..=end)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let before = &text[..end];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    format!("line {line}, column {col}")
}

/// XML declares its own encoding: a BOM wins, then `<?xml … encoding="…"?>`,
/// else UTF-8 (invalid bytes replaced, since input is often a raw HTTP body).
fn decode_input(bytes: &[u8]) -> Result<String, String> {
    if Encoding::for_bom(bytes).is_some() {
        return Ok(UTF_8.decode(bytes).0.into_owned());
    }
    if bytes.starts_with(b"<?xml") {
        let decl_end = bytes
            .windows(2)
            .position(|w| w == b"?>")
            .unwrap_or(bytes.len());
        let decl = String::from_utf8_lossy(&bytes[..decl_end.min(256)]).to_ascii_lowercase();
        if let Some(i) = decl.find("encoding") {
            let value = decl[i + "encoding".len()..]
                .trim_start()
                .strip_prefix('=')
                .map(|v| v.trim_start().trim_start_matches(['"', '\'']));
            if let Some(value) = value {
                let label = value.split(['"', '\'']).next().unwrap_or("");
                let enc = Encoding::for_label(label.as_bytes())
                    .ok_or_else(|| format!("unknown encoding {label:?} in the XML declaration"))?;
                // UTF-16 can't be what's declared in bytes that read as ASCII.
                if enc != UTF_8 && enc.output_encoding() == enc {
                    return Ok(enc.decode(bytes).0.into_owned());
                }
            }
        }
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

struct Step {
    descendant: bool,
    name: String,
}

/// `a/b` child steps, `//a` descendants, `a//b` descendants of `a`, `*` any name.
fn parse_path(path: &str, fname: &str) -> mlua::Result<Vec<Step>> {
    let bad = |why: &str| Error::runtime(format!("{fname}: invalid path {path:?}: {why}"));
    let mut rest = path;
    let mut descendant = false;
    if let Some(r) = rest.strip_prefix("//") {
        descendant = true;
        rest = r;
    } else if rest.starts_with('/') {
        return Err(bad("use `a/b` or `//a`, not a leading single `/`"));
    }
    let mut steps = Vec::new();
    loop {
        let end = rest.find('/').unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(bad("empty or malformed step"));
        }
        steps.push(Step {
            descendant,
            name: name.to_owned(),
        });
        rest = &rest[end..];
        if rest.is_empty() {
            return Ok(steps);
        }
        if let Some(r) = rest.strip_prefix("//") {
            descendant = true;
            rest = r;
        } else {
            descendant = false;
            rest = &rest[1..];
        }
        if rest.is_empty() {
            return Err(bad("trailing `/`"));
        }
    }
}

#[derive(Clone)]
struct Node {
    doc: Arc<Doc>,
    idx: u32,
}

impl Node {
    fn derive(&self, idx: u32) -> Self {
        Self {
            doc: Arc::clone(&self.doc),
            idx,
        }
    }

    fn elem(&self) -> &Elem {
        &self.doc.nodes[self.idx as usize]
    }

    fn child_elems(&self, of: u32) -> impl Iterator<Item = u32> + '_ {
        self.doc.nodes[of as usize]
            .kids
            .iter()
            .filter_map(|k| match k {
                Kid::Elem(i) => Some(*i),
                Kid::Text(_) => None,
            })
    }

    /// Pre-order, so results stay in document order.
    fn descendants(&self, of: u32, out: &mut Vec<u32>) {
        let mut stack: Vec<u32> = self.child_elems(of).collect();
        stack.reverse();
        while let Some(i) = stack.pop() {
            out.push(i);
            let before = stack.len();
            stack.extend(self.child_elems(i));
            stack[before..].reverse();
        }
    }

    fn select(&self, path: &str, fname: &str, first_only: bool) -> mlua::Result<Vec<Node>> {
        let steps = parse_path(path, fname)?;
        let mut current = vec![self.idx];
        for step in &steps {
            let mut candidates = Vec::new();
            for &from in &current {
                if step.descendant {
                    self.descendants(from, &mut candidates);
                } else {
                    candidates.extend(self.child_elems(from));
                }
            }
            let mut seen = vec![false; self.doc.nodes.len()];
            current = candidates
                .into_iter()
                .filter(|i| {
                    let hit = step.name == "*" || self.doc.nodes[*i as usize].name == step.name;
                    hit && !std::mem::replace(&mut seen[*i as usize], true)
                })
                .collect();
            if current.is_empty() {
                break;
            }
        }
        if first_only {
            current.truncate(1);
        }
        Ok(current.into_iter().map(|i| self.derive(i)).collect())
    }

    /// Text of the node and everything under it, in document order.
    fn text(&self) -> String {
        let mut out = String::new();
        let mut stack = vec![(self.idx, 0usize)];
        while let Some((at, pos)) = stack.pop() {
            let Some(kid) = self.doc.nodes[at as usize].kids.get(pos) else {
                continue;
            };
            stack.push((at, pos + 1));
            match kid {
                Kid::Text(t) => out.push_str(t),
                Kid::Elem(i) => stack.push((*i, 0)),
            }
        }
        out
    }
}

impl UserData for Node {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("select", |lua, this, path: Value| {
            let path = string_arg(lua, path, "lur.xml.select")?;
            this.select(&path, "lur.xml.select", false)
        });
        methods.add_method("select_one", |lua, this, path: Value| {
            let path = string_arg(lua, path, "lur.xml.select_one")?;
            Ok(this
                .select(&path, "lur.xml.select_one", true)?
                .into_iter()
                .next())
        });
        methods.add_method("text", |_, this, ()| Ok(this.text()));
        methods.add_method("tag", |_, this, ()| {
            Ok((this.idx != 0).then(|| this.elem().name.clone()))
        });
        methods.add_method("attr", |lua, this, name: Value| {
            let name = string_arg(lua, name, "lur.xml.attr")?;
            Ok(this
                .elem()
                .attrs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.clone()))
        });
        methods.add_method("attrs", |lua, this, ()| {
            let out = lua.create_table()?;
            for (k, v) in &this.elem().attrs {
                out.set(k.as_str(), v.as_str())?;
            }
            Ok(out)
        });
        methods.add_method("parent", |_, this, ()| {
            Ok(this
                .elem()
                .parent
                .filter(|p| *p != 0)
                .map(|p| this.derive(p)))
        });
        methods.add_method("children", |_, this, ()| {
            Ok(this
                .child_elems(this.idx)
                .map(|i| this.derive(i))
                .collect::<Vec<_>>())
        });
    }
}

fn string_arg(lua: &Lua, value: Value, fname: &str) -> mlua::Result<String> {
    let s: mlua::LuaString = argcheck::arg(lua, value, fname, 1, "string")?;
    s.to_str()
        .map(|s| s.to_string())
        .map_err(|e| Error::runtime(format!("{fname}: {e}")))
}

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let xml = lua.create_table().map_err(RunError::Init)?;

    let parse = lua
        .create_function(|lua, data: Value| {
            let data: mlua::LuaString = argcheck::arg(lua, data, "lur.xml.parse", 1, "string")?;
            let text = decode_input(&data.as_bytes())
                .map_err(|e| Error::runtime(format!("lur.xml.parse: {e}")))?;
            let doc =
                Doc::parse(&text).map_err(|e| Error::runtime(format!("lur.xml.parse: {e}")))?;
            Ok(Node {
                doc: Arc::new(doc),
                idx: 0,
            })
        })
        .map_err(RunError::Init)?;
    xml.set("parse", parse).map_err(RunError::Init)?;

    lur.set("xml", xml).map_err(RunError::Init)?;
    Ok(())
}
