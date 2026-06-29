# req.cookies Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose parsed cookies on the server request table as `req.cookies`, sharing one parser with `lur.cookie.parse`.

**Architecture:** Extract the lenient cookie-splitting logic from the `lur.cookie.parse` closure into a pure crate-internal `cookie_pairs(&[u8]) -> Vec<(&[u8], &[u8])>` in `src/capabilities/cookie.rs`. `lur.cookie.parse` and the server's `build_request_table` both build their tables from it, so the two can never diverge. `req.cookies` is parsed eagerly alongside `headers` and is always a table.

**Tech Stack:** Rust (edition 2024), mlua 0.11 (Luau), existing `tests/serve.rs`/`tests/capabilities.rs` harnesses.

## Global Constraints

- One parser: `cookie_pairs` is the single source of truth; `lur.cookie.parse` behaviour is unchanged (the existing `cookie_parse_*` Lua tests must stay green).
- `req.cookies` is **eager** (built with `headers`) and **always a table** — an absent/empty `Cookie` header yields an empty table, never `nil`.
- **Multiple `Cookie` headers merge** into one table; on a duplicate cookie name the later occurrence wins. Cookie names are **case-sensitive** (not lower-cased, unlike header names). Values are **verbatim** (no decoding).
- Cron context has no HTTP request and gains no `cookies` field.
- Pure unit tests (`#[cfg(test)]`) for `cookie_pairs` in `cookie.rs` are intended (it is a pure Rust function, not the Lua-tested capability surface).
- CI gates: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` (NOT `cargo test`). Run `cargo fmt --all` before committing.
- Commits GPG-signed (`git commit -S`). Stage files explicitly by name — NEVER `git add -A`/`.`.

---

## File Structure

- **Modify** `src/capabilities/cookie.rs` — add `pub(crate) fn cookie_pairs`, refactor `install_parse` to use it, add a `#[cfg(test)] mod tests` for `cookie_pairs`.
- **Modify** `src/serve.rs` — in `build_request_table`, after the `headers` block, build and set the `cookies` table.
- **Modify** `tests/serve.rs` — add `req_cookies_*` integration tests.
- **Modify** `README.md` and `ARCHITECTURE.md` — add `cookies` to the `req` field lists.

---

### Task 1: Extract `cookie_pairs` and refactor `lur.cookie.parse`

