//! `lur.text` — `replacer(pairs, opts?)`: many-to-many literal replacement in one
//! left-to-right pass, with Go's `strings.NewReplacer` semantics.
//!
//! A replacer is a byte trie over the `old` strings. At each position the
//! candidates are the `old` strings that are a prefix of the remaining text;
//! `match = "first"` (default) picks the one listed first, `match = "longest"` the
//! longest. Matches never overlap; text that matches nothing passes through
//! byte for byte (invalid UTF-8 included).
//!
//! Memory is bounded by `--replacer-max-bytes`: replacers that live only in one
//! VM are counted per VM, replacers shared through `opts.key` are counted for
//! the process. Over the cap raises; nothing is evicted.
//!
//! ## Design notes
//!
//! - Semantics are Go `strings.NewReplacer`'s (earliest listed pair wins at a position,
//!   non-overlapping, left to right). `match = "longest"` is an option, not a second
//!   constructor: input, method and budget are identical, only the winner differs, and both
//!   are the same trie.
//! - Input is parsed `{ old, new }` pairs, deliberately not a dictionary format: `OpenCC` TSV
//!   and the like are parsed in Lua (~15 ms for 54k lines measured, so no Rust text-parsing
//!   entry).
//! - Sharing is explicit (`opts.key`), never inferred from content: no hashing, and each VM
//!   otherwise owns its replacer (GC releases it, which keeps pool isolation). A key reused
//!   with a different `#pairs` or `match` raises instead of silently returning something else.
//!   A key is "first definition wins while some VM still holds it": once collected, a later
//!   call rebuilds from its own pairs, so a shared key must not carry request-specific data.
//! - The cap (`--replacer-max-bytes`) is per VM for unkeyed and per process for keyed,
//!   enforced by raising after a GC (no eviction: a replacer in use cannot be dropped). The
//!   trie is built incrementally so an over-cap dictionary stops early.
//! - `replace` runs in native code, so it caps its output at the VM's remaining `--memory`
//!   and polls the deadline. `first` also prunes by smallest pair index under a node;
//!   `longest` is O(n x longest old) like Go, bounded by the deadline.
//! - Not done: a loader-function form (build only on a cache miss), rune-aware matching
//!   (bytes are matched), regex replacement.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use mlua::{Error, Lua, Table, UserData, UserDataMethods, Value};

use crate::capabilities::argcheck;
use crate::runtime::{RunError, deadline_passed};

pub const DEFAULT_REPLACER_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Which candidate wins when several `old` strings match at one position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    First,
    Longest,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::First => "first",
            Mode::Longest => "longest",
        }
    }
}

struct Node {
    /// Sorted by byte.
    children: Vec<(u8, u32)>,
    /// Index of the first pair whose `old` ends here.
    pair: Option<u32>,
    /// Smallest pair index at or below this node (prunes `first` lookups).
    min_pair: u32,
}

impl Node {
    fn new() -> Self {
        Self {
            children: Vec::new(),
            pair: None,
            min_pair: u32::MAX,
        }
    }
}

/// Why `Matcher::replace` stopped.
enum Stop {
    TooLarge,
    Interrupted,
}

/// Work between deadline checks, in trie steps.
const CHECK_EVERY: usize = 1 << 16;

/// An immutable, shareable matcher.
struct Matcher {
    nodes: Vec<Node>,
    news: Vec<Box<[u8]>>,
    mode: Mode,
    bytes: usize,
    /// The counter this matcher was charged to; released on drop.
    counter: Arc<AtomicUsize>,
}

impl Drop for Matcher {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Incremental trie construction; `bytes` grows as pairs are added so a caller
/// can stop before an oversized dictionary is fully built.
struct Builder {
    nodes: Vec<Node>,
    news: Vec<Box<[u8]>>,
    bytes: usize,
}

impl Builder {
    fn new() -> Self {
        Self {
            nodes: vec![Node::new()],
            news: Vec::new(),
            bytes: size_of::<Node>(),
        }
    }

