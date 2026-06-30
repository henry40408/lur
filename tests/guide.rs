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
        if info != "lua" && info != "lua ignore" {
            continue;
        }
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
    assert!(!blocks.is_empty(), "no ```lua blocks found in the guide");
    for (i, block) in blocks.iter().enumerate() {
        if block.ignore {
            continue;
        }
        // Each block gets its own temp dir (cwd for relative fs paths) + db.
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_current_dir(dir.path()).expect("chdir");
        let rt = Runtime::with_config(permissive_config(dir.path().join("guide.db")))
            .expect("runtime builds");
        if let Err(e) = rt.run(&block.code) {
            panic!(
                "guide example #{i} failed: {e}\n--- block ---\n{}",
                block.code
            );
        }
    }
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