**Files:**
- Modify: `src/capabilities/cookie.rs`
- Test: `src/capabilities/cookie.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: the existing `trim_ows(&[u8]) -> &[u8]` helper.
- Produces: `pub(crate) fn cookie_pairs(header: &[u8]) -> Vec<(&[u8], &[u8])>` — lenient (name, value) byte pairs (split `;`, trim OWS, split first `=`, skip no-`=`/empty-name, value verbatim, duplicates preserved in order). Task 2 (serve) consumes this.

- [ ] **Step 1: Write the failing unit tests**

Add this test module at the end of `src/capabilities/cookie.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::cookie_pairs;

    #[test]
    fn cookie_pairs_basic_and_multiple() {
        assert_eq!(cookie_pairs(b"sid=abc"), vec![(&b"sid"[..], &b"abc"[..])]);
        assert_eq!(
            cookie_pairs(b"sid=abc; theme=dark"),
            vec![(&b"sid"[..], &b"abc"[..]), (&b"theme"[..], &b"dark"[..])]
        );
    }

    #[test]
    fn cookie_pairs_trims_ows_including_tabs() {
        assert_eq!(
            cookie_pairs(b"  a=1 ;\tb=2\t"),
            vec![(&b"a"[..], &b"1"[..]), (&b"b"[..], &b"2"[..])]
        );
    }

    #[test]
    fn cookie_pairs_is_lenient() {
        assert_eq!(cookie_pairs(b""), Vec::<(&[u8], &[u8])>::new());
        assert_eq!(cookie_pairs(b"garbage; x=1"), vec![(&b"x"[..], &b"1"[..])]);
        assert_eq!(cookie_pairs(b"=noname; y=2"), vec![(&b"y"[..], &b"2"[..])]);
    }

    #[test]
    fn cookie_pairs_keeps_inner_equals_and_duplicates() {
        assert_eq!(cookie_pairs(b"t=a=b"), vec![(&b"t"[..], &b"a=b"[..])]);
        assert_eq!(
            cookie_pairs(b"k=1; k=2"),
            vec![(&b"k"[..], &b"1"[..]), (&b"k"[..], &b"2"[..])]
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(cookie_pairs)'`
Expected: FAIL to compile / `cannot find function cookie_pairs`.

- [ ] **Step 3: Add `cookie_pairs` and refactor `install_parse`**

In `src/capabilities/cookie.rs`, add this function immediately after `trim_ows`:

```rust
/// Split a `Cookie` header value into (name, value) byte pairs using the
/// lenient rules: split on `;`, trim OWS per segment, split on the first `=`,
/// skip a segment with no `=` or an empty name. Values are verbatim. Borrows
/// from the input; duplicate names are preserved in order (a caller building a
/// map collapses them, later-wins).
pub(crate) fn cookie_pairs(header: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut pairs = Vec::new();
    for segment in header.split(|&b| b == b';') {
        let segment = trim_ows(segment);
        let Some(eq) = segment.iter().position(|&b| b == b'=') else {
            continue;
        };
        let name = &segment[..eq];
        if name.is_empty() {
            continue;
        }
        pairs.push((name, &segment[eq + 1..]));
    }
    pairs
}
```

Then replace the body of `install_parse`'s closure so it builds the table from `cookie_pairs`:

```rust
/// `lur.cookie.parse(header) -> { name = value, ... }`.
fn install_parse(lua: &Lua, cookie: &Table) -> Result<(), RunError> {
    let parse = lua
        .create_function(|lua, header: mlua::String| {
            let out = lua.create_table()?;
            let bytes = header.as_bytes();
            // Later duplicate overwrites earlier: plain table assignment.
            for (name, value) in cookie_pairs(&bytes) {
                out.set(lua.create_string(name)?, lua.create_string(value)?)?;
            }
            Ok(out)
        })
        .map_err(RunError::Init)?;
    cookie.set("parse", parse).map_err(RunError::Init)?;
    Ok(())
}
```

- [ ] **Step 4: Run the unit tests and the existing parse tests (expect PASS) + lint**

Run: `cargo fmt --all && cargo nextest run -E 'test(cookie)' && cargo clippy --all-targets -- -D warnings`
Expected: the 4 `cookie_pairs_*` unit tests PASS and the existing `cookie_parse_*` Lua tests still PASS (behaviour preserved); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/cookie.rs
git commit -S -m "refactor: extract cookie_pairs as the shared cookie parser"
```

---

### Task 2: Add `req.cookies` to the server request + docs

**Files:**
- Modify: `src/serve.rs` (`build_request_table`, after the `headers` block — currently `table.set("headers", headers)?;`)
- Modify: `tests/serve.rs`
- Modify: `README.md`, `ARCHITECTURE.md`

**Interfaces:**
- Consumes: `crate::capabilities::cookie::cookie_pairs` from Task 1; `RawRequest.headers: Vec<(String, String)>`.
- Produces: `req.cookies` (a Lua table of cookie name→value) on every HTTP request table.

- [ ] **Step 1: Write the failing integration tests**

Append to `tests/serve.rs`:

```rust
#[test]
fn req_cookies_parses_the_cookie_header() {
    let s = serve(
        "lur.serve.http('GET', '/c', function(req)\n\
         \treturn { body = (req.cookies.sid or '?') .. '|' .. (req.cookies.theme or '?') } end)",
    );
    let mut req = request("GET", "/c", "");
    req.headers = vec![("Cookie".to_owned(), "sid=abc; theme=dark".to_owned())];
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"abc|dark");
}

#[test]
fn req_cookies_is_empty_table_when_absent() {
    let s = serve(
        "lur.serve.http('GET', '/c', function(req)\n\
         \treturn { body = (next(req.cookies) == nil) and 'empty' or 'nonempty' } end)",
    );
    let resp = s.dispatch("GET", "/c", b"").expect("dispatch ok");
    assert_eq!(resp.body, b"empty");
}

#[test]
fn req_cookies_merges_multiple_headers_later_wins() {
    let s = serve(
        "lur.serve.http('GET', '/c', function(req)\n\
         \treturn { body = req.cookies.a .. '|' .. req.cookies.b } end)",
    );
    let mut req = request("GET", "/c", "");
    req.headers = vec![
        ("Cookie".to_owned(), "a=1; b=2".to_owned()),
        ("Cookie".to_owned(), "b=3".to_owned()),
    ];
    assert_eq!(s.dispatch_raw(&req).unwrap().body, b"1|3");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(req_cookies)'`
Expected: FAIL — `req.cookies` is `nil`, so the handlers error or compare wrong (e.g. attempt to index a nil value / body mismatch).

- [ ] **Step 3: Build the `cookies` table in `build_request_table`**

In `src/serve.rs`, immediately after the headers block (the line `table.set("headers", headers)?;`), insert:

```rust
    // Cookies: parse every `Cookie` header into one table (later value wins),
    // sharing the lenient parser with `lur.cookie.parse`. Always a table — an
    // absent or empty header yields an empty `req.cookies`, never `nil`. Cookie
    // names are case-sensitive, so (unlike header names) they are not altered.
    let cookies = lua.create_table()?;
    for (name, value) in &req.headers {
        if name.eq_ignore_ascii_case("cookie") {
            for (cname, cvalue) in crate::capabilities::cookie::cookie_pairs(value.as_bytes()) {
                cookies.set(lua.create_string(cname)?, lua.create_string(cvalue)?)?;
            }
        }
    }
    table.set("cookies", cookies)?;
```

- [ ] **Step 4: Run the integration tests (expect PASS) + full suite + lint**

Run: `cargo fmt --all && cargo nextest run && cargo clippy --all-targets -- -D warnings`
Expected: the 3 `req_cookies_*` tests PASS; full suite green; clippy clean.

- [ ] **Step 5: Update the docs**

In `README.md`, the `req` field sentence (currently):

```
The `req` object exposes `method`, `path`, `params`, `query` (last value per key),
`query_all` (all values), `headers` (lowercased), `body` (raw bytes), and `json()`.
```

becomes:

```
The `req` object exposes `method`, `path`, `params`, `query` (last value per key),
`query_all` (all values), `headers` (lowercased), `cookies` (parsed `Cookie`
header; empty table when absent), `body` (raw bytes), and `json()`.
```

In `ARCHITECTURE.md`, the request-lifecycle `build_req` field list (currently):

```
`build_req` (sets `method`, `path`, `params`, `query`/`query_all`,
   `headers`, `body`, the streaming `read`, and `json()`)
```

becomes (insert `cookies` after `headers`):

```
`build_req` (sets `method`, `path`, `params`, `query`/`query_all`,
   `headers`, `cookies`, `body`, the streaming `read`, and `json()`)
```

- [ ] **Step 6: Commit**

```bash
git add src/serve.rs tests/serve.rs README.md ARCHITECTURE.md
git commit -S -m "feat(serve): expose parsed cookies as req.cookies"
```

---

## Self-Review

**Spec coverage:**
- Shared `cookie_pairs` single source of truth → Task 1. ✓
- `lur.cookie.parse` behaviour unchanged (existing tests green) → Task 1 Step 4. ✓
- `req.cookies` eager, always a table, multi-header merge later-wins, verbatim → Task 2 Step 3 + tests. ✓
- Cron unaffected (only HTTP `build_request_table` touched) → Task 2 scope. ✓
- Docs (README + ARCHITECTURE req field lists) → Task 2 Step 5. ✓

**Placeholder scan:** No TBD/TODO; every code step shows complete code. ✓

**Type consistency:** `cookie_pairs(&[u8]) -> Vec<(&[u8], &[u8])>` defined in Task 1 and called as `crate::capabilities::cookie::cookie_pairs(value.as_bytes())` in Task 2 — signature matches (`value.as_bytes()` on a `String` yields `&[u8]`). `req.headers: Vec<(String, String)>` iterated as `&req.headers` → `(&String, &String)`; `name.eq_ignore_ascii_case("cookie")` and `value.as_bytes()` are valid on `String`. ✓

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks.
2. **Inline Execution** — execute in this session with checkpoints.