    fn add(&mut self, old: &[u8], new: &[u8]) {
        let i = self.news.len() as u32;
        let mut at = 0usize;
        self.nodes[0].min_pair = self.nodes[0].min_pair.min(i);
        for &b in old {
            at = match self.nodes[at].children.binary_search_by_key(&b, |c| c.0) {
                Ok(p) => self.nodes[at].children[p].1 as usize,
                Err(p) => {
                    let id = self.nodes.len();
                    self.nodes.push(Node::new());
                    self.nodes[at].children.insert(p, (b, id as u32));
                    self.bytes += size_of::<Node>() + 2 * size_of::<(u8, u32)>();
                    id
                }
            };
            self.nodes[at].min_pair = self.nodes[at].min_pair.min(i);
        }
        // A repeated `old` keeps the first pair.
        self.nodes[at].pair.get_or_insert(i);
        self.news.push(new.into());
        self.bytes += new.len() + size_of::<Box<[u8]>>();
    }

    fn finish(self, mode: Mode, counter: Arc<AtomicUsize>) -> Matcher {
        let bytes = estimate(&self.nodes, &self.news);
        Matcher {
            nodes: self.nodes,
            news: self.news,
            mode,
            bytes,
            counter,
        }
    }
}

impl Matcher {
    #[cfg(test)]
    fn build(pairs: &[(Vec<u8>, Vec<u8>)], mode: Mode, counter: Arc<AtomicUsize>) -> Self {
        let mut b = Builder::new();
        for (old, new) in pairs {
            b.add(old, new);
        }
        b.finish(mode, counter)
    }

    /// The `(pair, old_len)` that wins at the start of `text`, and the trie
    /// steps taken.
    fn lookup(&self, text: &[u8], skip_empty: bool) -> (Option<(u32, usize)>, usize) {
        let mut at = 0usize;
        let mut best: Option<(u32, usize)> = None;
        let mut depth = 0usize;
        loop {
            let node = &self.nodes[at];
            if let Some(p) = node.pair
                && !(depth == 0 && skip_empty)
            {
                match self.mode {
                    Mode::First => {
                        if best.is_none_or(|(b, _)| p < b) {
                            best = Some((p, depth));
                        }
                    }
                    Mode::Longest => best = Some((p, depth)),
                }
            }
            // Nothing deeper can list earlier than what we already have.
            if self.mode == Mode::First && best.is_some_and(|(b, _)| node.min_pair >= b) {
                break;
            }
            let Some(&b) = text.get(depth) else { break };
            match node.children.binary_search_by_key(&b, |c| c.0) {
                Ok(p) => at = node.children[p].1 as usize,
                Err(_) => break,
            }
            depth += 1;
        }
        (best, depth + 1)
    }

    /// `budget` caps the output length; `interrupted` is polled every
    /// [`CHECK_EVERY`] trie steps (native code does not see the VM deadline).
    fn replace(
        &self,
        text: &[u8],
        budget: usize,
        interrupted: impl Fn() -> bool,
    ) -> Result<Vec<u8>, Stop> {
        let mut out = Vec::with_capacity(text.len().min(budget));
        let (mut last, mut i, mut work) = (0usize, 0usize, 0usize);
        let mut prev_empty = false;
        while i <= text.len() {
            let (found, steps) = self.lookup(&text[i..], prev_empty);
            work += steps;
            if work >= CHECK_EVERY {
                work = 0;
                if interrupted() {
                    return Err(Stop::Interrupted);
                }
            }
            if let Some((p, len)) = found {
                let new = &self.news[p as usize];
                if out.len() + (i - last) + new.len() > budget {
                    return Err(Stop::TooLarge);
                }
                out.extend_from_slice(&text[last..i]);
                out.extend_from_slice(new);
                prev_empty = len == 0;
                i += len;
                last = i;
            } else {
                prev_empty = false;
                i += 1;
            }
        }
        let tail = &text[last.min(text.len())..];
        if out.len() + tail.len() > budget {
            return Err(Stop::TooLarge);
        }
        out.extend_from_slice(tail);
        Ok(out)
    }
}

fn estimate(nodes: &[Node], news: &[Box<[u8]>]) -> usize {
    let n: usize = nodes
        .iter()
        .map(|n| size_of::<Node>() + n.children.capacity() * size_of::<(u8, u32)>())
        .sum();
    n + news
        .iter()
        .map(|s| s.len() + size_of::<Box<[u8]>>())
        .sum::<usize>()
}

/// Process-wide registry of keyed replacers; holds only `Weak`, so a shared
/// replacer lives as long as some VM still references it.
#[derive(Debug)]
pub struct ReplacerStore {
    max_bytes: usize,
    shared_bytes: Arc<AtomicUsize>,
    keyed: Mutex<HashMap<String, Weak<Matcher>>>,
}

impl std::fmt::Debug for Matcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Matcher")
            .field("pairs", &self.news.len())
            .field("nodes", &self.nodes.len())
            .field("mode", &self.mode)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl Default for ReplacerStore {
    fn default() -> Self {
        Self::with_limit(DEFAULT_REPLACER_MAX_BYTES)
    }
}

impl ReplacerStore {
    /// `0` is unlimited.
    #[must_use]
    pub fn with_limit(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            shared_bytes: Arc::default(),
            keyed: Mutex::default(),
        }
    }
}

