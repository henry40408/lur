# Serve Response Headers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a `lur serve` handler set response headers by returning an optional `headers` map (string or array-of-strings values), validated against header injection.

**Architecture:** `Response` gains a flat `headers: Vec<(String, String)>`. `response_from` expands the handler's `headers` map (a string → one header; an array → repeated headers), validates each name/value with hyper's `HeaderName`/`HeaderValue` parsers, and returns a handler error (→ 500) on any invalid or wrong-typed entry. The hyper adapter `handle` sets the pre-validated pairs on the response builder.

**Tech Stack:** Rust (edition 2024), `mlua` (Luau), `hyper` (already a dependency — `hyper::header::{HeaderName, HeaderValue}`).

## Global Constraints

- Run with `cargo nextest run` (NOT `cargo test`). MSRV/toolchain managed separately — do not bump `rust-version`.
- Before every commit: `cargo fmt --all`, then `cargo clippy --all-targets -- -D warnings` must pass clean.
- All commits MUST be GPG-signed (`git commit -S`). Stage files explicitly by name — never `git add -A`/`git add .`.
- No new dependencies (hyper is already present). No `Cargo.toml` changes expected.
- Header parsing lives in `response_from` / a helper in `src/serve.rs`. An invalid header name, an invalid value (CRLF / control chars / illegal token), a non-UTF-8 name/value, or a non-string/non-array value type is a handler error: return `RunError::Script` (the existing dispatch path logs it and returns 500). Do NOT silently skip a bad header.
- `headers` value semantics: a Lua string → one header; a Lua array (sequence) of strings → repeated headers with that name, in array order. Relative order is guaranteed only among values under the same name (map iteration order is otherwise unspecified).
- No automatic `Content-Type` inference.

---

## File Structure

- **Modify:** `src/serve.rs` — `Response` struct (+`headers`), `response_from` (+headers expansion/validation helper), `handle` (emit headers), and every direct `Response { … }` constructor site (empty `headers`).
- **Modify:** `tests/serve.rs` — in-process behavioural tests asserting on `Response.headers`.
- **Modify:** `tests/serve_http.rs` — one real-HTTP test confirming headers reach the wire and an invalid header yields 500.
- **Modify:** `README.md`, `ARCHITECTURE.md` — document the `headers` field.

---

### Task 1: Response headers — struct, parsing, validation, emission

**Files:**
- Modify: `src/serve.rs` (struct `Response` ~L201; `response_from` ~L899; `handle` ~L433; Response literals at ~L426, ~L449, ~L589, ~L597)
- Test: `tests/serve.rs`

**Interfaces:**
- Consumes: existing `Response`, `response_from`, `handle`, `RunError::Script`, `serve(...)`/`dispatch(...)` test helpers.
- Produces: `Response.headers: Vec<(String, String)>` (flat, expanded, order-preserving); handler reply field `headers` (map of string→string|array).

- [ ] **Step 1: Write the failing tests**

Add to `tests/serve.rs`:

