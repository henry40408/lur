# `lur.cookie` — HTTP cookie parse & serialize

Status: design approved, ready for implementation plan.
Date: 2026-06-29.

## Motivation

Reading and setting cookies is one of the most common operations a server-mode
handler performs: session ids, CSRF tokens, preferences. Today a handler can
read the raw `Cookie` request header and emit a raw `Set-Cookie` response header
(via the response `headers` map), but it must split and re-assemble the wire
format by hand — error-prone string work that also risks header injection if a
value carries `\r`/`\n` or `;`.

`lur.cookie` provides the two missing primitives as a pure-compute capability:
`parse` turns a `Cookie` header into a name→value table, and `serialize` builds
one `Set-Cookie` value with the standard attributes, validating inputs so a
malformed cookie fails loudly instead of silently corrupting the response.

## Positioning

- **Pure-compute capability, not policy-gated.** Like `lur.base64`,
  `lur.json`, and `lur.crypto`, it touches no host resource the sandbox
  arbitrates. It is available in both `strict` and `loose` profiles with no
  grant.
- **Raw bytes, no automatic encoding.** Consistent with
  `lur.base64`/`lur.crypto`'s "bytes in/out, let the caller bridge"
  philosophy. `serialize` does not percent-encode the value and `parse` does
  not decode it; a caller storing arbitrary bytes wraps them in `lur.base64`
  themselves. This keeps behaviour predictable and avoids a lossy round-trip
  (a value that legitimately contains `%` is indistinguishable from an encoded
  one).
- **Zero new dependencies.** Pure Rust string parsing and assembly. The
  `expires` attribute is supplied by the caller as a pre-formatted string (see
  below), so no date library is pulled in.
- **Errors raise.** Failures raise a Lua error of the form
  `lur.cookie.<fn>: <detail>` (catchable with `pcall`), matching the existing
  capability convention.
- **Install order.** Installed in `capabilities::install` immediately after
  `crypto` (pure-compute neighbours), before `sandbox(true)`. The capability
  order line in ARCHITECTURE.md is updated to include `cookie`.

## API surface

### `lur.cookie.parse(header) → table`

Parses a `Cookie` **request** header value into a Lua table mapping cookie
names to values (both Lua strings).

```lua
lur.cookie.parse("sid=abc; theme=dark")   -- → { sid = "abc", theme = "dark" }
```

Rules:

- Split the input on `;` into segments.
- Trim leading/trailing optional whitespace (spaces and tabs) from each
  segment.
- Split each segment on its **first** `=`: the part before is the name, the
  part after is the value (a value may itself contain `=`, which is preserved).
- The value is taken verbatim — no unquoting, no percent-decoding.

Lenient parsing (the input comes from a client, so be liberal in what you
accept):

- A segment with no `=`, or with an empty name, is silently skipped.
- Empty input (or input that yields no valid pairs) returns an empty table.

Duplicate names: a later occurrence overwrites an earlier one (the natural
table-assignment semantics). This is documented behaviour.

### `lur.cookie.serialize(name, value, opts?) → string`

Builds one `Set-Cookie` **value** — the string that goes into the `Set-Cookie`
response header, **without** the `Set-Cookie:` prefix.

```lua
lur.cookie.serialize("sid", "abc", {
  path = "/", http_only = true, secure = true,
  same_site = "Lax", max_age = 3600,
})
-- → "sid=abc; Path=/; Max-Age=3600; HttpOnly; Secure; SameSite=Lax"
```

`name` and `value`:

- `name` must be a valid RFC 6265 cookie-name token: no control characters and
  none of the separator characters `( ) < > @ , ; : \ " / [ ] ? = { }` or
  space/tab. An invalid name raises.
- `value` is raw bytes, but must not contain a control character, `\r`, `\n`,
  or `;` (any of which would break the header). An invalid value raises. The
  full RFC cookie-octet set is not enforced — this honours the raw-bytes
  choice; the serve layer's `HeaderValue::from_bytes` is the final backstop.

`opts` (all optional). Attributes are emitted in a **fixed order** for
deterministic, testable output: `Domain`, `Path`, `Max-Age`, `Expires`,
`HttpOnly`, `Secure`, `SameSite`.