/// Per-VM live bytes of unkeyed replacers, kept in `lua` app data.
struct LocalBytes(Arc<AtomicUsize>);

struct Replacer(Arc<Matcher>, usize);

impl UserData for Replacer {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("replace", |lua, this, text: Value| {
            let text: mlua::LuaString = argcheck::arg(
                lua,
                text,
                argcheck::ArgSpec {
                    fname: "replacer:replace",
                    n: 1,
                    expected: "string",
                },
            )?;
            // Leave the VM's remaining memory budget as the output cap.
            let budget = match this.1 {
                0 => usize::MAX,
                limit => limit.saturating_sub(lua.used_memory()),
            };
            let out = this
                .0
                .replace(&text.as_bytes(), budget, || deadline_passed(lua))
                .map_err(|stop| match stop {
                    Stop::TooLarge => {
                        Error::runtime("replacer:replace: output would exceed the memory limit")
                    }
                    Stop::Interrupted => {
                        Error::runtime("replacer:replace: interrupted by the deadline")
                    }
                })?;
            lua.create_string(out)
        });
    }
}

fn text_of(lua: &Lua, value: Value, pos: &str, what: &str) -> mlua::Result<Vec<u8>> {
    let got = value.type_name();
    match value {
        Value::String(_) | Value::Integer(_) | Value::Number(_) => lua
            .coerce_string(value)?
            .map(|s| s.as_bytes().to_vec())
            .ok_or_else(|| Error::runtime("unreachable")),
        _ => Err(Error::runtime(format!(
            "lur.text.replacer: {pos} {what} must be string, got {got}"
        ))),
    }
}

const REPLACER: &str = "lur.text.replacer";

pub fn install(
    lua: &Lua,
    lur: &Table,
    store: Arc<ReplacerStore>,
    memory_limit: usize,
) -> Result<(), RunError> {
    let text = lua.create_table().map_err(RunError::Init)?;

    let replacer = lua
        .create_function(move |lua, (pairs, opts): (Value, Value)| {
            let pairs: Table = argcheck::arg(
                lua,
                pairs,
                argcheck::ArgSpec {
                    fname: REPLACER,
                    n: 1,
                    expected: "table",
                },
            )?;
            let ReplacerOpts { mode, key } = replacer_opts(opts)?;
            let request = Request {
                count: pairs.raw_len(),
                limit: store.max_bytes,
                pairs,
                mode,
            };
            let matcher = match key {
                Some(key) => keyed_matcher(lua, &store, &request, &key)?,
                None => local_matcher(lua, &request)?,
            };
            lua.create_userdata(Replacer(matcher, memory_limit))
        })
        .map_err(RunError::Init)?;
    text.set("replacer", replacer).map_err(RunError::Init)?;

    lur.set("text", text).map_err(RunError::Init)?;
    Ok(())
}

/// Parsed `opts` of `lur.text.replacer`.
struct ReplacerOpts {
    mode: Mode,
    key: Option<String>,
}