```rust
#[test]
fn handler_sets_a_response_header() {
    let s = serve(
        "lur.serve.http('GET', '/h', function(req)\n\
         \treturn { status = 200, headers = { ['Content-Type'] = 'application/json' }, body = '{}' }\n\
         end)",
    );
    let resp = s.dispatch("GET", "/h", b"").expect("dispatch ok");
    assert_eq!(resp.status, 200);
    assert!(
        resp.headers
            .iter()
            .any(|(n, v)| n == "Content-Type" && v == "application/json"),
        "headers: {:?}",
        resp.headers
    );
}

#[test]
fn handler_sets_multiple_values_for_one_header() {
    let s = serve(
        "lur.serve.http('GET', '/h', function(req)\n\
         \treturn { headers = { ['Set-Cookie'] = { 'a=1', 'b=2' } } }\n\
         end)",
    );
    let resp = s.dispatch("GET", "/h", b"").expect("dispatch ok");
    let cookies: Vec<&str> = resp
        .headers
        .iter()
        .filter(|(n, _)| n == "Set-Cookie")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(cookies, vec!["a=1", "b=2"]);
}

#[test]
fn omitting_headers_yields_none() {
    let s = serve("lur.serve.http('GET', '/h', function(req) return { body = 'x' } end)");
    let resp = s.dispatch("GET", "/h", b"").expect("dispatch ok");
    assert!(resp.headers.is_empty(), "headers: {:?}", resp.headers);
    assert_eq!(resp.body, b"x");
}

#[test]
fn invalid_header_value_is_a_handler_error() {
    // A value with an embedded newline must not become a response — header
    // injection guard. dispatch returns Err (the HTTP layer maps it to 500).
    let s = serve(
        "lur.serve.http('GET', '/h', function(req)\n\
         \treturn { headers = { ['X-Bad'] = 'line1\\nline2' } }\n\
         end)",
    );
    assert!(s.dispatch("GET", "/h", b"").is_err());
}

#[test]
fn non_string_header_value_is_a_handler_error() {
    let s = serve(
        "lur.serve.http('GET', '/h', function(req)\n\
         \treturn { headers = { ['X-Num'] = 42 } }\n\
         end)",
    );
    assert!(s.dispatch("GET", "/h", b"").is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -E 'test(handler_sets_a_response_header) | test(handler_sets_multiple_values_for_one_header) | test(omitting_headers_yields_none) | test(invalid_header_value_is_a_handler_error) | test(non_string_header_value_is_a_handler_error)'`
Expected: FAIL — `Response` has no `headers` field, so `tests/serve.rs` does not compile (compile error is an acceptable RED).

- [ ] **Step 3: Add the `headers` field to `Response`**

In `src/serve.rs`, update the struct (~L201):

```rust
/// The host-side view of a handler's reply (spec §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status code (defaults to 200 when the handler omits it).
    pub status: u16,
    /// Response headers, already expanded to a flat ordered list (repeated
    /// names allowed, e.g. multiple `Set-Cookie`). Empty when the handler set
    /// none.
    pub headers: Vec<(String, String)>,
    /// Response body as raw bytes (defaults to empty).
    pub body: Vec<u8>,
}
```

- [ ] **Step 4: Add `headers: Vec::new()` to every host-generated Response literal**

In `src/serve.rs`, update the four direct constructors so the crate compiles:

The 500 fallback in `handle` (~L426):

```rust
                Response {
                    status: 500,
                    headers: Vec::new(),
                    body: b"Internal Server Error".to_vec(),
                }
```

The 404 in `dispatch_async` (~L449):

```rust
            return Ok(Response {
                status: 404,
                headers: Vec::new(),
                body: b"Not Found".to_vec(),
            });
```

`timeout_response` (~L589):

```rust
    Response {
        status: 503,
        headers: Vec::new(),
        body: b"Service Unavailable".to_vec(),
    }
```

`oversize_response` (~L597):

```rust
    Response {
        status: 413,
        headers: Vec::new(),
        body: b"Payload Too Large".to_vec(),
    }
```

- [ ] **Step 5: Parse and validate headers in `response_from`**

In `src/serve.rs`, replace the `response_from` body so it builds `headers` and add the helpers below it. The `Value` and `Table` types are already imported in this file.

