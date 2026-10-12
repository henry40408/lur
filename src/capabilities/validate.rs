//! `lur.validate` — a small validation toolkit: schema constructors plus
//! `parse` / `try`. Pure compute (no I/O, no policy); the same in one-shot
//! and serve mode.
//!
//! A schema is an opaque userdata wrapping an immutable [`Node`] tree, built
//! bottom-up (so it can never be cyclic). A plain Lua function is accepted
//! anywhere a schema is. Validation always builds a *new* result and collects
//! every [`Issue`], failing with a `{kind = "validation", message, issues}`
//! table (see [`super::raise`] for how a table is raised).
//!
//! Limits: schemas nest at most [`MAX_DEPTH`] deep (so cyclic *input* tables
//! are fine — traversal never goes deeper than the schema), at most
//! [`MAX_ISSUES`] issues are collected, and echoed input is cut at
//! [`ECHO_LEN`] characters.

use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mlua::{Error, Function, Lua, Table, UserData, Value};

use super::raise;
use crate::runtime::RunError;

const MAX_DEPTH: usize = 64;
const MAX_ISSUES: usize = 100;
const ECHO_LEN: usize = 40;
/// Longest user-supplied (validator function) message kept in an issue.
const USER_MESSAGE_LEN: usize = 200;
/// Luau numbers are f64: integers are exact only up to 2^53.
const MAX_SAFE_INT: i64 = 1 << 53;

enum Lit {
    Str(Vec<u8>),
    Num(f64),
    Bool(bool),
}

enum Node {
    Str {
        pattern: Option<mlua::LuaString>,
        min_len: Option<usize>,
        max_len: Option<usize>,
        default: Option<mlua::LuaString>,
    },
    Int {
        min: Option<i64>,
        max: Option<i64>,
        default: Option<i64>,
        clamp: bool,
    },
    Number {
        min: Option<f64>,
        max: Option<f64>,
        default: Option<f64>,
        clamp: bool,
    },
    Bool {
        default: Option<bool>,
    },
    OneOf(Vec<Lit>),
    List {
        item: Arc<Node>,
        min_items: Option<usize>,
        max_items: Option<usize>,
    },
    Object {
        /// Sorted by name so issue order is deterministic.
        fields: Vec<(String, Arc<Node>)>,
        strict: bool,
    },
    Optional(Arc<Node>),
    Func(Function),
}

impl Node {
    fn depth(&self) -> usize {
        1 + match self {
            Node::List { item, .. } => item.depth(),
            Node::Optional(inner) => inner.depth(),
            Node::Object { fields, .. } => fields.iter().map(|(_, n)| n.depth()).max().unwrap_or(0),
            _ => 0,
        }
    }

    fn has_default(&self) -> bool {
        match self {
            Node::Str { default, .. } => default.is_some(),
            Node::Int { default, .. } => default.is_some(),
            Node::Number { default, .. } => default.is_some(),
            Node::Bool { default } => default.is_some(),
            Node::Optional(inner) => inner.has_default(),
            _ => false,
        }
    }

    /// Does `value` count as "not provided" for this node? `nil` always does;
    /// `""` does for the scalar types that are usually parsed from text.
    fn is_missing(&self, value: &Value) -> bool {
        match value {
            Value::Nil => true,
            Value::String(s) if s.as_bytes().is_empty() => match self {
                Node::Int { .. } | Node::Number { .. } | Node::Bool { .. } => true,
                Node::Optional(inner) => inner.is_missing(value),
                _ => false,
            },
            _ => false,
        }
    }
}

/// The schema value scripts hold.
struct Schema(Arc<Node>);

impl UserData for Schema {}

struct Issue {
    path: String,
    message: String,
}

struct Ctx<'a> {
    find: &'a Function,
    lua: &'a Lua,
    issues: Vec<Issue>,
    truncated: bool,
}

impl Ctx<'_> {
    fn full(&self) -> bool {
        self.truncated
    }

    fn fail(&mut self, path: &str, message: impl Into<String>) {
        if self.issues.len() >= MAX_ISSUES {
            self.truncated = true;
            return;
        }
        self.issues.push(Issue {
            path: path.to_owned(),
            message: message.into(),
        });
    }
}

/// `Some(value)` is valid (`Nil` = absent optional); `None` means issues were
/// recorded. `Err` is a real script/runtime error (timeout, OOM, ...).
type Checked<'a> = Pin<Box<dyn Future<Output = mlua::Result<Option<Value>>> + Send + 'a>>;