fn replacer_opts(opts: Value) -> mlua::Result<ReplacerOpts> {
    let mut parsed = ReplacerOpts {
        mode: Mode::First,
        key: None,
    };
    let t = match opts {
        Value::Nil => return Ok(parsed),
        Value::Table(t) => t,
        other => {
            return Err(Error::runtime(format!(
                "{REPLACER}: argument #2 must be table, got {}",
                other.type_name()
            )));
        }
    };
    for pair in t.pairs::<Value, Value>() {
        let (k, v) = pair?;
        let Value::String(k) = &k else {
            return Err(Error::runtime(format!(
                "{REPLACER}: opts keys must be strings, got {}",
                k.type_name()
            )));
        };
        match &*k.as_bytes() {
            b"match" => parsed.mode = match_opt(&v)?,
            b"key" => parsed.key = Some(key_opt(&v)?),
            other => {
                return Err(Error::runtime(format!(
                    "{REPLACER}: unknown option {:?}",
                    String::from_utf8_lossy(other)
                )));
            }
        }
    }
    Ok(parsed)
}

fn match_opt(v: &Value) -> mlua::Result<Mode> {
    match v {
        Value::String(s) if &*s.as_bytes() == b"first" => Ok(Mode::First),
        Value::String(s) if &*s.as_bytes() == b"longest" => Ok(Mode::Longest),
        _ => Err(Error::runtime(format!(
            "{REPLACER}: opts.match must be \"first\" or \"longest\""
        ))),
    }
}

fn key_opt(v: &Value) -> mlua::Result<String> {
    match v {
        Value::String(s) if !s.as_bytes().is_empty() => Ok(s.to_str()?.to_owned()),
        _ => Err(Error::runtime(format!(
            "{REPLACER}: opts.key must be a non-empty string"
        ))),
    }
}

/// What one `lur.text.replacer` call asks to build.
struct Request {
    pairs: Table,
    count: usize,
    mode: Mode,
    /// Byte cap for the counter the matcher is charged to; 0 = unlimited.
    limit: usize,
}

/// Streams the pairs into a trie and stops as soon as it would pass the cap.
fn build_matcher(
    lua: &Lua,
    request: &Request,
    counter: &Arc<AtomicUsize>,
) -> mlua::Result<Arc<Matcher>> {
    let Request {
        pairs,
        count,
        mode,
        limit,
    } = request;
    let (count, mode, limit) = (*count, *mode, *limit);
    let over = |extra: usize| limit != 0 && counter.load(Ordering::Relaxed) + extra > limit;
    let refuse = || {
        Error::runtime(format!(
            "{REPLACER}: replacer memory limit exceeded ({limit} bytes; raise --replacer-max-bytes)"
        ))
    };
    let mut builder = Builder::new();
    let mut collected = false;
    for i in 1..=count {
        let el: Value = pairs.raw_get(i)?;
        let Value::Table(el) = el else {
            return Err(Error::runtime(format!(
                "{REPLACER}: pairs[{i}] must be {{old, new}}, got {}",
                el.type_name()
            )));
        };
        if el.raw_len() != 2 {
            return Err(Error::runtime(format!(
                "{REPLACER}: pairs[{i}] must be {{old, new}}"
            )));
        }
        let pos = format!("pairs[{i}]");
        let old = text_of(lua, el.raw_get(1)?, &pos, "old")?;
        let new = text_of(lua, el.raw_get(2)?, &pos, "new")?;
        builder.add(&old, &new);
        if over(builder.bytes) {
            // Unreferenced replacers stay charged until collected.
            if collected {
                return Err(refuse());
            }
            collected = true;
            lua.gc_collect()?;
            lua.gc_collect()?;
            if over(builder.bytes) {
                return Err(refuse());
            }
        }
    }
    let m = builder.finish(mode, Arc::clone(counter));
    // Reserve; a refused matcher's Drop releases it again.
    counter.fetch_add(m.bytes, Ordering::Relaxed);
    if limit != 0 && counter.load(Ordering::Relaxed) > limit {
        return Err(refuse());
    }
    Ok(Arc::new(m))
}

