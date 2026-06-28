# `lur.serve` response headers

Status: design approved, ready for implementation plan.
Date: 2026-06-28.

## Motivation

A server-mode handler can currently return only `{ status?, body? }`. There is
no way to set a response header, so a handler cannot send `Content-Type`,
`Location` (redirects), `Set-Cookie`, `Cache-Control`, or any custom header.
This is a hard limitation for almost any real HTTP service. The internal
`response_from` error message already names `{ status, body, headers }`, so the
shape was anticipated; this change implements it.

## Behaviour

The handler's returned table gains an optional `headers` field: a map whose keys
are header names (strings) and whose values are either:

- a **string** — a single header value, or
- an **array of strings** — multiple values for the same header name (e.g. two
  `Set-Cookie` headers).

`status` (default 200) and `body` (default empty) are unchanged. Omitting
`headers` yields no custom headers, exactly as today.

```lua
lur.serve.http("GET", "/thing", function(req)
  return {
    status = 200,
    headers = {
      ["Content-Type"] = "application/json",
      ["Set-Cookie"] = { "a=1", "b=2" },
    },
    body = lur.json.encode(data),
  }
end)
```

## Parsing and validation (`response_from`)

`response_from` walks the `headers` map after reading `status` and `body`:

- A **string** value produces one `(name, value)` pair.
- A **table** value is treated as an array of strings; each element produces a
  `(name, value)` pair with the same name, preserving array order.
- Any other value type for a header (number, boolean, …), or a non-string array
  element, is an error.

Each header name is validated with hyper's `HeaderName::from_bytes` and each
value with `HeaderValue::from_bytes`. **Any invalid name or value (illegal
characters, or a value containing `\r`/`\n`/control characters) makes the whole
response fail:** `response_from` returns `RunError::Script`, which the existing
dispatch path logs and turns into a **500**. This is fail-closed — a malformed
header surfaces immediately rather than being silently dropped — and it closes
the response-splitting / header-injection vector (a handler cannot emit a value
with embedded CRLF).

Iteration order over the Lua map is unspecified; only the relative order of
multiple values under the *same* name (from the array) is guaranteed.

## Content-Type

No automatic `Content-Type` is inferred. `body` is raw bytes and the handler
knows its type; guessing from the body shape is unreliable and violates "explicit
over magic". A handler that wants `application/json` sets it explicitly.

## Rust changes (`src/serve.rs`)

- `Response` gains `headers: Vec<(String, String)>` — the already-expanded, flat,
  order-preserving list of header pairs.
- `handle` (the hyper adapter) sets each pair on the `HyperResponse::builder()`
  via `.header(name, value)` between `.status(...)` and `.body(...)`.
- `response_from` builds the `headers` vec as described, returning
  `RunError::Script` on any validation failure.
- Every other site that constructs a `Response` directly — the 404, 500, 503
  (`timeout_response`), and 413 (`oversize_response`) paths, plus
  `dispatch`/`dispatch_raw` test helpers — sets `headers` to an empty vec
  (host-generated responses carry no custom headers). Use the codebase's
  prevailing idiom (explicit `headers: vec![]` or `..Default::default()`).

## Testing

Integration tests in `tests/serve_http.rs` (real HTTP over the running server):

- A single-value header (`Content-Type`) appears in the response.
- Two `Set-Cookie` values both appear as separate headers.
- Omitting `headers` behaves exactly as today (no extra headers, body/status
  intact).
- A header value containing `\n` yields a 500.
- A header whose value is a number (wrong type) yields a 500.

Unit-level coverage where `response_from`/`dispatch` are exercised directly:

- `response_from` expands a string value and an array value correctly.
- `response_from` returns an error for an invalid value.

## Documentation

- README server section: update the handler return description from
  `{ status?, body? }` to `{ status?, headers?, body? }`, with the multi-value
  `Set-Cookie` example and a note that invalid headers produce a 500 and no
  `Content-Type` is inferred.
- ARCHITECTURE request-lifecycle step 4 (`response_from`): note that headers are
  expanded and validated, and that an invalid header maps to 500.

## Out of scope (possible follow-ups)

- Setting response headers from cron handlers (cron has no HTTP response).
- A streaming/chunked response body.
- Per-route default headers / middleware.
- Automatic `Content-Type` inference.