fn echo(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut cut: String = text.chars().take(ECHO_LEN).collect();
    if text.chars().count() > ECHO_LEN {
        cut.push('…');
    }
    format!("{cut:?}")
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn is_ident(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn child_key(path: &str, key: &str) -> String {
    let mut shown: String = key.chars().take(ECHO_LEN).collect();
    if key.chars().count() > ECHO_LEN {
        shown.push('…');
    }
    if is_ident(key) && shown == key {
        if path.is_empty() {
            shown
        } else {
            format!("{path}.{shown}")
        }
    } else {
        format!("{path}[{shown:?}]")
    }
}

fn child_index(path: &str, i: usize) -> String {
    format!("{path}[{i}]")
}

/// A decimal integer string: `-?[0-9]+`, nothing else.
fn parse_int_str(b: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(b).ok()?;
    let digits = s.strip_prefix('-').unwrap_or(s);
    (!digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit())).then_some(s)
}

/// A finite decimal float string (`-1.5`, `.5`, `1e3`); no `+`, hex, `inf`,
/// `nan`, or whitespace.
fn parse_float_str(b: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(b).ok()?;
    let body = s.strip_prefix('-').unwrap_or(s);
    let first = *body.as_bytes().first()?;
    if !(first.is_ascii_digit() || first == b'.') {
        return None;
    }
    if !body
        .bytes()
        .all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

fn lua_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        _ => None,
    }
}

/// Message of a failed validator function: the Lua error text without the
/// `chunk:line:` position prefix or traceback. `Err` for errors that must keep
/// propagating (memory, deadline).
fn validator_message(e: &Error) -> Result<String, ()> {
    fn text(e: &Error) -> Option<String> {
        match e {
            Error::RuntimeError(s) => Some(s.clone()),
            Error::CallbackError { cause, .. } => text(cause),
            _ => None,
        }
    }
    let Some(raw) = text(e) else {
        return Err(());
    };
    let head = raw
        .split("\nstack traceback:")
        .next()
        .unwrap_or(&raw)
        .trim();
    if head == "lur: deadline exceeded" || head.starts_with("not enough memory") {
        return Err(());
    }
    // A table error without `__tostring` stringifies to its address.
    if head.starts_with("table: 0x") {
        return Ok("validation function failed".to_owned());
    }
    // `script:12: message` → `message`
    let msg = match head.split_once(": ") {
        Some((pos, rest)) if looks_like_position(pos) => rest,
        _ => head,
    };
    let mut out: String = msg.chars().take(USER_MESSAGE_LEN).collect();
    if out.is_empty() {
        "validation failed".clone_into(&mut out);
    }
    Ok(out)
}

fn looks_like_position(s: &str) -> bool {
    // `<chunk>:<line>` or `<chunk>:<line>:<col>`
    let mut parts = s.rsplitn(3, ':');
    let last = parts.next().unwrap_or("");
    let mid = parts.next();
    let num = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit());
    match (mid, parts.next()) {
        (Some(line), Some(chunk)) => num(last) && num(line) && !chunk.is_empty(),
        (Some(chunk), None) => num(last) && !chunk.is_empty(),
        _ => false,
    }
}