| Field        | Lua type | Output                  | Notes |
|--------------|----------|-------------------------|-------|
| `domain`     | string   | `; Domain=<v>`          | validated for illegal characters (control/`;`/CR/LF) |
| `path`       | string   | `; Path=<v>`            | validated for illegal characters |
| `max_age`    | integer  | `; Max-Age=<n>`         | must be an integer; a non-integer (including a float like `1.5`) raises. Negative and `0` are allowed (RFC: expire immediately) |
| `expires`    | string   | `; Expires=<v>`         | pre-formatted IMF-fixdate string; validated for illegal characters. Produce it with `os.date("!%a, %d %b %Y %H:%M:%S GMT", t)` |
| `secure`     | boolean  | `; Secure` when `true`  | `false`/absent omits it |
| `http_only`  | boolean  | `; HttpOnly` when `true`| `false`/absent omits it |
| `same_site`  | string   | `; SameSite=<canonical>`| one of `"Strict"`, `"Lax"`, `"None"`, accepted case-insensitively and emitted in canonical case; any other value raises |

**SameSite=None guard.** If `same_site` is `"None"` but `secure` is not `true`,
`serialize` raises. A browser silently rejects a `SameSite=None` cookie that
lacks `Secure`, so this fail-closed check surfaces the mistake at call time
instead of producing a cookie that never sticks. The guard raises rather than
silently adding `Secure` — it reports the error instead of mutating the
caller's intent.

## Worked example — session login & read

```lua
-- set a session cookie on login
lur.serve.http("POST", "/login", function(req)
  local sid = lur.crypto.hex.encode(lur.crypto.random_bytes(16))
  return {
    status = 204,
    headers = {
      ["Set-Cookie"] = lur.cookie.serialize("sid", sid, {
        path = "/", http_only = true, secure = true,
        same_site = "Lax", max_age = 3600,
      }),
    },
  }
end)

-- read it back on a later request
lur.serve.http("GET", "/me", function(req)
  local cookies = lur.cookie.parse(req.headers["cookie"] or "")
  local sid = cookies.sid
  if not sid then return { status = 401 } end
  -- … look up the session …
  return { status = 200, body = "ok" }
end)
```

Multiple cookies in one response use the `Set-Cookie` array form already
supported by the response `headers` map:

```lua
headers = {
  ["Set-Cookie"] = {
    lur.cookie.serialize("sid", sid, { path = "/", http_only = true }),
    lur.cookie.serialize("theme", "dark", { path = "/" }),
  },
}
```

Note: `req.headers` keys are lower-cased by the server, so the request header is
read as `req.headers["cookie"]`.

## Testing

Inline unit tests in `src/capabilities/cookie.rs` and Lua-script integration
coverage in `tests/capabilities.rs`:

`parse`:
- basic single pair; multiple pairs.
- optional-whitespace trimming around segments.
- a segment with no `=` is skipped; empty input → empty table.
- duplicate name → later value wins.
- a value containing `=` is preserved.

`serialize`:
- `name=value` only (no opts).
- each attribute individually produces the correct token.
- all attributes combined appear in the fixed order.
- a `false` boolean omits its flag.
- `same_site` accepted case-insensitively, emitted canonical; an invalid value
  raises.
- `same_site = "None"` without `secure = true` raises; with `secure = true`
  succeeds.
- an invalid `name` raises; a `value` containing `;`/`\r`/`\n` raises.
- a non-integer `max_age` raises.

## Documentation

- README.md "Data & I/O" section gains a `lur.cookie` entry documenting
  `parse`/`serialize`, the raw-bytes stance, the `expires`-as-string
  convention, and the `SameSite=None`+`Secure` guard.
- ARCHITECTURE.md capability-order diagram and list include `cookie`.

## Out of scope (possible follow-ups)

- Automatic percent-encode/decode of cookie values.
- `expires` accepting a Unix timestamp formatted internally (caller uses
  `os.date`; would require date math or a new dependency).
- Parsing `Set-Cookie` (response direction, with attributes) — this module
  parses only the `Cookie` request header.
- Signed or encrypted cookies (require key management).