/// A replacer shared by `key` across VMs: look up, build outside the lock,
/// then re-check before registering.
fn keyed_matcher(
    lua: &Lua,
    store: &ReplacerStore,
    request: &Request,
    key: &str,
) -> mlua::Result<Arc<Matcher>> {
    let found = live_matcher(
        &mut store.keyed.lock().expect("replacer store poisoned"),
        key,
    );
    if let Some(m) = found {
        check_key_shape(&m, key, request)?;
        return Ok(m);
    }
    let built = build_matcher(lua, request, &store.shared_bytes)?;
    let mut keyed = store.keyed.lock().expect("replacer store poisoned");
    if let Some(m) = live_matcher(&mut keyed, key) {
        check_key_shape(&m, key, request)?; // lost a race; drop ours
        return Ok(m);
    }
    keyed.insert(key.to_owned(), Arc::downgrade(&built));
    Ok(built)
}

fn live_matcher(keyed: &mut HashMap<String, Weak<Matcher>>, key: &str) -> Option<Arc<Matcher>> {
    keyed.retain(|_, w| w.strong_count() > 0);
    keyed.get(key).and_then(Weak::upgrade)
}

fn check_key_shape(m: &Matcher, key: &str, request: &Request) -> mlua::Result<()> {
    if m.mode != request.mode || m.news.len() != request.count {
        return Err(Error::runtime(format!(
            "{REPLACER}: key {key:?} is already used by a replacer with {} pairs and match = \"{}\"",
            m.news.len(),
            m.mode.name()
        )));
    }
    Ok(())
}

/// A replacer private to this VM, charged to the VM's own byte counter.
fn local_matcher(lua: &Lua, request: &Request) -> mlua::Result<Arc<Matcher>> {
    let local = if let Some(l) = lua.app_data_ref::<LocalBytes>() {
        Arc::clone(&l.0)
    } else {
        let c = Arc::new(AtomicUsize::new(0));
        lua.set_app_data(LocalBytes(Arc::clone(&c)));
        c
    };
    build_matcher(lua, request, &local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(pairs: &[(&str, &str)], mode: Mode) -> Matcher {
        let p: Vec<(Vec<u8>, Vec<u8>)> = pairs
            .iter()
            .map(|(a, b)| (a.as_bytes().to_vec(), b.as_bytes().to_vec()))
            .collect();
        Matcher::build(&p, mode, Arc::default())
    }

    fn run(pairs: &[(&str, &str)], mode: Mode, s: &str) -> String {
        String::from_utf8(
            rep(pairs, mode)
                .replace(s.as_bytes(), usize::MAX, || false)
                .ok()
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn go_vectors_first() {
        let f = Mode::First;
        assert_eq!(run(&[("a", "1"), ("a", "2")], f, "aa"), "11");
        assert_eq!(run(&[("a", "1"), ("aa", "2")], f, "aaa"), "111");
        assert_eq!(run(&[("aa", "2"), ("a", "1")], f, "aaa"), "21");
        assert_eq!(run(&[("", "X")], f, "ab"), "XaXbX");
        assert_eq!(run(&[("", "X")], f, ""), "X");
        assert_eq!(run(&[("a", "b"), ("b", "a")], f, "abba"), "baab");
        // empty is listed first, so it wins, then "a" matches at the same spot
        assert_eq!(run(&[("", "X"), ("a", "1")], f, "a"), "X1X");
    }

    #[test]
    fn longest_equals_first_over_length_sorted_pairs() {
        let pairs = [("a", "1"), ("ab", "2"), ("abc", "3"), ("b", "4"), ("", "-")];
        let mut sorted = pairs.to_vec();
        sorted.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
        for s in ["", "a", "abc", "abcab", "xabcx", "bbab"] {
            assert_eq!(
                run(&pairs, Mode::Longest, s),
                run(&sorted, Mode::First, s),
                "{s:?}"
            );
        }
    }

    #[test]
    fn longest_empty_old_only_when_nothing_else_matches() {
        assert_eq!(run(&[("", "-"), ("ab", "X")], Mode::Longest, "abc"), "X-c-");
    }

    #[test]
    fn invalid_utf8_passes_through() {
        let m = rep(&[("a", "b")], Mode::First);
        assert_eq!(
            m.replace(b"\xFFa\xFE", usize::MAX, || false).ok().unwrap(),
            b"\xFFb\xFE"
        );
    }
}