fn check<'a>(
    cx: &'a mut Ctx<'_>,
    node: &'a Node,
    value: Value,
    path: String,
    depth: usize,
) -> Checked<'a> {
    Box::pin(async move {
        if depth > MAX_DEPTH {
            cx.fail(&path, "is nested too deeply");
            return Ok(None);
        }
        match node {
            Node::Func(f) => match f.call_async::<Value>(value).await {
                Ok(v) => Ok(Some(v)),
                Err(e) => match validator_message(&e) {
                    Ok(message) => {
                        cx.fail(&path, message);
                        Ok(None)
                    }
                    Err(()) => Err(e),
                },
            },
            Node::Optional(inner) => {
                // `optional` only waives "required"; a `default` still fills.
                let skip = (inner.is_missing(&value) && !inner.has_default())
                    || (value.is_nil() && matches!(**inner, Node::Func(_)));
                if skip {
                    Ok(Some(Value::Nil))
                } else {
                    check(cx, inner, value, path, depth).await
                }
            }
            Node::Str {
                pattern,
                min_len,
                max_len,
                default,
            } => match value {
                Value::Nil => match default {
                    Some(d) => Ok(Some(Value::String(d.clone()))),
                    None => Ok(required(cx, &path)),
                },
                Value::String(s) => {
                    let ok = check_str(cx, &s, pattern.as_ref(), *min_len, *max_len, &path)?;
                    Ok(ok.then_some(Value::String(s)))
                }
                _ => {
                    cx.fail(&path, "must be a string");
                    Ok(None)
                }
            },
            Node::Int {
                min,
                max,
                default,
                clamp,
            } => {
                if node.is_missing(&value) {
                    return Ok(match default {
                        Some(d) => Some(Value::Number(*d as f64)),
                        None => required(cx, &path),
                    });
                }
                Ok(check_int(cx, &value, *min, *max, *clamp, &path))
            }
            Node::Number {
                min,
                max,
                default,
                clamp,
            } => {
                if node.is_missing(&value) {
                    return Ok(match default {
                        Some(d) => Some(Value::Number(*d)),
                        None => required(cx, &path),
                    });
                }
                Ok(check_number(cx, &value, *min, *max, *clamp, &path))
            }
            Node::Bool { default } => {
                if node.is_missing(&value) {
                    return Ok(match default {
                        Some(d) => Some(Value::Boolean(*d)),
                        None => required(cx, &path),
                    });
                }
                Ok(match &value {
                    Value::Boolean(_) => Some(value),
                    Value::String(s) if &*s.as_bytes() == b"true" => Some(Value::Boolean(true)),
                    Value::String(s) if &*s.as_bytes() == b"false" => Some(Value::Boolean(false)),
                    other => {
                        let got = describe(other);
                        cx.fail(&path, format!("must be a boolean{got}"));
                        None
                    }
                })
            }
            Node::OneOf(options) => {
                if value.is_nil() {
                    return Ok(required(cx, &path));
                }
                let hit = options.iter().any(|lit| match (lit, &value) {
                    (Lit::Str(a), Value::String(b)) => a.as_slice() == &*b.as_bytes(),
                    (Lit::Bool(a), Value::Boolean(b)) => a == b,
                    (Lit::Num(a), v) => lua_number(v) == Some(*a),
                    _ => false,
                });
                if hit {
                    Ok(Some(value))
                } else {
                    cx.fail(&path, format!("must be one of: {}", list_options(options)));
                    Ok(None)
                }
            }
            Node::List {
                item,
                min_items,
                max_items,
            } => check_list(cx, item, *min_items, *max_items, value, path, depth).await,
            Node::Object { fields, strict } => {
                check_object(cx, fields, *strict, value, path, depth).await
            }
        }
    })
}

fn required(cx: &mut Ctx<'_>, path: &str) -> Option<Value> {
    cx.fail(path, "is required");
    None
}

/// ` (got "abc")` for string input, nothing otherwise.
fn describe(value: &Value) -> String {
    match value {
        Value::String(s) => format!(" (got {})", echo(&s.as_bytes())),
        _ => String::new(),
    }
}

fn list_options(options: &[Lit]) -> String {
    let mut out = String::new();
    for (i, lit) in options.iter().enumerate() {
        if out.len() > 120 {
            out.push_str(", ...");
            break;
        }
        if i > 0 {
            out.push_str(", ");
        }
        match lit {
            Lit::Str(s) => out.push_str(&echo(s)),
            Lit::Num(n) => out.push_str(&fmt_num(*n)),
            Lit::Bool(b) => out.push_str(&b.to_string()),
        }
    }
    out
}

fn check_str(
    cx: &mut Ctx<'_>,
    s: &mlua::LuaString,
    pattern: Option<&mlua::LuaString>,
    min_len: Option<usize>,
    max_len: Option<usize>,
    path: &str,
) -> mlua::Result<bool> {
    let mut ok = true;
    if min_len.is_some() || max_len.is_some() {
        let len = String::from_utf8_lossy(&s.as_bytes()).chars().count();
        if let Some(min) = min_len
            && len < min
        {
            cx.fail(path, format!("must be at least {min} characters"));
            ok = false;
        }
        if let Some(max) = max_len
            && len > max
        {
            cx.fail(path, format!("must be at most {max} characters"));
            ok = false;
        }
    }
    if let Some(p) = pattern {
        let hit: Value = cx.find.call((s.clone(), p.clone()))?;
        if hit.is_nil() {
            cx.fail(path, "has an invalid format");
            ok = false;
        }
    }
    Ok(ok)
}

