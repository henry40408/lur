# `req.cookies` — parsed cookies on the server request

Status: design approved, ready for implementation plan.
Date: 2026-06-29.

## Motivation

`lur.cookie.parse` (shipped in #46) turns a `Cookie` header into a name→value
table, but every handler that reads a cookie must write the same boilerplate:

```lua
local cookies = lur.cookie.parse(req.headers["cookie"] or "")
```

Cookies are read on nearly every authenticated request, so this repeats
constantly. The server already pre-parses the request line into convenience
fields (`params`, `query`/`query_all`, lower-cased `headers`); `req.cookies` is
the same move one level deeper: parse the `Cookie` header once, eagerly, into a
table the handler can read directly as `req.cookies.sid`.

## Behaviour

The request table gains a `cookies` field: a table mapping cookie names to
values (both Lua strings), produced by parsing the request's `Cookie`
header(s) with the existing lenient rules.

```lua
lur.serve.http("GET", "/me", function(req)
  local sid = req.cookies.sid           -- was: lur.cookie.parse(req.headers["cookie"] or "").sid
  if not sid then return { status = 401 } end
  -- …
end)
```

- **Eager.** Parsed once while the request table is built, alongside `headers`
  — not lazily on first access. This matches how `headers`/`query` are already
  materialized and keeps the request table a plain table (no metatable access
  pattern is introduced). The `Cookie` header is typically short, so the cost is
  negligible.
- **Always a table.** When there is no `Cookie` header (or it is empty / all
  segments are malformed), `req.cookies` is an empty table, never `nil`. Reads
  are uniform: `req.cookies.sid` is `nil` for a missing cookie, no `req.cookies`
  existence check needed.
- **Multiple `Cookie` headers merge.** If a request carries more than one
  `Cookie` header, all of them are parsed and merged into the one table; on a
  duplicate cookie name the later occurrence wins (consistent with
  `lur.cookie.parse`'s within-header duplicate rule).
- **Values verbatim.** No decoding, exactly as `lur.cookie.parse`.

## Single source of truth — shared `cookie_pairs`

The parsing rules must not diverge between `lur.cookie.parse` and `req.cookies`.
The lenient split/trim/first-`=` logic currently living inside the
`lur.cookie.parse` closure is extracted into a pure, crate-internal helper in
`src/capabilities/cookie.rs`:

```rust
/// Split a `Cookie`/`Set-Cookie`-style header value into (name, value) byte
/// pairs using the lenient rules: split on `;`, trim OWS per segment, split on
/// the first `=`, skip a segment with no `=` or an empty name, value verbatim.
/// Borrows from the input; the caller builds whatever it needs from the pairs.
pub(crate) fn cookie_pairs(header: &[u8]) -> Vec<(&[u8], &[u8])>;
```

Both call sites build their Lua table from the pairs (later pair overwrites
earlier, preserving "later wins"):

- `lur.cookie.parse` (unchanged behaviour): create a table, set each pair.
- The server request builder: create the `cookies` table, and for each request
  header whose lower-cased name is `cookie`, extend the table from
  `cookie_pairs(value)`.

This keeps one parse implementation, makes the rules unit-testable as a pure
function, and means `req.cookies` and `lur.cookie.parse` can never drift apart.

## Rust changes

`src/capabilities/cookie.rs`:
- Extract `pub(crate) fn cookie_pairs(header: &[u8]) -> Vec<(&[u8], &[u8])>` from
  the current `install_parse` closure body. The closure becomes: create a
  table, iterate `cookie_pairs(header.as_bytes())`, `set` each pair. No
  behaviour change — the existing `cookie_parse_*` tests must stay green.

`src/serve.rs` (`build_request_table`, after the `headers` block at ~line 755):
- Build a `cookies` table. Iterate the request headers; for each whose
  lower-cased name equals `cookie`, call `cookie::cookie_pairs(value.as_bytes())`
  and `set` each `(name, value)` into the table (creating Lua strings from the
  byte slices). Then `table.set("cookies", cookies)?`.
- Cron jobs have no HTTP request and do not build a request table, so they are
  unaffected — no `cookies` field appears there.

## Testing

- **Pure unit tests** for `cookie_pairs` in `src/capabilities/cookie.rs`
  (`#[cfg(test)]` is acceptable here because this is a pure Rust function, unlike
  the capability surface which is Lua-script tested): basic pair, multiple,
  OWS/tab trimming, no-`=` skipped, empty-name skipped, empty input → empty vec,
  inner `=` preserved, duplicate name preserved as two pairs (the table-build
  layer collapses them, not `cookie_pairs`).
- **Server integration tests** (`tests/serve.rs` in-process `dispatch`, and/or
  `tests/serve_http.rs` real HTTP) asserting a handler sees:
  - a single cookie: `Cookie: sid=abc` → `req.cookies.sid == "abc"`.
  - multiple cookies in one header → both present.
  - no `Cookie` header → `req.cookies` is an empty table (and `next(req.cookies)
    == nil`), not `nil`.
  - two `Cookie` headers → merged, later wins on a duplicate name.
- The existing `cookie_parse_*` Lua tests continue to pass unchanged (proves the
  `cookie_pairs` extraction preserved behaviour).

## Documentation

- README server section: the `req` field list (currently "`method`, `path`,
  `params`, `query` …, `headers` …, `body`, and `json()`") gains `cookies`
  (parsed `Cookie` header, lower-cased names not applicable — cookie names are
  case-sensitive; empty table when absent).
- ARCHITECTURE request-lifecycle step 3 (`build_req` field list) adds `cookies`.

## Out of scope (possible follow-ups)

- A `cookies` field on cron context (cron has no HTTP request).
- Lazy/metatable parsing (eager chosen for simplicity and consistency).
- Any change to `lur.cookie.serialize` or response-side cookie handling.