```rust
fn response_from(values: MultiValue) -> Result<Response, RunError> {
    let table = match values.into_iter().next() {
        Some(Value::Table(t)) => t,
        _ => {
            return Err(RunError::Script(mlua::Error::RuntimeError(
                "handler must return a table { status, body, headers }".into(),
            )));
        }
    };

    let status = table
        .get::<Option<i64>>("status")
        .map_err(RunError::Script)?
        .unwrap_or(200) as u16;
    let headers = headers_from(&table)?;
    let body = table
        .get::<Option<mlua::String>>("body")
        .map_err(RunError::Script)?
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or_default();

    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Expand and validate the optional `headers` map from a handler's reply. A
/// value is either a string (one header) or an array of strings (the header
/// repeated). An invalid name/value or a wrong value type is a handler error,
/// which the dispatch path turns into a 500 — this also blocks header injection
/// (a value with embedded CRLF is rejected by hyper's parser).
fn headers_from(table: &Table) -> Result<Vec<(String, String)>, RunError> {
    let Some(map) = table
        .get::<Option<Table>>("headers")
        .map_err(RunError::Script)?
    else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for pair in map.pairs::<mlua::String, Value>() {
        let (name, value) = pair.map_err(RunError::Script)?;
        let name = String::from_utf8(name.as_bytes().to_vec())
            .map_err(|_| header_error("a header name is not valid UTF-8"))?;
        match value {
            Value::String(s) => push_header(&mut out, &name, &s)?,
            Value::Table(arr) => {
                for item in arr.sequence_values::<mlua::String>() {
                    let item = item.map_err(RunError::Script)?;
                    push_header(&mut out, &name, &item)?;
                }
            }
            other => {
                return Err(header_error(&format!(
                    "header '{name}' value must be a string or array of strings, got {}",
                    other.type_name()
                )));
            }
        }
    }
    Ok(out)
}

/// Validate one `(name, value)` with hyper's parsers (rejecting CRLF, control
/// characters, and illegal token characters) and push the owned pair.
fn push_header(out: &mut Vec<(String, String)>, name: &str, value: &mlua::String) -> Result<(), RunError> {
    use hyper::header::{HeaderName, HeaderValue};
    HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| header_error(&format!("invalid header name '{name}'")))?;
    let bytes = value.as_bytes();
    HeaderValue::from_bytes(&bytes)
        .map_err(|_| header_error(&format!("invalid value for header '{name}'")))?;
    let value = String::from_utf8(bytes.to_vec())
        .map_err(|_| header_error(&format!("header '{name}' value is not valid UTF-8")))?;
    out.push((name.to_owned(), value));
    Ok(())
}

/// A handler-side header error, surfaced through the normal 500 path.
fn header_error(msg: &str) -> RunError {
    RunError::Script(mlua::Error::RuntimeError(format!("lur.serve: {msg}")))
}
```

- [ ] **Step 6: Emit the headers in `handle`**

In `src/serve.rs`, replace the response-building tail of `handle` (~L433) so each validated pair is set on the builder:

```rust
        let mut builder = HyperResponse::builder().status(response.status);
        for (name, value) in &response.headers {
            builder = builder.header(name, value);
        }
        let built = builder.body(Full::new(Bytes::from(response.body)));
        Ok(built.unwrap_or_else(|_| HyperResponse::new(Full::new(Bytes::new()))))
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo nextest run -E 'test(handler_sets_a_response_header) | test(handler_sets_multiple_values_for_one_header) | test(omitting_headers_yields_none) | test(invalid_header_value_is_a_handler_error) | test(non_string_header_value_is_a_handler_error)'`
Expected: PASS (5 tests).