fn check_int(
    cx: &mut Ctx<'_>,
    value: &Value,
    min: Option<i64>,
    max: Option<i64>,
    clamp: bool,
    path: &str,
) -> Option<Value> {
    let n: i64 = match value {
        Value::String(s) => match parse_int_str(&s.as_bytes()).map(str::parse::<i64>) {
            Some(Ok(n)) if n.abs() <= MAX_SAFE_INT => n,
            Some(_) => {
                cx.fail(path, "is out of range (must be within +/-2^53)");
                return None;
            }
            None => {
                cx.fail(path, format!("must be an integer{}", describe(value)));
                return None;
            }
        },
        v => match lua_number(v) {
            Some(f) if f.fract() == 0.0 && f.abs() <= MAX_SAFE_INT as f64 => f as i64,
            Some(f) if f.fract() == 0.0 => {
                cx.fail(path, "is out of range (must be within +/-2^53)");
                return None;
            }
            _ => {
                cx.fail(path, "must be an integer");
                return None;
            }
        },
    };
    let mut n = n;
    if let Some(lo) = min
        && n < lo
    {
        if clamp {
            n = lo;
        } else {
            cx.fail(path, format!("must be >= {lo}"));
            return None;
        }
    }
    if let Some(hi) = max
        && n > hi
    {
        if clamp {
            n = hi;
        } else {
            cx.fail(path, format!("must be <= {hi}"));
            return None;
        }
    }
    Some(Value::Number(n as f64))
}

fn check_number(
    cx: &mut Ctx<'_>,
    value: &Value,
    min: Option<f64>,
    max: Option<f64>,
    clamp: bool,
    path: &str,
) -> Option<Value> {
    let parsed = match value {
        Value::String(s) => parse_float_str(&s.as_bytes()),
        v => lua_number(v).filter(|n| n.is_finite()),
    };
    let Some(mut n) = parsed else {
        cx.fail(path, format!("must be a finite number{}", describe(value)));
        return None;
    };
    if let Some(lo) = min
        && n < lo
    {
        if clamp {
            n = lo;
        } else {
            cx.fail(path, format!("must be >= {}", fmt_num(lo)));
            return None;
        }
    }
    if let Some(hi) = max
        && n > hi
    {
        if clamp {
            n = hi;
        } else {
            cx.fail(path, format!("must be <= {}", fmt_num(hi)));
            return None;
        }
    }
    Some(Value::Number(n))
}

/// Elements of a dense array table, or `None` when it has holes / other keys.
fn sequence(t: &Table) -> mlua::Result<Option<Vec<Value>>> {
    let len = t.raw_len();
    let mut count = 0usize;
    for pair in t.clone().pairs::<Value, Value>() {
        pair?;
        count += 1;
        if count > len {
            return Ok(None);
        }
    }
    if count != len {
        return Ok(None);
    }
    (1..=len)
        .map(|i| t.raw_get(i))
        .collect::<mlua::Result<Vec<Value>>>()
        .map(Some)
}

async fn check_list(
    cx: &mut Ctx<'_>,
    item: &Node,
    min_items: Option<usize>,
    max_items: Option<usize>,
    value: Value,
    path: String,
    depth: usize,
) -> mlua::Result<Option<Value>> {
    let items = match &value {
        Value::Nil => return Ok(required(cx, &path)),
        Value::Table(t) => sequence(t)?,
        _ => None,
    };
    let Some(items) = items else {
        cx.fail(&path, "must be a list");
        return Ok(None);
    };
    if let Some(min) = min_items
        && items.len() < min
    {
        cx.fail(&path, format!("must have at least {min} items"));
        return Ok(None);
    }
    if let Some(max) = max_items
        && items.len() > max
    {
        cx.fail(&path, format!("must have at most {max} items"));
        return Ok(None);
    }
    let out = cx.lua.create_table_with_capacity(items.len(), 0)?;
    let before = cx.issues.len();
    let mut n = 0;
    for (i, v) in items.into_iter().enumerate() {
        if cx.full() {
            break;
        }
        if let Some(v) = check(cx, item, v, child_index(&path, i + 1), depth + 1).await?
            && !v.is_nil()
        {
            n += 1;
            out.raw_set(n, v)?;
        }
    }
    Ok((cx.issues.len() == before && !cx.truncated).then_some(Value::Table(out)))
}

