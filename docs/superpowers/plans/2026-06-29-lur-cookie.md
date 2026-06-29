# lur.cookie Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a pure-compute `lur.cookie` capability with `parse` (Cookie request header → name→value table) and `serialize` (name/value/opts → one Set-Cookie value).

**Architecture:** A new `src/capabilities/cookie.rs` submodule following the exact shape of `src/capabilities/base64.rs` and `crypto.rs`: an `install(lua, lur)` that registers a flat `lur.cookie` table holding two `create_function` closures. Wired into `capabilities::install` immediately after `crypto`. Byte-oriented string handling throughout (cookie names/values are byte strings); no new crates.

**Tech Stack:** Rust (edition 2024), mlua 0.11 (Luau), existing test harness `tests/capabilities.rs`.

## Global Constraints

- Pure-compute capability — **not** policy-gated; no `Arc<Policy>` argument. Installed in `capabilities::install` immediately after `crypto::install`, before `sandbox(true)`.
- **Zero new dependencies.** Pure Rust byte-string parsing/assembly only.
- Errors raise a Lua runtime error of the form `lur.cookie.<fn>: <detail>` via `mlua::Error::runtime(...)`, catchable with `pcall`.
- **Raw bytes, no automatic encoding** — `serialize` does not percent-encode the value; `parse` does not decode. Values are taken verbatim.
- `serialize` emits attributes in this **fixed order**: `Domain`, `Path`, `Max-Age`, `Expires`, `HttpOnly`, `Secure`, `SameSite`.
- **SameSite=None guard:** if `same_site` canonicalizes to `"None"` but `secure` is not `true`, `serialize` raises (it does not silently add `Secure`).
- Tests live in `tests/capabilities.rs` using the existing `run(src)` helper (matches the `crypto`/`base64` convention — capability modules carry no inline `#[cfg(test)]`).
- CI gates: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` must all pass. Run `cargo fmt --all` before every commit.
- All commits GPG-signed (`git commit -S`). Stage files explicitly by name (never `git add -A`/`.`).

---

## File Structure

- **Create** `src/capabilities/cookie.rs` — the whole capability (install + `parse` + `serialize` + private validation helpers). One focused file, ~150 lines, mirroring `crypto.rs`.
- **Modify** `src/capabilities/mod.rs` — declare `pub mod cookie;` and call `cookie::install(lua, &lur)?;` after the `crypto::install` line.
- **Modify** `tests/capabilities.rs` — add `cookie_parse_*` and `cookie_serialize_*` test functions.
- **Modify** `README.md` — add a `lur.cookie` bullet to the "Data & I/O" section.
- **Modify** `ARCHITECTURE.md` — add `cookie` to the capability-order line.

---

### Task 1: `lur.cookie.parse` + module scaffold + wiring

**Files:**
- Create: `src/capabilities/cookie.rs`
- Modify: `src/capabilities/mod.rs:8-9` (module decl) and `:40` (install call)
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: `crate::runtime::RunError`; `mlua::{Error, Lua, Table, Value}`; the `RunError::Init` mapping idiom from `base64.rs`/`crypto.rs`.
- Produces: `pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError>` registering `lur.cookie`; `lur.cookie.parse(header: string) -> table` mapping cookie names to values (both Lua strings). Lenient: segments without `=` or with an empty name are skipped; empty input yields an empty table; on a duplicate name the later value wins; values are verbatim.

- [ ] **Step 1: Create the module file with `install` + `parse` + the OWS-trim helper**

Create `src/capabilities/cookie.rs`:

```rust
//! `lur.cookie` — parse the `Cookie` request header and build `Set-Cookie`
//! values. Pure-compute capability, no policy gate, in the spirit of
//! `lur.base64`/`lur.crypto`: raw bytes in, raw bytes out, no automatic
//! percent-encoding. `serialize` validates its inputs so a malformed cookie
//! fails loudly rather than corrupting the response.

use mlua::{Error, Lua, Table, Value};

use crate::runtime::RunError;

/// Install the flat `lur.cookie` table (`parse` + `serialize`).
pub fn install(lua: &Lua, lur: &Table) -> Result<(), RunError> {
    let cookie = lua.create_table().map_err(RunError::Init)?;

    install_parse(lua, &cookie)?;
    install_serialize(lua, &cookie)?;

    lur.set("cookie", cookie).map_err(RunError::Init)?;
    Ok(())
}

