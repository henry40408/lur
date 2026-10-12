//! The Lua examples in docs/GUIDE.md ARE the test suite. Each fenced lua block is
//! run under a permissive sandbox; lua-ignore blocks are shown but skipped, and
//! lua-serve blocks are loaded together as one server app and driven with requests.

use std::sync::Arc;

use lur::policy::Policy;
use lur::runtime::{Runtime, RuntimeConfig};

const GUIDE: &str = include_str!("../docs/GUIDE.md");

/// A fenced lua block and whether it is marked `ignore` or `serve`.
struct Block {
    code: String,
    ignore: bool,
    serve: bool,
}

/// Fenced lua blocks in raw Markdown: `lua` runs, `lua ignore` is skipped,
/// `lua serve` joins the server app exercised by `serve_examples_answer_requests`.
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
        // Catch ```Lua, ```lua title=x etc. before they silently escape the suite.
        assert!(
            matches!(info, "lua" | "lua ignore" | "lua serve"),
            "unrecognised lua fence info string {info:?} — use `lua`, `lua ignore` or `lua serve`"
        );
        let ignore = info == "lua ignore";
        let serve = info == "lua serve";
        let mut code = String::new();
        for body in lines.by_ref() {
            if body.trim_start().starts_with("```") {
                break;
            }
            code.push_str(body);
            code.push('\n');
        }
        blocks.push(Block {
            code,
            ignore,
            serve,
        });
    }
    blocks
}

/// Loose profile (full fs/env/net) with a temp db.
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
        if block.ignore || block.serve {
            continue;
        }
        // Each block gets its own temp dir (cwd for relative fs paths) + db.
        let dir = tempfile::tempdir().expect("tempdir");
        // SAFETY: cwd is process-global; nextest runs each test in its own process.
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

/// The `lua serve` blocks form one app: load it and send requests covering
/// the documented statuses.
#[test]
fn serve_examples_answer_requests() {
    use lur::serve::{RawRequest, Response, Server};

    let app: String = lua_blocks(GUIDE)
        .iter()
        .filter(|b| b.serve)
        .map(|b| b.code.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!app.is_empty(), "no `lua serve` blocks in the guide");
    let server = Server::load(
        &app,
        RuntimeConfig {
            pool_size: 1,
            ..Default::default()
        },
    )
    .expect("serve examples load as one app");

    let req =
        |method: &str, path: &str, query: &str, ctype: Option<&str>, body: &str| -> Response {
            server
                .dispatch_raw(&RawRequest {
                    method: method.into(),
                    path: path.into(),
                    query: query.into(),
                    headers: ctype
                        .map(|c| vec![("content-type".to_owned(), c.to_owned())])
                        .unwrap_or_default(),
                    body: body.as_bytes().to_vec(),
                })
                .unwrap_or_else(|e| panic!("{method} {path} failed: {e}"))
        };
    let text = |r: &Response| String::from_utf8_lossy(&r.body).into_owned();
    let json = Some("application/json");
    let form = Some("application/x-www-form-urlencoded");

    let r = req("POST", "/echo", "", json, r#"{"a":1}"#);
    assert_eq!((r.status, text(&r).as_str()), (200, r#"{"a":1}"#));
    let cookies = r.headers.iter().filter(|(n, _)| n == "set-cookie").count();
    assert_eq!(cookies, 2, "array header value repeats Set-Cookie");

    assert_eq!(text(&req("GET", "/feed", "", None, "")), "tag: all");
    assert_eq!(text(&req("GET", "/feed/lua", "", None, "")), "tag: lua");

    assert_eq!(req("GET", "/items/5", "", None, "").status, 200);
    assert_eq!(req("GET", "/items/2000", "", None, "").status, 410);
    assert_eq!(req("GET", "/items/abc", "", None, "").status, 404);

    let r = req("POST", "/search", "", form, "q=luau&tag=a&tag=b");
    assert_eq!((r.status, text(&r).as_str()), (200, "luau 2"));

    assert_eq!(text(&req("GET", "/users/7", "", None, "")), "user 7");
    assert_eq!(req("GET", "/users/0", "", None, "").status, 404);
    assert_eq!(
        req("GET", "/users", "limit=500&tag=a", None, "").status,
        400
    );
    assert_eq!(req("GET", "/users", "limit=5", None, "").status, 422);
    assert_eq!(req("GET", "/users", "tag=a", None, "").status, 200);

    let ok = r#"{"email":"a@b.c","age":20}"#;
    assert_eq!(req("POST", "/signup", "", json, ok).status, 201);
    assert_eq!(
        req("POST", "/signup", "", Some("text/plain"), ok).status,
        415
    );
    assert_eq!(req("POST", "/signup", "", json, "{").status, 400);
    let bad = r#"{"email":"a","age":5}"#;
    assert_eq!(req("POST", "/signup", "", json, bad).status, 422);
}

/// Writes every function's dotted path in `lur` (e.g. `crypto.hex.encode`) to
/// `./__lur_fns.txt`, one per line.
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

/// Every function the runtime exposes — what the guide must have examples for.
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

/// `lur.<path>` functions *called* (followed by `(`) in any guide block;
/// data fields like `lur.args.positional[1]` are excluded.
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
    // Functions intentionally without an example.
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
        "http", "db", "kv", "async", "serve", "url", "charset", "html", "xml", "feed", "validate",
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