async fn check_object(
    cx: &mut Ctx<'_>,
    fields: &[(String, Arc<Node>)],
    strict: bool,
    value: Value,
    path: String,
    depth: usize,
) -> mlua::Result<Option<Value>> {
    let t = match value {
        Value::Nil => return Ok(required(cx, &path)),
        Value::Table(t) => t,
        _ => {
            cx.fail(&path, "must be an object");
            return Ok(None);
        }
    };
    let out = cx.lua.create_table()?;
    let before = cx.issues.len();
    for (name, node) in fields {
        if cx.full() {
            break;
        }
        let raw: Value = t.raw_get(name.as_str())?;
        let at = child_key(&path, name);
        if let Some(v) = check(cx, node, raw, at, depth + 1).await?
            && !v.is_nil()
        {
            out.raw_set(name.as_str(), v)?;
        }
    }
    if strict {
        for pair in t.pairs::<Value, Value>() {
            if cx.full() {
                break;
            }
            let (key, _) = pair?;
            let known = match &key {
                Value::String(k) => fields.iter().any(|(n, _)| n.as_bytes() == &*k.as_bytes()),
                _ => false,
            };
            if known {
                continue;
            }
            let at = match &key {
                Value::String(k) => child_key(&path, &String::from_utf8_lossy(&k.as_bytes())),
                Value::Integer(i) => format!("{path}[{i}]"),
                Value::Number(n) => format!("{path}[{}]", fmt_num(*n)),
                other => format!("{path}[<{}>]", tname(other)),
            };
            cx.fail(&at, "is not a known key");
        }
    }
    Ok((cx.issues.len() == before && !cx.truncated).then_some(Value::Table(out)))
}

// ---------------------------------------------------------------- constructors

fn to_node(value: Value, fname: &str, what: &str) -> mlua::Result<Arc<Node>> {
    match value {
        Value::UserData(ud) => match ud.borrow::<Schema>() {
            Ok(s) => Ok(Arc::clone(&s.0)),
            Err(_) => Err(not_schema(fname, what, "userdata")),
        },
        Value::Function(f) => Ok(Arc::new(Node::Func(f))),
        other => Err(not_schema(fname, what, tname(&other))),
    }
}

fn not_schema(fname: &str, what: &str, got: &str) -> Error {
    Error::runtime(format!(
        "{fname}: {what} must be a schema or function, got {got}"
    ))
}

fn schema(lua: &Lua, node: Node, fname: &str) -> mlua::Result<mlua::AnyUserData> {
    if node.depth() > MAX_DEPTH {
        return Err(Error::runtime(format!(
            "{fname}: schema is nested deeper than {MAX_DEPTH} levels"
        )));
    }
    lua.create_userdata(Schema(Arc::new(node)))
}

/// The optional `opts` table of a constructor, with unknown keys rejected.
fn opts_of(value: Value, fname: &str, pos: usize, allowed: &[&str]) -> mlua::Result<Option<Table>> {
    let t = match value {
        Value::Nil => return Ok(None),
        Value::Table(t) => t,
        other => {
            return Err(Error::runtime(format!(
                "{fname}: argument #{pos} must be table, got {}",
                tname(&other)
            )));
        }
    };
    for pair in t.clone().pairs::<Value, Value>() {
        let (k, _) = pair?;
        match &k {
            Value::String(s) if allowed.iter().any(|a| a.as_bytes() == &*s.as_bytes()) => {}
            Value::String(s) => {
                return Err(Error::runtime(format!(
                    "{fname}: unknown option {}",
                    echo(&s.as_bytes())
                )));
            }
            other => {
                return Err(Error::runtime(format!(
                    "{fname}: opts keys must be strings, got {}",
                    tname(other)
                )));
            }
        }
    }
    Ok(Some(t))
}

fn opt_get(t: Option<&Table>, key: &str) -> mlua::Result<Value> {
    match t {
        Some(t) => t.raw_get(key),
        None => Ok(Value::Nil),
    }
}

fn bad_opt(fname: &str, key: &str, want: &str, got: &Value) -> Error {
    Error::runtime(format!(
        "{fname}: opts.{key} must be {want}, got {}",
        tname(got)
    ))
}

fn opt_i64(t: Option<&Table>, fname: &str, key: &str) -> mlua::Result<Option<i64>> {
    let v = opt_get(t, key)?;
    match lua_number(&v) {
        None if v.is_nil() => Ok(None),
        Some(f) if f.fract() == 0.0 && f.abs() <= MAX_SAFE_INT as f64 => Ok(Some(f as i64)),
        _ => Err(bad_opt(fname, key, "an integer within +/-2^53", &v)),
    }
}

fn opt_count(t: Option<&Table>, fname: &str, key: &str) -> mlua::Result<Option<usize>> {
    match opt_i64(t, fname, key)? {
        None => Ok(None),
        Some(n) if n >= 0 => Ok(Some(usize::try_from(n).unwrap_or(usize::MAX))),
        Some(_) => Err(Error::runtime(format!("{fname}: opts.{key} must be >= 0"))),
    }
}

fn opt_f64(t: Option<&Table>, fname: &str, key: &str) -> mlua::Result<Option<f64>> {
    let v = opt_get(t, key)?;
    match lua_number(&v) {
        None if v.is_nil() => Ok(None),
        Some(f) if f.is_finite() => Ok(Some(f)),
        _ => Err(bad_opt(fname, key, "a finite number", &v)),
    }
}