/// Trim leading/trailing optional whitespace (space / tab) from a byte slice.
fn trim_ows(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if *first == b' ' || *first == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if *last == b' ' || *last == b'\t' {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// `lur.cookie.parse(header) -> { name = value, ... }`.
fn install_parse(lua: &Lua, cookie: &Table) -> Result<(), RunError> {
    let parse = lua
        .create_function(|lua, header: mlua::String| {
            let out = lua.create_table()?;
            let bytes = header.as_bytes();
            for segment in bytes.split(|&b| b == b';') {
                let segment = trim_ows(segment);
                let Some(eq) = segment.iter().position(|&b| b == b'=') else {
                    continue; // no '=' -> skip
                };
                let name = &segment[..eq];
                if name.is_empty() {
                    continue; // empty name -> skip
                }
                let value = &segment[eq + 1..];
                // Later duplicate overwrites earlier: plain table assignment.
                out.set(lua.create_string(name)?, lua.create_string(value)?)?;
            }
            Ok(out)
        })
        .map_err(RunError::Init)?;
    cookie.set("parse", parse).map_err(RunError::Init)?;
    Ok(())
}
```

Add a temporary stub so the file compiles before Task 2 implements it:

```rust
/// `lur.cookie.serialize(name, value, opts?) -> string` (implemented in Task 2).
fn install_serialize(_lua: &Lua, _cookie: &Table) -> Result<(), RunError> {
    Ok(())
}
```

- [ ] **Step 2: Wire the module into `capabilities::install`**

In `src/capabilities/mod.rs`, add the module declaration alphabetically near the other `pub mod` lines (after `pub mod base64;`):

```rust
pub mod cookie;
```

And add the install call immediately after the `crypto::install` line (currently line 40):

```rust
    crypto::install(lua, &lur)?;
    cookie::install(lua, &lur)?;
```

- [ ] **Step 3: Add the parse tests**

Append to `tests/capabilities.rs`:

```rust
#[test]
fn cookie_parse_basic_and_multiple() {
    run("local c = lur.cookie.parse('sid=abc; theme=dark')\n\
         assert(c.sid == 'abc', 'sid')\n\
         assert(c.theme == 'dark', 'theme')");
}

#[test]
fn cookie_parse_trims_whitespace_including_tabs() {
    run("local c = lur.cookie.parse('  a=1 ;' .. string.char(9) .. 'b=2' .. string.char(9))\n\
         assert(c.a == '1', 'a trimmed')\n\
         assert(c.b == '2', 'b trimmed')");
}

#[test]
fn cookie_parse_is_lenient_and_keeps_inner_equals() {
    run("assert(next(lur.cookie.parse('')) == nil, 'empty -> empty table')\n\
         local c = lur.cookie.parse('garbage; x=1')\n\
         assert(c.x == '1' and c.garbage == nil, 'segment without = is skipped')\n\
         assert(lur.cookie.parse('=novalue; y=2').y == '2', 'empty name skipped')\n\
         assert(lur.cookie.parse('k=1; k=2').k == '2', 'later duplicate wins')\n\
         assert(lur.cookie.parse('t=a=b').t == 'a=b', 'value keeps inner =')");
}
```

- [ ] **Step 4: Run the parse tests (expect PASS) and the lint gate**

Run: `cargo fmt --all && cargo nextest run -E 'test(cookie_parse)' && cargo clippy --all-targets -- -D warnings`
Expected: 3 `cookie_parse_*` tests PASS; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/cookie.rs src/capabilities/mod.rs tests/capabilities.rs
git commit -S -m "feat: add lur.cookie.parse"
```

---

### Task 2: `lur.cookie.serialize` + validation + SameSite guard

**Files:**
- Modify: `src/capabilities/cookie.rs` (replace the `install_serialize` stub; add validation helpers)
- Test: `tests/capabilities.rs`

**Interfaces:**
- Consumes: the module and imports from Task 1 (`mlua::{Error, Lua, Table, Value}`).
- Produces: `lur.cookie.serialize(name: string, value: string, opts?: table) -> string`. Returns the `Set-Cookie` value (no `Set-Cookie:` prefix). `opts` fields: `domain`/`path`/`expires` (string), `max_age` (integer), `secure`/`http_only` (boolean), `same_site` (`"Strict"`/`"Lax"`/`"None"`, case-insensitive). Raises on: invalid name token, value containing control/`;`/CR/LF, non-integer `max_age`, unknown `same_site`, and `same_site="None"` without `secure=true`.

- [ ] **Step 1: Add the validation helpers**

In `src/capabilities/cookie.rs`, add these private helpers (place above `install_serialize`):

```rust
/// RFC 6265 cookie-name separator characters (a name is a token: no controls,
/// no separators, no space/tab).
fn is_separator(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')'
            | b'<'
            | b'>'
            | b'@'
            | b','
            | b';'
            | b':'
            | b'\\'
            | b'"'
            | b'/'
            | b'['
            | b']'
            | b'?'
            | b'='
            | b'{'
            | b'}'
    )
}

/// Validate a cookie name: non-empty, all bytes are token characters
/// (visible ASCII `0x21..=0x7e`, excluding separators).
fn validate_name(name: &[u8]) -> Result<(), Error> {
    if name.is_empty() {
        return Err(Error::runtime("lur.cookie.serialize: name must not be empty"));
    }
    if !name
        .iter()
        .all(|&b| (0x21..=0x7e).contains(&b) && !is_separator(b))
    {
        return Err(Error::runtime(
            "lur.cookie.serialize: name contains an invalid character",
        ));
    }
    Ok(())
}

/// Reject bytes that would break the header: controls (`< 0x20`), DEL
/// (`0x7f`), and `;`. Used for the cookie value and for the
/// `domain`/`path`/`expires` attribute values. Bytes `>= 0x80` are allowed
/// (raw-bytes stance; the serve layer's `HeaderValue::from_bytes` is the
/// final backstop).
fn reject_bad_bytes(label: &str, v: &[u8]) -> Result<(), Error> {
    if v.iter().any(|&b| b < 0x20 || b == 0x7f || b == b';') {
        return Err(Error::runtime(format!(
            "lur.cookie.serialize: {label} contains an invalid character"
        )));
    }
    Ok(())
}

/// Canonicalize a `same_site` value, accepting any case.
fn canon_same_site(v: &[u8]) -> Result<&'static str, Error> {
    if v.eq_ignore_ascii_case(b"strict") {
        Ok("Strict")
    } else if v.eq_ignore_ascii_case(b"lax") {
        Ok("Lax")
    } else if v.eq_ignore_ascii_case(b"none") {
        Ok("None")
    } else {
        Err(Error::runtime(
            "lur.cookie.serialize: same_site must be Strict, Lax, or None",
        ))
    }
}
```

- [ ] **Step 2: Replace the `install_serialize` stub with the real implementation**

Replace the Task 1 stub body with:

```rust
/// `lur.cookie.serialize(name, value, opts?) -> string`. Returns one
/// `Set-Cookie` value (without the `Set-Cookie:` prefix).
fn install_serialize(lua: &Lua, cookie: &Table) -> Result<(), RunError> {
    let serialize = lua
        .create_function(
            |lua, (name, value, opts): (mlua::String, mlua::String, Option<Table>)| {
                let name = name.as_bytes();
                let value = value.as_bytes();
                validate_name(&name)?;
                reject_bad_bytes("value", &value)?;

                let mut out: Vec<u8> = Vec::new();
                out.extend_from_slice(&name);
                out.push(b'=');
                out.extend_from_slice(&value);

                if let Some(opts) = opts {
                    if let Some(domain) = opts.get::<Option<mlua::String>>("domain")? {
                        let domain = domain.as_bytes();
                        reject_bad_bytes("domain", &domain)?;
                        out.extend_from_slice(b"; Domain=");
                        out.extend_from_slice(&domain);
                    }
                    if let Some(path) = opts.get::<Option<mlua::String>>("path")? {
                        let path = path.as_bytes();
                        reject_bad_bytes("path", &path)?;
                        out.extend_from_slice(b"; Path=");
                        out.extend_from_slice(&path);
                    }
                    if let Some(max_age) = opts.get::<Option<Value>>("max_age")? {
                        let n = match max_age {
                            Value::Integer(i) => i,
                            Value::Number(f)
                                if f.is_finite()
                                    && f.fract() == 0.0
                                    && f >= i64::MIN as f64
                                    && f <= i64::MAX as f64 =>
                            {
                                f as i64
                            }
                            _ => {
                                return Err(Error::runtime(
                                    "lur.cookie.serialize: max_age must be an integer",
                                ));
                            }
                        };
                        out.extend_from_slice(format!("; Max-Age={n}").as_bytes());
                    }
                    if let Some(expires) = opts.get::<Option<mlua::String>>("expires")? {
                        let expires = expires.as_bytes();
                        reject_bad_bytes("expires", &expires)?;
                        out.extend_from_slice(b"; Expires=");
                        out.extend_from_slice(&expires);
                    }

                    let same_site = match opts.get::<Option<mlua::String>>("same_site")? {
                        Some(s) => Some(canon_same_site(&s.as_bytes())?),
                        None => None,
                    };
                    let secure = opts.get::<Option<bool>>("secure")?.unwrap_or(false);
                    let http_only = opts.get::<Option<bool>>("http_only")?.unwrap_or(false);

                    if same_site == Some("None") && !secure {
                        return Err(Error::runtime(
                            "lur.cookie.serialize: same_site=None requires secure=true",
                        ));
                    }

                    if http_only {
                        out.extend_from_slice(b"; HttpOnly");
                    }
                    if secure {
                        out.extend_from_slice(b"; Secure");
                    }
                    if let Some(s) = same_site {
                        out.extend_from_slice(b"; SameSite=");
                        out.extend_from_slice(s.as_bytes());
                    }
                }

                lua.create_string(&out)
            },
        )
        .map_err(RunError::Init)?;
    cookie.set("serialize", serialize).map_err(RunError::Init)?;
    Ok(())
}
```

- [ ] **Step 3: Add the serialize tests**

Append to `tests/capabilities.rs`:

```rust
#[test]
fn cookie_serialize_bare_and_single_attributes() {
    run("local s = lur.cookie.serialize\n\
         assert(s('sid', 'abc') == 'sid=abc', 'bare')\n\
         assert(s('a', 'b', {domain='example.com'}) == 'a=b; Domain=example.com', 'domain')\n\
         assert(s('a', 'b', {path='/'}) == 'a=b; Path=/', 'path')\n\
         assert(s('a', 'b', {max_age=3600}) == 'a=b; Max-Age=3600', 'max_age')\n\
         assert(s('a', 'b', {max_age=0}) == 'a=b; Max-Age=0', 'max_age zero')\n\
         assert(s('a', 'b', {max_age=-1}) == 'a=b; Max-Age=-1', 'max_age negative')\n\
         assert(s('a', 'b', {http_only=true}) == 'a=b; HttpOnly', 'http_only')\n\
         assert(s('a', 'b', {secure=true}) == 'a=b; Secure', 'secure')\n\
         assert(s('a', 'b', {same_site='lax'}) == 'a=b; SameSite=Lax', 'same_site canon')");
}

#[test]
fn cookie_serialize_false_omits_and_order_is_fixed() {
    run("local s = lur.cookie.serialize\n\
         assert(s('a', 'b', {secure=false, http_only=false}) == 'a=b', 'false omits flags')\n\
         local full = s('sid', 'abc', {\n\
           domain='example.com', path='/', max_age=3600,\n\
           expires='Mon, 29 Jun 2026 12:53:14 GMT',\n\
           http_only=true, secure=true, same_site='Strict' })\n\
         assert(full == 'sid=abc; Domain=example.com; Path=/; Max-Age=3600; '\n\
           .. 'Expires=Mon, 29 Jun 2026 12:53:14 GMT; HttpOnly; Secure; SameSite=Strict',\n\
           'fixed attribute order')");
}

#[test]
fn cookie_serialize_same_site_none_requires_secure() {
    run("local s = lur.cookie.serialize\n\
         assert(pcall(function() return s('a','b',{same_site='None'}) end) == false,\n\
           'None without secure raises')\n\
         assert(s('a','b',{same_site='None', secure=true}) == 'a=b; Secure; SameSite=None',\n\
           'None with secure ok')\n\
         assert(pcall(function() return s('a','b',{same_site='bogus'}) end) == false,\n\
           'unknown same_site raises')");
}

#[test]
fn cookie_serialize_rejects_invalid_inputs() {
    run("local s = lur.cookie.serialize\n\
         assert(pcall(function() return s('a b', 'v') end) == false, 'name with space')\n\
         assert(pcall(function() return s('a=b', 'v') end) == false, 'name with =')\n\
         assert(pcall(function() return s('', 'v') end) == false, 'empty name')\n\
         assert(pcall(function() return s('a', 'b;c') end) == false, 'value with ;')\n\
         assert(pcall(function() return s('a', 'b' .. string.char(10) .. 'c') end) == false,\n\
           'value with LF')\n\
         assert(pcall(function() return s('a', 'b', {max_age=1.5}) end) == false,\n\
           'non-integer max_age')");
}
```

- [ ] **Step 4: Run the serialize tests (expect PASS), full suite, and the lint gate**

Run: `cargo fmt --all && cargo nextest run -E 'test(cookie)' && cargo clippy --all-targets -- -D warnings`
Expected: all `cookie_*` tests PASS; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add src/capabilities/cookie.rs tests/capabilities.rs
git commit -S -m "feat: add lur.cookie.serialize"
```

---

### Task 3: Documentation

**Files:**
- Modify: `README.md:202-203` (insert after the `lur.crypto` bullet, before `lur.log`)
- Modify: `ARCHITECTURE.md:81` (capability-order line)

**Interfaces:**
- Consumes: the `parse`/`serialize` surface from Tasks 1–2.
- Produces: no code; documentation only.

- [ ] **Step 1: Add the README "Data & I/O" entry**

In `README.md`, insert this bullet immediately after the `lur.crypto` bullet (which ends `...legacy interop only.`) and before the `lur.log` bullet:

```markdown
- **`lur.cookie`** — pure-compute cookie helpers (no policy needed).
  `parse(header) → { name = value, … }` reads a `Cookie` request header
  (lenient: malformed segments are skipped; on a duplicate name the later value
  wins; values are verbatim — no decoding). `serialize(name, value, opts?) →
  string` builds one `Set-Cookie` value (no `Set-Cookie:` prefix); `opts` may
  set `domain`/`path`/`expires` (string), `max_age` (integer seconds),
  `secure`/`http_only` (boolean), and `same_site` (`"Strict"`/`"Lax"`/`"None"`).
  Values are raw bytes (base64 them for arbitrary data); an invalid name, a
  value with `;`/CR/LF, or `same_site="None"` without `secure=true` raises.
  Produce `expires` with `os.date("!%a, %d %b %Y %H:%M:%S GMT", t)`.
```

- [ ] **Step 2: Update the ARCHITECTURE capability-order line**

In `ARCHITECTURE.md`, change the capability-order line (line 81) from:

```
null · log · json · base64 · crypto · io · fs · http · env · db · async · args · serve · state
```

to:

```
null · log · json · base64 · crypto · cookie · io · fs · http · env · db · async · args · serve · state
```

- [ ] **Step 3: Verify docs build / no broken references**

Run: `cargo fmt --all && cargo nextest run -E 'test(cookie)'`
Expected: cookie tests still PASS (sanity; docs are prose only).

- [ ] **Step 4: Commit**

```bash
git add README.md ARCHITECTURE.md
git commit -S -m "docs: document lur.cookie"
```

---

## Self-Review

**Spec coverage:**
- Pure-compute, not policy-gated, installed after `crypto` → Task 1 Step 2; Global Constraints. ✓
- `parse` rules (split `;`, trim OWS, split first `=`, verbatim value, skip no-`=`/empty-name, empty→empty table, later-dup-wins) → Task 1 Step 1 + tests. ✓
- `serialize` name token validation, value byte rejection, fixed attribute order, each attribute, `max_age` integer-only, `same_site` canonicalization, `SameSite=None` guard → Task 2. ✓
- Raw-bytes / no auto-encoding → enforced by taking values verbatim; Global Constraints. ✓
- Errors raise `lur.cookie.<fn>: <detail>` → all error sites prefixed. ✓
- Docs (README Data & I/O + ARCHITECTURE order) → Task 3. ✓

**Placeholder scan:** No TBD/TODO; every code step shows complete code. The Task 1 `install_serialize` stub is intentional scaffolding, replaced wholesale in Task 2 Step 2. ✓

**Type consistency:** `install(lua, lur) -> Result<(), RunError>`, `install_parse`/`install_serialize(lua, cookie)`, helper signatures (`trim_ows(&[u8]) -> &[u8]`, `validate_name(&[u8]) -> Result<(), Error>`, `reject_bad_bytes(&str, &[u8]) -> Result<(), Error>`, `canon_same_site(&[u8]) -> Result<&'static str, Error>`) are referenced consistently across tasks. `mlua::String` fully qualified (no clash with `std::String`). ✓

## Execution Handoff

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks.
2. **Inline Execution** — execute in this session with checkpoints.
