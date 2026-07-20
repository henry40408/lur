//! The Lua examples in docs/GUIDE.md ARE the test suite. Each fenced lua block is
//! run under a permissive sandbox; lua-ignore blocks are shown but skipped.

use std::sync::Arc;

use lur::policy::Policy;
use lur::runtime::{Runtime, RuntimeConfig};

const GUIDE: &str = include_str!("../docs/GUIDE.md");

/// A fenced lua block and whether it is marked `ignore`.
struct Block {
    code: String,
    ignore: bool,
}

/// Scan raw Markdown for fenced lua blocks. The info string after `lua` selects
/// behavior: empty → runnable, `ignore` → skipped.
fn lua_blocks(md: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines = md.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let Some(info) = trimmed.strip_prefix("```") else {
            continue;
        };
        let info = info.trim();
        let info_lower = info.to_ascii_lowercase();
        if !info_lower.starts_with("lua") {
            continue;
        }
        // Catch typos like ```Lua, ```lua title=x before they silently escape the suite.
        assert!(
            !(info != "lua" && info != "lua ignore"),
            "unrecognised lua fence info string {info:?} — use `lua` or `lua ignore`"
        );
        let ignore = info == "lua ignore";
        let mut code = String::new();
        for body in lines.by_ref() {
            if body.trim_start().starts_with("```") {
                break;
            }
            code.push_str(body);
            code.push('\n');
        }
        blocks.push(Block { code, ignore });
    }
    blocks
}

/// A permissive-but-sandboxed config: full fs/env/net (loose), with a temp db.
fn permissive_config(db_path: std::path::PathBuf) -> RuntimeConfig {
    RuntimeConfig {
        policy: Arc::new(Policy::loose().expect("loose policy")),
        db_path: Some(db_path),
        ..Default::default()
    }
}

#[test]
fn every_runnable_example_succeeds() {
    let blocks = lua_blocks(GUIDE);
    let mut ran: usize = 0;
    for (i, block) in blocks.iter().enumerate() {
        if block.ignore {
            continue;
        }
        // Each block gets its own temp dir (cwd for relative fs paths) + db.
        let dir = tempfile::tempdir().expect("tempdir");
        // SAFETY: set_current_dir is process-global; safe only because nextest runs each test in its own process.
        std::env::set_current_dir(dir.path()).expect("chdir");
        let rt = Runtime::with_config(permissive_config(dir.path().join("guide.db")))
            .expect("runtime builds");
        if let Err(e) = rt.run(&block.code) {
            panic!(
                "guide example #{i} failed: {e}\n--- block ---\n{}",
                block.code
            );
        }
        ran += 1;
    }
    assert!(
        ran >= 11,
        "expected at least 11 runnable guide examples, but only ran {ran} — did examples get mistagged or removed?"
    );
}

/// Lua that walks the live `lur` table and writes every function's dotted path
/// (e.g. `crypto.hex.encode`) to `./__lur_fns.txt`, one per line.
const REFLECT_LUA: &str = r#"
local names = {}
local function walk(prefix, t, depth)
  if depth > 4 then return end
  for k, v in pairs(t) do
    if type(k) == "string" then
      local path = prefix .. k
      local ty = type(v)
      if ty == "function" then
        names[#names + 1] = path
      elseif ty == "table" then
        walk(path .. ".", v, depth + 1)
      end
    end
  end
end
walk("", lur, 0)
table.sort(names)
lur.fs.write("./__lur_fns.txt", table.concat(names, "\n"))
"#;

/// Reflect the runtime `lur` table into the set of function dotted-paths it
/// exposes (the authoritative list of what the guide must example).
fn runtime_functions() -> Vec<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    // SAFETY: process-global cwd; safe only because nextest isolates each test.
    std::env::set_current_dir(dir.path()).expect("chdir");
    let rt = Runtime::with_config(permissive_config(dir.path().join("reflect.db")))
        .expect("runtime builds");
    rt.run(REFLECT_LUA).expect("reflection script runs");
    std::fs::read_to_string(dir.path().join("__lur_fns.txt"))
        .expect("read reflected function list")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The set of `lur.<dotted.path>` functions actually *called* (followed by `(`)
/// across all guide code blocks (runnable and `ignore`). Indexing like
/// `lur.args.positional[1]` is not a call, so data fields are excluded.
fn called_functions(blocks: &[Block]) -> std::collections::HashSet<String> {
    let mut called = std::collections::HashSet::new();
    for b in blocks {
        let bytes = b.code.as_bytes();
        let mut i = 0;
        while let Some(rel) = b.code[i..].find("lur.") {
            let start = i + rel + 4;
            let mut j = start;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'.')
            {
                j += 1;
            }
            let path = b.code[start..j].trim_end_matches('.');
            if !path.is_empty() && j < bytes.len() && bytes[j] == b'(' {
                called.insert(path.to_string());
            }
            i = j.max(start);
        }
    }
    called
}

#[test]
fn every_runtime_function_has_an_example() {
    let funcs = runtime_functions();
    assert!(!funcs.is_empty(), "reflection found no lur functions");
    let called = called_functions(&lua_blocks(GUIDE));
    // Functions intentionally without a worked example (none today).
    const EXCEPTIONS: &[&str] = &[];
    let mut missing: Vec<String> = funcs
        .into_iter()
        .filter(|f| !called.contains(f) && !EXCEPTIONS.contains(&f.as_str()))
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "lur functions with no example in the guide: {missing:?}"
    );
}

#[test]
fn every_capability_is_documented() {
    const CAPS: &[&str] = &[
        "json", "base64", "crypto", "cookie", "time", "log", "args", "state", "io", "fs", "env",
        "http", "db", "kv", "async", "serve",
    ];
    let missing: Vec<&str> = CAPS
        .iter()
        .copied()
        .filter(|c| {
            // `io` is documented as lur.stdin / lur.stdout.
            if *c == "io" {
                return !GUIDE.contains("lur.stdin") && !GUIDE.contains("lur.stdout");
            }
            !GUIDE.contains(&format!("lur.{c}"))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "capabilities missing from the guide: {missing:?}"
    );
}