fn opt_bool(t: Option<&Table>, fname: &str, key: &str) -> mlua::Result<Option<bool>> {
    match opt_get(t, key)? {
        Value::Nil => Ok(None),
        Value::Boolean(b) => Ok(Some(b)),
        other => Err(bad_opt(fname, key, "boolean", &other)),
    }
}

fn opt_string(t: Option<&Table>, fname: &str, key: &str) -> mlua::Result<Option<mlua::LuaString>> {
    match opt_get(t, key)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s)),
        other => Err(bad_opt(fname, key, "string", &other)),
    }
}

fn range_check<T: PartialOrd + std::fmt::Display>(
    fname: &str,
    lo: (&str, Option<T>),
    hi: (&str, Option<T>),
) -> mlua::Result<()> {
    if let (Some(a), Some(b)) = (&lo.1, &hi.1)
        && a > b
    {
        return Err(Error::runtime(format!(
            "{fname}: opts.{} ({a}) must not exceed opts.{} ({b})",
            lo.0, hi.0
        )));
    }
    Ok(())
}

fn install_constructors(lua: &Lua, v: &Table) -> Result<(), RunError> {
    let init = RunError::Init;

    v.set(
        "str",
        lua.create_function(|lua, opts: Value| {
            const F: &str = "lur.validate.str";
            let t = opts_of(opts, F, 1, &["pattern", "min_len", "max_len", "default"])?;
            let min_len = opt_count(t.as_ref(), F, "min_len")?;
            let max_len = opt_count(t.as_ref(), F, "max_len")?;
            range_check(F, ("min_len", min_len), ("max_len", max_len))?;
            let pattern = opt_string(t.as_ref(), F, "pattern")?;
            if let Some(p) = &pattern {
                // Reject malformed patterns up front, with string.find's own message.
                let find: Function = lua.globals().get::<Table>("string")?.get("find")?;
                if let Err(e) = find.call::<Value>((lua.create_string("")?, p.clone())) {
                    let msg = validator_message(&e).unwrap_or_else(|()| e.to_string());
                    return Err(Error::runtime(format!(
                        "{F}: opts.pattern is not a valid Lua pattern: {msg}"
                    )));
                }
            }
            let default = opt_string(t.as_ref(), F, "default")?;
            schema(
                lua,
                Node::Str {
                    pattern,
                    min_len,
                    max_len,
                    default,
                },
                F,
            )
        })
        .map_err(init)?,
    )
    .map_err(RunError::Init)?;

    v.set(
        "int",
        lua.create_function(|lua, opts: Value| {
            const F: &str = "lur.validate.int";
            let t = opts_of(opts, F, 1, &["min", "max", "default", "clamp"])?;
            let min = opt_i64(t.as_ref(), F, "min")?;
            let max = opt_i64(t.as_ref(), F, "max")?;
            range_check(F, ("min", min), ("max", max))?;
            schema(
                lua,
                Node::Int {
                    min,
                    max,
                    default: opt_i64(t.as_ref(), F, "default")?,
                    clamp: opt_bool(t.as_ref(), F, "clamp")?.unwrap_or(false),
                },
                F,
            )
        })
        .map_err(RunError::Init)?,
    )
    .map_err(RunError::Init)?;

    v.set(
        "number",
        lua.create_function(|lua, opts: Value| {
            const F: &str = "lur.validate.number";
            let t = opts_of(opts, F, 1, &["min", "max", "default", "clamp"])?;
            let min = opt_f64(t.as_ref(), F, "min")?;
            let max = opt_f64(t.as_ref(), F, "max")?;
            range_check(F, ("min", min), ("max", max))?;
            schema(
                lua,
                Node::Number {
                    min,
                    max,
                    default: opt_f64(t.as_ref(), F, "default")?,
                    clamp: opt_bool(t.as_ref(), F, "clamp")?.unwrap_or(false),
                },
                F,
            )
        })
        .map_err(RunError::Init)?,
    )
    .map_err(RunError::Init)?;

    v.set(
        "bool",
        lua.create_function(|lua, opts: Value| {
            const F: &str = "lur.validate.bool";
            let t = opts_of(opts, F, 1, &["default"])?;
            schema(
                lua,
                Node::Bool {
                    default: opt_bool(t.as_ref(), F, "default")?,
                },
                F,
            )
        })
        .map_err(RunError::Init)?,
    )
    .map_err(RunError::Init)?;

    v.set(
        "one_of",
        lua.create_function(|lua, values: Value| {
            const F: &str = "lur.validate.one_of";
            let Value::Table(t) = values else {
                return Err(Error::runtime(format!(
                    "{F}: argument #1 must be table, got {}",
                    tname(&values)
                )));
            };
            let items = sequence(&t)?.ok_or_else(|| {
                Error::runtime(format!("{F}: argument #1 must be an array of values"))
            })?;
            if items.is_empty() {
                return Err(Error::runtime(format!(
                    "{F}: argument #1 must not be empty"
                )));
            }
            let mut lits = Vec::with_capacity(items.len());
            for item in items {
                lits.push(match item {
                    Value::String(s) => Lit::Str(s.as_bytes().to_vec()),
                    Value::Boolean(b) => Lit::Bool(b),
                    other => match lua_number(&other) {
                        Some(n) if !n.is_nan() => Lit::Num(n),
                        _ => {
                            return Err(Error::runtime(format!(
                                "{F}: values must be strings, numbers or booleans, got {}",
                                tname(&other)
                            )));
                        }
                    },
                });
            }
            schema(lua, Node::OneOf(lits), F)
        })
        .map_err(RunError::Init)?,
    )
    .map_err(RunError::Init)?;

    v.set(
        "list",
        lua.create_function(|lua, (item, opts): (Value, Value)| {
            const F: &str = "lur.validate.list";
            let item = to_node(item, F, "argument #1")?;
            let t = opts_of(opts, F, 2, &["min_items", "max_items"])?;
            let min_items = opt_count(t.as_ref(), F, "min_items")?;
            let max_items = opt_count(t.as_ref(), F, "max_items")?;
            range_check(F, ("min_items", min_items), ("max_items", max_items))?;
            schema(
                lua,
                Node::List {
                    item,
                    min_items,
                    max_items,
                },
                F,
            )
        })
        .map_err(RunError::Init)?,
    )
    .map_err(RunError::Init)?;

    v.set(
        "object",
        lua.create_function(|lua, (fields, opts): (Value, Value)| {
            const F: &str = "lur.validate.object";
            let Value::Table(t) = fields else {
                return Err(Error::runtime(format!(
                    "{F}: argument #1 must be table, got {}",
                    tname(&fields)
                )));
            };
            let o = opts_of(opts, F, 2, &["strict"])?;
            let strict = opt_bool(o.as_ref(), F, "strict")?.unwrap_or(false);
            let mut list = Vec::new();
            for pair in t.pairs::<Value, Value>() {
                let (k, v) = pair?;
                let Value::String(k) = k else {
                    return Err(Error::runtime(format!(
                        "{F}: field names must be strings, got {}",
                        tname(&k)
                    )));
                };
                let name = k
                    .to_str()
                    .map_err(|_e| Error::runtime(format!("{F}: field names must be valid UTF-8")))?
                    .to_owned();
                let node = to_node(v, F, &format!("field '{name}'"))?;
                list.push((name, node));
            }
            list.sort_by(|a, b| a.0.cmp(&b.0));
            schema(
                lua,
                Node::Object {
                    fields: list,
                    strict,
                },
                F,
            )
        })
        .map_err(RunError::Init)?,
    )
    .map_err(RunError::Init)?;

    v.set(
        "optional",
        lua.create_function(|lua, inner: Value| {
            const F: &str = "lur.validate.optional";
            let inner = to_node(inner, F, "argument #1")?;
            schema(lua, Node::Optional(inner), F)
        })
        .map_err(RunError::Init)?,
    )
    .map_err(RunError::Init)?;

    Ok(())
}