- [ ] **Step 8: Format, lint, full suite, commit**

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo nextest run
git add src/serve.rs tests/serve.rs
git commit -S -m "feat(serve): let handlers set response headers"
```

Expected: clippy clean, full suite green.

---

### Task 2: Real-HTTP confirmation + documentation

**Files:**
- Test: `tests/serve_http.rs`
- Modify: `README.md`, `ARCHITECTURE.md`

**Interfaces:**
- Consumes: `Response.headers` emission from Task 1; `spawn_server`/`round_trip` helpers in `tests/serve_http.rs`.

- [ ] **Step 1: Write the real-HTTP test**

Add to `tests/serve_http.rs` (header names are emitted lowercased on the wire; lowercase the response before matching):

```rust
#[test]
fn serve_emits_response_headers_over_http() {
    let (addr, _reaper, _dir) = spawn_server(
        "lur.serve.http('GET', '/h', function(req)\n\
         \treturn {\n\
         \t\tstatus = 200,\n\
         \t\theaders = { ['Content-Type'] = 'application/json', ['Set-Cookie'] = { 'a=1', 'b=2' } },\n\
         \t\tbody = '{}',\n\
         \t}\n\
         end)",
    );

    let response = round_trip(
        &addr,
        "GET /h HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    let lower = response.to_lowercase();

    assert!(response.starts_with("HTTP/1.1 200"), "status line: {response:?}");
    assert!(lower.contains("content-type: application/json"), "ct missing: {response:?}");
    assert!(lower.contains("set-cookie: a=1"), "cookie a missing: {response:?}");
    assert!(lower.contains("set-cookie: b=2"), "cookie b missing: {response:?}");
}

#[test]
fn serve_invalid_response_header_is_500() {
    let (addr, _reaper, _dir) = spawn_server(
        "lur.serve.http('GET', '/h', function(req)\n\
         \treturn { headers = { ['X-Bad'] = 'line1\\nline2' } }\n\
         end)",
    );

    let response = round_trip(
        &addr,
        "GET /h HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    assert!(response.starts_with("HTTP/1.1 500"), "status line: {response:?}");
}
```

- [ ] **Step 2: Run the new tests to verify they pass**

Run: `cargo nextest run -E 'test(serve_emits_response_headers_over_http) | test(serve_invalid_response_header_is_500)'`
Expected: PASS (2 tests).

- [ ] **Step 3: Update the README server section**

In `README.md`, the **Server mode** section describes the handler return as `{ status?, body? }`. Update it to document `headers`. Change the `lur.serve.http` bullet's handler description to read:

```markdown
specific route (more static segments, then a concrete method over `ANY`) wins
regardless of registration order. The `handler(req)` returns
`{ status?, headers?, body? }` (`status` defaults to `200`, `body` to empty).
`headers` is a map of header name → value, where a value is a string (one
header) or an array of strings (the header repeated, e.g. multiple
`Set-Cookie`). Header names/values are validated; an invalid one (illegal
characters, or a value with a newline) makes the handler return `500`. No
`Content-Type` is inferred — set it explicitly.
```

Then update the `POST /echo` example block to show a header:

```lua
lur.serve.http("POST", "/echo", function(req)
  local data = req.json()
  return {
    status = 200,
    headers = { ["Content-Type"] = "application/json" },
    body = lur.json.encode(data),
  }
end)
```

- [ ] **Step 4: Update ARCHITECTURE.md request lifecycle**

In `ARCHITECTURE.md`, the **Request lifecycle** section step 4 mentions `response_from` reads `status` and `body`. Extend it to note headers:

Change the step-4 sentence:

```
4. Map the result: a returned table → `response_from` (reads `status`, default 200, and
   `body`, default empty); timeout → **503**; a Lua error → logged and **500**. A handler
   error never brings the server down (spec §8).
```

to:

```
4. Map the result: a returned table → `response_from` (reads `status`, default 200,
   `body`, default empty, and an optional `headers` map — string or array-of-strings
   values, expanded and validated with hyper's header parsers; an invalid header is a
   handler error → **500**); timeout → **503**; a Lua error → logged and **500**. A
   handler error never brings the server down (spec §8).
```

- [ ] **Step 5: Format check, full suite, commit**

```bash
cargo fmt --all
cargo nextest run
git add tests/serve_http.rs README.md ARCHITECTURE.md
git commit -S -m "test(serve): confirm response headers over HTTP; document headers"
```

Expected: full suite green.

---

## Self-Review

**Spec coverage:**
- `headers` map with string-or-array values → Task 1 `headers_from`. ✓
- String → one header; array → repeated headers in order → Task 1 (`Value::String` / `Value::Table` arms) + multi-value test. ✓
- hyper validation, invalid → 500 (fail-closed, injection guard) → Task 1 `push_header` + invalid-value/invalid-type tests + Task 2 real-HTTP 500 test. ✓
- `Response.headers` flat ordered vec; `handle` emits them → Task 1 Steps 3/6 + Task 2 wire test. ✓
- All host-generated Response sites carry empty headers → Task 1 Step 4. ✓
- No Content-Type inference → not implemented (correctly absent); documented in README. ✓
- Omitting headers unchanged → Task 1 `omitting_headers_yields_none`. ✓
- README + ARCHITECTURE updates → Task 2. ✓

**Note on test location:** The spec named `tests/serve_http.rs` for header-presence checks; the plan puts most behavioural assertions in `tests/serve.rs` (in-process `dispatch`, asserting `Response.headers` directly — faster and exact) and keeps one real-HTTP confirmation in `tests/serve_http.rs`. Faithful refinement, not a scope change.

**Type consistency:** `Response.headers: Vec<(String, String)>` used identically in struct, `response_from`, `handle`, and both test files. Helpers `headers_from` / `push_header` / `header_error` named consistently across steps.

**Placeholder scan:** None — every step shows complete code or exact edits.