// ---------------------------------------------------------------- entry points

fn build_error(lua: &Lua, meta: &Table, cx: &Ctx<'_>) -> mlua::Result<Table> {
    let mut message = String::from("validation failed: ");
    for (i, issue) in cx.issues.iter().take(10).enumerate() {
        if i > 0 {
            message.push_str("; ");
        }
        if !issue.path.is_empty() {
            message.push_str(&issue.path);
            message.push_str(": ");
        }
        message.push_str(&issue.message);
    }
    let hidden = cx.issues.len().saturating_sub(10);
    if hidden > 0 || cx.truncated {
        let _ = write!(
            message,
            "; (+{hidden}{} more)",
            if cx.truncated { "+" } else { "" }
        );
    }

    let issues = lua.create_table_with_capacity(cx.issues.len(), 0)?;
    for (i, issue) in cx.issues.iter().enumerate() {
        let entry = lua.create_table()?;
        entry.set("path", issue.path.as_str())?;
        entry.set("message", issue.message.as_str())?;
        issues.raw_set(i + 1, entry)?;
    }
    let err = lua.create_table()?;
    err.set("kind", "validation")?;
    err.set("message", message)?;
    err.set("issues", issues)?;
    if cx.truncated {
        err.set("truncated", true)?;
    }
    err.set_metatable(Some(meta.clone()))?;
    Ok(err)
}

/// Names of the top-level fields of an object schema that are lists (also
/// when wrapped in `optional`); `None` if `schema` is not an object schema.
/// `req.valid_query` uses it to feed those fields from `query_all`.
pub(crate) fn list_field_names(schema: &Value) -> Option<Vec<String>> {
    let Value::UserData(ud) = schema else {
        return None;
    };
    let s = ud.borrow::<Schema>().ok()?;
    let Node::Object { fields, .. } = &*s.0 else {
        return None;
    };
    let is_list = |mut n: &Node| {
        while let Node::Optional(inner) = n {
            n = inner;
        }
        matches!(n, Node::List { .. })
    };
    Some(
        fields
            .iter()
            .filter(|(_, n)| is_list(n))
            .map(|(name, _)| name.clone())
            .collect(),
    )
}

pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let validate = lua.create_table().map_err(RunError::Init)?;
    install_constructors(lua, &validate)?;

    let find: Function = lua
        .globals()
        .get::<Table>("string")
        .and_then(|s| s.get("find"))
        .map_err(RunError::Init)?;
    let meta = raise::error_meta(lua).map_err(RunError::Init)?;

    // `try`: (true, value) | (false, err). `parse` re-raises the same err.
    let run = move |fname: &'static str| {
        let find = find.clone();
        let meta = meta.clone();
        lua.create_async_function(move |lua, (schema, value): (Value, Value)| {
            let find = find.clone();
            let meta = meta.clone();
            async move {
                let node = to_node(schema, fname, "argument #1")?;
                let mut cx = Ctx {
                    find: &find,
                    lua: &lua,
                    issues: Vec::new(),
                    truncated: false,
                };
                let out = check(&mut cx, &node, value, String::new(), 0).await?;
                match out {
                    Some(v) if cx.issues.is_empty() => Ok((true, v)),
                    _ => Ok((false, Value::Table(build_error(&lua, &meta, &cx)?))),
                }
            }
        })
    };

    validate
        .set("try", run("lur.validate.try").map_err(RunError::Init)?)
        .map_err(RunError::Init)?;
    let factory = raise::factory(lua).map_err(RunError::Init)?;
    let parse = raise::raising(&factory, run("lur.validate.parse").map_err(RunError::Init)?)
        .map_err(RunError::Init)?;
    validate.set("parse", parse).map_err(RunError::Init)?;

    lur.set("validate", validate).map_err(RunError::Init)?;
    Ok(())
}

/// Luau's `type()` name (mlua reports whole numbers as `integer`).
fn tname(v: &Value) -> &'static str {
    match v {
        Value::Integer(_) | Value::Number(_) => "number",
        other => other.type_name(),
    }
}

#[cfg(test)]
mod tests {
    use super::{echo, looks_like_position, parse_float_str, parse_int_str};

    #[test]
    fn int_strings_are_strictly_decimal() {
        for ok in ["0", "-1", "42", "007"] {
            assert!(parse_int_str(ok.as_bytes()).is_some(), "{ok}");
        }
        for bad in ["", "-", "+1", "1.5", "1e3", "0x10", " 1", "1 ", "--1"] {
            assert!(parse_int_str(bad.as_bytes()).is_none(), "{bad}");
        }
    }

    #[test]
    fn float_strings_must_be_finite_decimal() {
        assert_eq!(parse_float_str(b"1.5"), Some(1.5));
        assert_eq!(parse_float_str(b"-.5"), Some(-0.5));
        assert_eq!(parse_float_str(b"1e3"), Some(1000.0));
        for bad in [
            "", "+1", "inf", "nan", "0x10", " 1", "1e999", "--1", "e5", "1_0",
        ] {
            assert!(parse_float_str(bad.as_bytes()).is_none(), "{bad}");
        }
    }

    #[test]
    fn echo_truncates() {
        let long = "x".repeat(500);
        let out = echo(long.as_bytes());
        assert!(out.chars().count() < 50, "{out}");
        assert!(out.ends_with("…\""), "{out}");
        assert_eq!(echo(b"a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn position_prefix_detection() {
        assert!(looks_like_position("script:12"));
        assert!(looks_like_position("app.lua:3:7"));
        assert!(!looks_like_position("note"));
        assert!(!looks_like_position("a:b"));
    }
}
