# lur guide

`lur` runs Luau in a sandbox: `lur script.lua [args]` runs once;
`lur serve app.lua` runs an HTTP server. Capabilities live under `lur.*`, gated
by a policy (default `strict` — deny-all). Flags and the sandbox model are in
the [README](../README.md).

Every example below runs in the test suite (except those that need a network,
a Postgres database, piped input or server mode).

- Failures, including policy denials, raise a Lua error; catch them with `pcall`.
- Every `lur.*` table is read-only: assigning into one raises. Copy into a local
  table to modify.

```lua
assert(not pcall(function() lur.json.encode = nil end))
assert(not pcall(function() lur.extra = 1 end))
```

## Data & I/O

### lur.null

Sentinel for JSON `null` and SQL `NULL`, distinct from `nil` (which means *absent*).

```lua
assert(lur.null ~= nil)
assert(lur.json.decode("[null]")[1] == lur.null)
```

### lur.json

| Call | Returns |
| --- | --- |
| `encode(value)` | string |
| `decode(text, opts?)` | value |

- JSON `null` decodes to `lur.null`. Strings must be UTF-8: base64 binary first.
- `decode(text, { null = "drop" })` omits object keys whose value is `null` and removes
  `null` array elements (the array is compacted, so later elements shift down). Nested
  values are handled the same way; a bare top-level `null` stays `lur.null`. `"keep"` is
  the default.
- `res.json({ null = "drop" })` on an HTTP response takes the same option.

```lua
local s = lur.json.encode({ ok = true, n = 3 })
local v = lur.json.decode(s)
assert(v.ok == true and v.n == 3)
assert(lur.json.decode("null") == lur.null)
```

```lua
local v = lur.json.decode('{"a":1,"b":null,"c":[1,null,2]}', { null = "drop" })
assert(v.b == nil and #v.c == 2 and v.c[2] == 2)
```

### lur.base64

`encode(bytes) → string`, `decode(text) → bytes`.

```lua
local enc = lur.base64.encode("hi")
assert(enc == "aGk=")
assert(lur.base64.decode(enc) == "hi")
```

### lur.crypto

Hashing, HMAC, hex, CSPRNG bytes, constant-time compare. Digests are raw bytes.

| Call | Returns | Notes |
| --- | --- | --- |
| `sha256` / `sha512` / `sha1` / `md5(data)` | bytes | `sha1`/`md5` are legacy-interop only |
| `hmac_sha256` / `hmac_sha512` / `hmac_sha1(key, msg)` | bytes | |
| `hex.encode(bytes)` / `hex.decode(text)` | string / bytes | |
| `random_bytes(n)` | bytes | OS CSPRNG |
| `constant_eq(a, b)` | bool | timing-safe |

```lua
local digest = lur.crypto.sha256("abc")
assert(lur.crypto.hex.encode(digest)
  == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
assert(lur.crypto.hex.decode(lur.crypto.hex.encode(digest)) == digest)

local mac = lur.crypto.hmac_sha256("key", "msg")
assert(lur.crypto.constant_eq(mac, lur.crypto.hmac_sha256("key", "msg")))
assert(not lur.crypto.constant_eq(mac, lur.crypto.hmac_sha256("key", "other")))

assert(#lur.crypto.random_bytes(16) == 16)
assert(#lur.crypto.sha512("x") == 64 and #lur.crypto.sha1("x") == 20)
assert(#lur.crypto.md5("x") == 16)
assert(#lur.crypto.hmac_sha512("k", "m") == 64)
assert(#lur.crypto.hmac_sha1("k", "m") == 20)
```

### lur.cookie

Parse a `Cookie` header; build one `Set-Cookie` value. Values are raw bytes.

| Call | Returns | Notes |
| --- | --- | --- |
| `parse(header)` | `{ name = value, … }` | reads a `Cookie` header |
| `serialize(name, value, opts?)` | string | builds one `Set-Cookie` value (no prefix) |

`serialize` options:

| Option | Type |
| --- | --- |
| `domain`, `path`, `expires` | string |
| `max_age` | integer seconds |
| `secure`, `http_only` | boolean |
| `same_site` | `"Strict"` / `"Lax"` / `"None"` |

- `parse` is lenient: malformed segments are skipped, a later duplicate wins, nothing is
  decoded.
- An invalid name, a value with `;`/CR/LF, or `same_site = "None"` without `secure = true`
  raises.
- Format `expires` with `os.date("!%a, %d %b %Y %H:%M:%S GMT", t)`.

```lua
local jar = lur.cookie.parse("a=1; b=2")
assert(jar.a == "1" and jar.b == "2")

local set = lur.cookie.serialize("sid", "xyz", { http_only = true, path = "/" })
assert(set:find("sid=xyz", 1, true) == 1)
assert(set:find("HttpOnly", 1, true))
```

### lur.time

Clocks and timestamp parsing missing from `os.*`, in integer milliseconds (divide by
`1000` for `os.date`). Malformed input raises.

| Call | Returns | Notes |
| --- | --- | --- |
| `now_ms()` | epoch ms | Unix time |
| `monotonic_ms()` | ms | for elapsed-time differences |
| `parse_rfc3339(text)` | epoch ms | a UTC offset such as `Z` is required |
| `parse_http_date(text)` | epoch ms | |
| `parse_rfc2822(text)` | epoch ms | honors the offset in the text |
| `format_rfc3339(ms, tz?)` | string | |
| `format_rfc2822(ms, tz?)` | string | |
| `format(ms, strftime, tz?)` | string | |
| `parse(text, fmt?, tz?)` | epoch ms | lenient; see below |
| `try_parse(text, fmt?, tz?)` | epoch ms or `nil` | `parse` that returns `nil` for unparseable (or `nil`) text; a bad `fmt` or `tz` still raises |

- `tz` is an IANA name (`"Asia/Taipei"`), a fixed offset (`"+08:00"`), or `nil`/`"UTC"`.
- `parse` without `fmt` accepts RFC 3339, RFC 2822, HTTP-date, then
  `YYYY-MM-DD[ T]HH:MM[:SS[.f]]` and `YYYY/MM/DD[ HH:MM[:SS]]`; wall-clock text with no
  offset is read in `tz`.
- A local time in a DST gap raises; in an overlap the earlier instant wins.
- Use `try_parse` so one bad date need not fail a whole feed.

```lua
assert(lur.time.now_ms() > 0)

local a = lur.time.monotonic_ms()
local b = lur.time.monotonic_ms()
assert(b >= a)

assert(lur.time.parse_rfc3339("1970-01-01T00:00:01Z") == 1000)
assert(lur.time.parse_http_date("Thu, 01 Jan 1970 00:00:01 GMT") == 1000)
```

```lua
local ms = lur.time.parse("2026-10-05 14:30", nil, "Asia/Taipei")
assert(ms == lur.time.parse("2026-10-05T06:30:00Z"))
assert(lur.time.format_rfc3339(ms, "Asia/Taipei") == "2026-10-05T14:30:00.000+08:00")
assert(lur.time.format_rfc2822(ms) == "Mon, 5 Oct 2026 06:30:00 +0000")
assert(lur.time.format(ms, "%Y/%m/%d %H:%M", "Asia/Taipei") == "2026/10/05 14:30")
assert(lur.time.parse_rfc2822("Thu, 01 Jan 1970 00:00:01 +0000") == 1000)
assert(lur.time.parse("05/10/2026", "%d/%m/%Y") == lur.time.parse("2026-10-05"))
```

```lua
assert(lur.time.try_parse("2026-10-05T06:30:00Z") == lur.time.parse("2026-10-05"
  .. " 06:30"))
assert(lur.time.try_parse("not a date") == nil)
```

### lur.url

Parse and build URLs; resolve the relative links found in scraped pages.

| Call | Returns | Notes |
| --- | --- | --- |
| `parse(url)` | `{ href, scheme, host?, port?, username?, password?, path, query?, fragment? }` | `port` only when explicit and non-default; an invalid URL raises |
| `join(base, relative)` | string | resolves relative links; `base` must be absolute |
| `encode_query(table)` | string | keys sorted; an array value repeats the key; string, number and boolean values |
| `decode_query(text)` | table | leading `?` optional, last duplicate wins, `+` is a space |

```lua
local u = lur.url.parse("https://example.com:8443/a?x=1#top")
assert(u.host == "example.com" and u.port == 8443 and u.query == "x=1")

assert(lur.url.join("https://e.com/blog/1", "../about") == "https://e.com/about")

local q = lur.url.encode_query({ b = "x y", a = { 1, 2 } })
assert(q == "a=1&a=2&b=x+y")
assert(lur.url.decode_query("?b=x+y").b == "x y")
```

### lur.charset

For pages that aren't UTF-8 (Big5, GBK, Shift_JIS, … via `encoding_rs`, WHATWG labels):
decode them before `lur.html` sees them.

| Call | Returns |
| --- | --- |
| `decode(bytes, hint?)` | string |
| `encode(text, label)` | bytes |

- `hint` is a charset label or a whole `Content-Type` value.
- Precedence: BOM, then `hint`, then a `<meta charset>` in the first 1 KiB, then UTF-8.
- Invalid bytes become U+FFFD; an unknown label raises.
- `encode` writes unmappable characters as `&#NNN;` and refuses UTF-16.

```lua
local big5 = "\xA7\x41\xA6\x6E"                       -- 你好
assert(lur.charset.decode(big5, "big5") == "你好")
assert(lur.charset.decode(big5, "text/html; charset=Big5") == "你好")
assert(lur.charset.decode('<meta charset="big5">' .. big5):find("你好", 1, true))

-- from a response: lur.charset.decode(res.body, res.headers["content-type"])
assert(lur.charset.encode("你好", "big5") == big5)        -- unmappable chars become &#N;
```

### lur.html

Parse HTML and query it with CSS selectors; sanitize untrusted markup.
`parse(html) → doc`: invalid UTF-8 is replaced (decode other encodings with
`lur.charset` first), and parsing follows the HTML5 algorithm, so `<td>` outside a
`<table>` is dropped.

The doc and its elements are nodes sharing these methods; call them with `:`.

| Method | Returns | Notes |
| --- | --- | --- |
| `select(css)` | array | descendants only |
| `select_one(css)` | node or `nil` | descendants only; for both, an invalid selector raises |
| `text(opts?)` | string | `{ trim = true }` strips surrounding whitespace (default `false`: text as is); `lur.html` only |
| `text(css)` | string or `nil` | shorthand for `select_one(css):text()`; `nil` when nothing matches, `""` when the match has no text; takes no options (`lur.html` and `lur.xml`) |
| `html()`, `inner_html()`, `tag()` | string | |
| `attr(name)` | string or `nil` | |
| `attrs()` | table | |
| `parent()` | node or `nil` | `nil` for the document and the root element |
| `children()` | array | elements only |

```lua
local doc = lur.html.parse([[<ul><li class="a"> one </li><li>two</li></ul>]])
assert(#doc:select("li") == 2)
assert(doc:text("li.a") == " one ")
assert(doc:select_one("li.a"):text({ trim = true }) == "one")
assert(doc:text("p") == nil)
```

`lur.html.sanitize(html, opts?) → string` strips scripts, styles, event handlers and
unsafe URLs with `ammonia`'s allowlist.

- Only `http`, `https` and `mailto` links survive, and links get
  `rel="noopener noreferrer"`.
- `opts.base` (absolute URL) resolves relative `href`/`src`; without it they are kept as
  written.
- Broken markup is repaired.

```lua
local dirty = [[<p onclick="x()">Hi <a href="/a">a</a><script>alert(1)</script></p>]]
local clean = lur.html.sanitize(dirty, { base = "https://e.com/blog/" })
assert(clean:find('href="https://e.com/a"', 1, true))
assert(not clean:find("script") and not clean:find("onclick"))
```

### lur.xml

Read RSS, Atom, sitemaps and other XML. `parse(xml) → doc` has the same node methods as
`lur.html` (`select`, `select_one`, `text`, `tag`, `attr`, `attrs`, `parent`,
`children`), but selection takes a slash path.

| Path | Meaning |
| --- | --- |
| `a/b` | children |
| `//a` | descendants (also mid-path) |
| `*` | any name |

- Results are in document order and deduplicated.
- Names match exactly as written, so a prefixed tag is `dc:creator`; namespaces are not
  resolved.
- Malformed XML raises with line/column.
- Unknown entities such as `&nbsp;` are kept as text; `DOCTYPE` is ignored, so custom
  entities are not expanded.
- A BOM or the declaration's `encoding` selects the charset; otherwise invalid UTF-8 is
  replaced. Use `lur.charset` only for non-XML text.

```lua
local doc = lur.xml.parse([==[<rss xmlns:dc="http://purl.org/dc/elements/1.1/"><channel>
  <item><title>A</title><link>https://e.com/a</link><dc:creator>Ann</dc:creator></item>
  <item><title><![CDATA[B & C]]></title><link href="https://e.com/b"/></item>
</channel></rss>]==])
local items = doc:select("rss/channel/item")
assert(#items == 2)
assert(items[1]:select_one("dc:creator"):text() == "Ann")
assert(items[2]:select_one("title"):text() == "B & C")
assert(items[2]:select_one("link"):attr("href") == "https://e.com/b")
assert(#doc:select("//title") == 2)
assert(items[1]:tag() == "item" and items[1]:parent():tag() == "channel")
assert(#items[1]:children() == 3)
assert(doc:select_one("//link"):attrs() ~= nil)
```

### lur.feed

Emit a feed: `rss(meta, items, opts?)`, `atom(meta, items, opts?)` and
`json(meta, items, opts?)` return a string (RSS 2.0 / Atom 1.0 / JSON Feed 1.1).

| Field | Where | Notes |
| --- | --- | --- |
| `title` | `meta`, item | required |
| `feed_url`, `description`, `language`, `updated` | `meta` | optional |
| `link`, `id` | `meta` | optional (see format requirements below) |
| `guid`, `link` | item | optional (see format requirements below) |
| `date`, `updated`, `summary`, `content` (HTML), `author` | item | optional |
| `categories` | item | array of strings |
| `enclosure` | item | `{ url, type?, length? }` |

- Dates are epoch milliseconds.
- RSS needs `meta.link`; Atom needs `meta.id` or `meta.link`; Atom/JSON items need `guid`
  or `link`.
- XML is escaped and characters illegal in XML 1.0 are dropped.
- `opts.skip_invalid = true` skips items whose `title` is missing (these raise by
  default) and also drops empty or blank titles, which are accepted by default. Any
  other problem (wrong types, bad dates, no `guid`/`link` for Atom/JSON) still raises.

```lua
local doc = lur.html.parse([[<ul><li><a href="/a">A</a></li><li><a href="/b">B</a></li></ul>]])
local items = {}
for _, a in ipairs(doc:select("li a")) do
  items[#items + 1] = { title = a:text(), link = "https://e.com" .. a:attr("href") }
end
local xml = lur.feed.atom({ title = "E", link = "https://e.com" }, items)
assert(xml:find("<title>A</title>", 1, true))

local meta = { title = "E", link = "https://e.com" }
assert(lur.feed.rss(meta, items):find("<rss", 1, true))
assert(lur.json.decode(lur.feed.json(meta, items)).items[1].title == "A")
```

```lua
local doc = lur.html.parse("<ul><li> A </li><li> </li></ul>")
local items = {}
for _, li in ipairs(doc:select("li")) do
  items[#items + 1] = { title = li:text({ trim = true }), link = "https://e.com" }
end
assert(items[1].title == "A")
local xml = lur.feed.rss({ title = "E", link = "https://e.com" }, items, { skip_invalid = true })
assert(xml:find("<title>A</title>", 1, true))
local _, count = xml:gsub("<item>", "")
assert(count == 1)
```

### lur.log

`info`/`warn`/`error(msg)` write `<level>: <msg>\n` to **stderr** (stdout is the
data channel).

```lua
lur.log.info("starting")
lur.log.warn("careful")
lur.log.error("oops")
```

### lur.io

`lur.stdout` and `lur.stdin` are raw byte streams.

| Call | Behaviour |
| --- | --- |
| `lur.stdout.write(bytes)` | raw bytes, no implicit newline |
| `lur.stdout.flush()` | |
| `lur.stdin.read()` | drains all bytes |
| `lur.stdin.read(n)` | up to `n` bytes (`nil` at EOF) |
| `lur.stdin.lines()` | iterates newline-stripped lines |

```lua
lur.stdout.write("data\n")
lur.stdout.flush()
```

```lua ignore
-- Reading stdin needs piped input; run as: echo hi | lur read.lua
local all = lur.stdin.read()
lur.stdout.write(all)
for line in lur.stdin.lines() do
  lur.stdout.write(line .. "\n")
end
```

### lur.args

`positional` is a 1-indexed array; `flags` maps `--name value` / `--name=value` to the
string and a bare `--flag` to `true`. Both are read-only, like every `lur.*` table: copy
into a local table to modify.

```lua
assert(type(lur.args.positional) == "table")
assert(type(lur.args.flags) == "table")

local flags = {}
for k, v in pairs(lur.args.flags) do flags[k] = v end
flags.verbose = true -- the copy is writable
assert(not pcall(function() lur.args.flags.verbose = true end))
```

## Validation

### lur.validate

Validate and convert untrusted input (query strings, JSON bodies) with small composable
schemas. Pure compute, no policy, same in one-shot and serve mode. The result is always a
new table; the input is never modified.

| Call | Returns | On failure |
| --- | --- | --- |
| `parse(schema, value)` | converted value | raises `err` |
| `try(schema, value)` | `true, converted` or `false, err` | returns the same `err` |

```lua
local q = lur.validate.parse(lur.validate.object({
  q = lur.validate.str({ min_len = 1 }),
  limit = lur.validate.int({ min = 1, max = 100, default = 20 }),
}), { q = "luau", limit = "5" })           -- { q = "luau", limit = 5 }
assert(q.q == "luau" and q.limit == 5)
```

#### Schemas

`opts` is always optional.

| Schema | Options | Accepts |
| --- | --- | --- |
| `str{ … }` | `pattern?`, `min_len?`, `max_len?`, `default?` | strings only (numbers are not coerced); `pattern` is a Lua pattern as in `string.find`, unanchored; lengths count characters |
| `int{ … }` | `min?`, `max?`, `default?`, `clamp?` | a number with an integral value, or a string that is strictly `-?%d+`; abs(n) must be <= 2^53 |
| `number{ … }` | `min?`, `max?`, `default?`, `clamp?` | finite numbers only; strings must be decimal floats |
| `bool{ … }` | `default?` | a boolean, or the strings `"true"` / `"false"` |
| `one_of{ … }` | the allowed values | strict equality over strings / numbers / booleans |
| `list(item, opts?)` | `min_items?`, `max_items?` | an array whose items match `item` |
| `object(fields, opts?)` | `strict?` | a table whose fields match `fields` |
| `optional(schema)` | | `schema`, or an absent value |
| `function(value) → value` | | a plain function is a schema anywhere one is accepted; raising rejects (the message, minus its position, becomes the issue message) |

- `clamp = true` clamps into `[min, max]` instead of failing.
- Unknown `object` keys are dropped; `strict = true` makes them issues.
- Bad arguments (unknown option, `min > max`) raise `lur.validate.<fn>: …`.
- Schemas nest at most 64 levels.

```lua
local schema = lur.validate.object({
  q = lur.validate.str({ min_len = 1, max_len = 100 }),
  limit = lur.validate.int({ min = 1, max = 100, default = 20 }),
  order = lur.validate.optional(lur.validate.one_of({ "asc", "desc" })),
  tags = lur.validate.list(lur.validate.str({ pattern = "^%w+$" }), { max_items = 5 }),
  ratio = lur.validate.number({ min = 0, max = 1, default = 0.5 }),
  exact = lur.validate.bool({ default = false }),
})

-- strings from a query string are converted; unknown keys are dropped
local ok, out = lur.validate.try(schema, { q = "luau", limit = "5", tags = { "a" }, junk = 1 })
assert(ok and out.limit == 5 and out.ratio == 0.5 and out.junk == nil)

-- a plain function is a schema too: return the value, or raise to reject
local even = function(n)
  assert(n % 2 == 0, "must be even")
  return n
end
assert(lur.validate.parse(lur.validate.object({ n = even }), { n = 4 }).n == 4)
```

#### Missing values and defaults

- Values are required unless wrapped in `optional(...)` or given a `default` (which fills
  a missing value as-is).
- For `int`/`number`/`bool`, `""` counts as missing, so `?limit=` falls back to the
  default.
- `optional(x)` only waives "required": an inner `default` still fills.
- A `lur.null` (JSON `null`) given for an object field or the root counts as absent:
  `optional` yields `nil`, `default` fills, and a required value reports `is required`.
  Function validators still receive the raw `lur.null`, and `null` list items are not
  absent (they fail normally).
- `default` values are returned as given, not checked against `min`/`max`/`pattern`.
- A function validator's result is used as is, so returning (or mutating) its input
  table aliases it: the input is not deep-copied there.

#### Errors

On failure `err` is a table:

```text
{ kind = "validation", message = "…", issues = { { path = "a.b[2].c", message = "…" }, … } }
```

- All issues are collected; paths read `a.b[2].c`, the root is `""`.
- At most 100 issues are kept, then `truncated = true`.
- `tostring(err)` is `message`, an uncaught one prints that message, and it is catchable
  with `pcall`.
- Raised inside a `lur.async` task it reaches the combinator's caller as a plain string
  (its message), not a table; use `try` there.

```lua
local schema = lur.validate.object({
  q = lur.validate.str({ min_len = 1 }),
  limit = lur.validate.int({ max = 100 }),
  tags = lur.validate.list(lur.validate.str({ pattern = "^%w+$" })),
})

-- all problems are reported, with paths
local ok, err = lur.validate.try(schema, { q = "", limit = "500", tags = { "a", "b c" } })
assert(not ok and err.kind == "validation" and #err.issues == 3)
assert(err.issues[1].path == "limit" and err.issues[1].message == "must be <= 100")

-- parse raises the same value; catch it with pcall
local caught, e = pcall(lur.validate.parse, schema, {})
assert(not caught and e.kind == "validation" and tostring(e) == e.message)

local ok2, err2 = lur.validate.try(lur.validate.int({ max = 10 }), "11")
assert(not ok2 and err2.issues[1].message == "must be <= 10")
```

## Capabilities (policy-gated)

### lur.fs

`read(path) → bytes`, `write(path, bytes)`. Grant with `--allow-fs-read` /
`--allow-fs-write` / `--allow-fs` (or `-A`).

- Paths are canonicalized before the allowlist check, so `..` and symlink escapes fail.
- Files are opened confined to the granted root, so a symlink swapped in mid-call cannot
  escape either.

```lua
lur.fs.write("./note.txt", "hello")
assert(lur.fs.read("./note.txt") == "hello")
```

### lur.env

`lur.env(name) → string | nil` (`lur.env` is itself the function). Grant with
`--allow-env` (or `-A`). It returns `nil` for **both** denied and unset, so it is not an
oracle.

```lua
assert(lur.env("LUR_GUIDE_DEFINITELY_UNSET") == nil)
```

### lur.http

`request(method, url, opts?)` plus `get`/`post`/`put`/`patch`/`delete`/`head(url, opts?)`.
Grant hosts with `--allow-net`.

| `opts` | Type | Meaning |
| --- | --- | --- |
| `headers` | table | request headers |
| `query` | table | query parameters |
| `body` / `json` / `form` | one of | raw body, JSON-encoded value, or a flat table of string/number/boolean values (url-encoded with sorted keys, so the body is deterministic; nested tables and functions raise; `Content-Type` is set only when `headers` has none) |
| `timeout` | ms | per-request timeout; no default of its own (the global `--timeout` still applies) |
| `cache` | table | `{ ttl_ms, vary? }`, see [Caching](#caching) |

Returns `{ status, body, headers, headers_all, json(), text(), html() }`. The helpers are
closures: call them with a dot, e.g. `res.text()`, not `res:text()`. All three work on
cached responses.

| Helper | Behaviour |
| --- | --- |
| `res.json(opts?)` | decodes `body`; `opts` as in `lur.json.decode` |
| `res.text()` | decodes `body` with the `lur.charset` rules: a BOM, then the `Content-Type` charset, then `<meta charset>` in the first 1 KiB, then UTF-8 (an unknown header charset falls back to the `<meta>`/UTF-8 steps; invalid bytes become U+FFFD), so a Big5 or Shift_JIS page arrives as proper UTF-8 |
| `res.html()` | the same document node as `lur.html.parse(res.text())` |

Guards:

- Every request and redirect hop is checked against the allowlist and the private-IP
  (SSRF) guard.
- TLS is always verified; the body is capped by `--max-http-body`.
- Proxy environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) are ignored.

The examples need a network, so they are shown but not run.

```lua ignore
local res = lur.http.get("https://example.com", { timeout = 5000 })
assert(res.status == 200)

local posted = lur.http.post("https://api.example.com/items", {
  json = { name = "widget" },
})
local body = posted.json()

-- url-encoded form; `res.json({ null = "drop" })` omits JSON nulls
local login = lur.http.post("https://example.com/login", {
  form = { user = "ann", remember = true },
})
local data = login.json({ null = "drop" })

-- legacy-charset pages: decode via Content-Type, then query with CSS selectors
local page = lur.http.get("https://example.com/big5.html")
local utf8 = page.text()
local title = page.html():select_one("title"):text()

-- `request` takes any method
lur.http.request("OPTIONS", "https://api.example.com/items")
lur.http.put("https://api.example.com/items/1", { json = { name = "v2" } })
lur.http.patch("https://api.example.com/items/1", { json = { name = "v3" } })
lur.http.delete("https://api.example.com/items/1")
local probe = lur.http.head("https://example.com")
assert(probe.status == 200)
```

#### Caching

`cache = { ttl_ms = 60000, vary? }` serves repeat GETs from memory and sets `res.cached`
(`true` on a hit).

- GET only. In memory, shared by the whole process; it needs no `--db`, but is lost on
  restart and not shared between processes.
- Only 2xx responses are stored, and not ones that set a cookie. The policy check runs
  before the lookup.
- The key is the method, final URL (query included) and all request headers. A request
  carrying `Authorization`, `Cookie` or `Proxy-Authorization` bypasses the cache unless
  that header is named in `vary`, e.g. `vary = { "authorization" }`.
- `lur.http.cache_clear()` drops everything and returns how many entries it held.

Bounds:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--http-cache-max-entries` | `1024` | entry cap; `0` is unlimited |
| `--http-cache-max-bytes` | `64m` | byte cap; `0` is unlimited |

- An insert over either cap frees expired entries first, then the least recently used.
- A single response larger than the byte cap is simply not cached (`res.cached` stays
  `false`); each body is still bounded by `--max-http-body`.
- Expired entries are also freed on lookup and swept every minute on insert.

```lua ignore
-- cached in memory for 5 minutes
local feed = lur.http.get("https://example.com/feed.xml", { cache = { ttl_ms = 300000 } })
assert(feed.cached == false or feed.cached == true)

-- keyed per Authorization header instead of bypassing the cache
lur.http.get("https://api.example.com/me", {
  headers = { authorization = "Bearer t" },
  cache = { ttl_ms = 60000, vary = { "authorization" } },
})
local dropped = lur.http.cache_clear() -- number of entries dropped
```

## Storage

Requires `--db`, which takes a SQLite path or a `postgres://` / `postgresql://` URL; the
scheme picks the backend at first use. There is no SQL-portability layer: placeholders and
types are native to the backend.

### lur.db

| Call | Returns | Notes |
| --- | --- | --- |
| `exec(sql, ...params)` | `{ rows_affected, last_insert_id }` | |
| `query(sql, ...params)` | rows (tables keyed by column) | |
| `tx(fn)` | | runs `fn(tx)` on a pinned connection, committing on return and rolling back on error |

- Placeholders: `?` on SQLite, `$1, $2, …` on Postgres. JSON-encode tables before binding.

```lua
lur.db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
local r = lur.db.exec("INSERT INTO t (name) VALUES (?)", "alice")
assert(r.rows_affected == 1)

local rows = lur.db.query("SELECT name FROM t WHERE id = ?", r.last_insert_id)
assert(rows[1].name == "alice")

lur.db.tx(function(tx)
  tx.exec("INSERT INTO t (name) VALUES (?)", "bob")
end)
assert(#lur.db.query("SELECT id FROM t") == 2)
```

### lur.kv

Key/value store on the `--db` backend; string keys, raw-byte values.

| Call | Returns | Notes |
| --- | --- | --- |
| `get(key)` | bytes or `nil` | counters come back as decimal strings |
| `set(key, bytes)` | | |
| `delete(key)` | | |
| `add(key, value)` | bool | atomic set-if-absent |
| `cas(key, expected, new)` | bool | atomic; `nil` expected = must be absent, `nil` new = delete; counters (`incr`/`decr`) are stored as integers, so `cas` never matches them |
| `incr` / `decr(key, n?)` | integer | atomic counters, start from 0, step 1 |
| `update(key, fn)` | | atomic read-modify-write; return `nil` to delete |
| `expire(key, ms)` | bool | sets an expiry on a live key |
| `ttl(key)` | `ms, exists` | see [Expiry](#expiry) |

- Inside `update`'s `fn`, a nested `lur.kv` call raises, and a `lur.db` write blocks on
  the lock, so avoid it.

```lua
lur.kv.set("greeting", "hi")
assert(lur.kv.get("greeting") == "hi")
lur.kv.delete("greeting")
assert(lur.kv.get("greeting") == nil)

assert(lur.kv.add("once", "first") == true)
assert(lur.kv.add("once", "again") == false)
assert(lur.kv.get("once") == "first")

-- cas(key, expected, new): true if applied
assert(lur.kv.cas("once", "first", "second") == true)
assert(lur.kv.cas("once", "first", "nope")  == false)

-- incr/decr: start from 0; optional step
assert(lur.kv.incr("hits")    == 1)
assert(lur.kv.incr("hits", 4) == 5)
assert(lur.kv.decr("hits", 2) == 3)

-- update: return the new value, or nil to delete
lur.kv.update("counter", function(cur)
  local n = tonumber(cur) or 0
  return tostring(n + 1)
end)
assert(lur.kv.get("counter") == "1")
```

#### Expiry

`set`, `add`, `cas`, `update`, `incr` and `decr` take a last `opts` table with
`ttl_ms` (a positive number of **milliseconds**; zero, negatives and non-numbers
raise). An expired key reads as absent everywhere: `get` is `nil`, `add` succeeds,
`cas` with `expected = nil` matches, `update` sees `nil`, `incr` restarts from 0.

| Call | Effect on an existing expiry |
| --- | --- |
| `set` without `ttl_ms` | clears it, like Redis `SET` |
| `cas` / `update` without `ttl_ms` | keep it; pass `ttl_ms` to replace it |
| `incr` / `decr` | apply `ttl_ms` only when the key has no expiry yet (the window opens at the first hit and later hits don't extend it: a fixed window); `renew_ttl = true` resets it on every call instead (idle timeout) |

- A counter that predates expiries picks one up on its next `incr` with `ttl_ms`.
- `ttl(k)` reports: `nil, false` absent · `nil, true` never expires · `ms, true` expiring.
- Always pass `ttl_ms` to `incr` for a rate limit: without it the counter never resets.

```lua
lur.kv.set("session", "abc", { ttl_ms = 60000 })
local ms, exists = lur.kv.ttl("session")
assert(exists and ms > 0 and ms <= 60000)

lur.kv.set("session", "abc") -- no ttl_ms: now permanent
ms, exists = lur.kv.ttl("session")
assert(ms == nil and exists)
assert(lur.kv.expire("session", 30000) == true)
ms, exists = lur.kv.ttl("missing")
assert(ms == nil and not exists)

-- Fixed-window rate limit: 60 hits per minute per client.
local hits = lur.kv.incr("rl:203.0.113.7", 1, { ttl_ms = 60000 })
assert(hits == 1)
if hits > 60 then
  -- reject; lur.kv.ttl(key) is the time left until the window resets
end
```

### Postgres backend

`--db postgres://…` (or `postgresql://`) selects Postgres. Nothing is translated.

- **Placeholders** are `$1, $2, …`.
- **`last_insert_id` is SQLite-only**: always `0` on Postgres; use
  `INSERT … RETURNING id` via `db.query`.
- **Row types**: only `int2`/`int4`/`int8` (integer), `float4`/`float8` (number), and
  `text`/`varchar`/`bpchar`/`name`/`bytea` (string) map to Lua. Anything else
  (`numeric`, `timestamptz`, `jsonb`, `uuid`, `bool`, arrays, …) raises
  `lur.db: unsupported column type '<T>' in column '<name>'; CAST it to text (e.g.
  <name>::text)`, so cast it, e.g. `col::text`.
- **TLS**: append `?sslmode=require` (or another `sslmode`) to the URL.
- **SQLite contention**: write transactions use `BEGIN IMMEDIATE`; a 5 s
  `busy_timeout` plus up to 5 jittered attempts on single-statement writes, lock
  acquisition, and open absorb "database is locked".
- **`db.tx` / `kv.update` can fail**: on Postgres they run at `SERIALIZABLE` and may
  raise a serialization conflict with a stable, locale-independent message containing
  `SQLSTATE 40001`; on SQLite they may raise once busy retries run out. `lur` does
  **not** retry them: wrap them in `pcall` or your own retry.

```lua ignore
-- lur --db postgres://user:pass@localhost/lur_dev?sslmode=disable app.lua
lur.db.exec("CREATE TABLE IF NOT EXISTS t (id SERIAL PRIMARY KEY, name TEXT)")
local rows = lur.db.query("INSERT INTO t (name) VALUES ($1) RETURNING id", "alice")
assert(rows[1].id ~= nil)

local ok, err = pcall(function()
  return lur.db.tx(function(tx)
    tx.exec("UPDATE t SET name = $1 WHERE id = $2", "alicia", rows[1].id)
  end)
end)
assert(ok or err ~= nil)
```

## Concurrency

### lur.async

`sleep(ms)`, and combinators over arrays of zero-arg functions:

| Call | Behaviour |
| --- | --- |
| `all(fns)` | fail-fast |
| `race(fns)` | first to settle |
| `any(fns)` | first to succeed |
| `settled(fns)` | never raises; `{ ok, value }` or `{ ok, err }` per task |
| `map(list, fn, opts?)` | applies `fn(item, index)` to every array element concurrently; results in input order |

- `map` options: `opts.limit` (integer >= 1) caps that call's in-flight tasks;
  `opts.drop_errors` omits failed items instead of failing fast.
- Lua runs one step at a time; tasks interleave only at I/O awaits.
- `--max-concurrency` caps in-flight tasks.

```lua
lur.async.sleep(1)
local results = lur.async.all({
  function() return 1 end,
  function() return 2 end,
})
assert(results[1] == 1 and results[2] == 2)

local settled = lur.async.settled({
  function() error("boom") end,
  function() return "ok" end,
})
assert(settled[1].ok == false)
assert(settled[2].ok == true and settled[2].value == "ok")

-- race: first to settle wins
local first = lur.async.race({
  function() return "fast" end,
  function() lur.async.sleep(20); return "slow" end,
})
assert(first == "fast")

-- any: first to *succeed* wins
local winner = lur.async.any({
  function() error("nope") end,
  function() return "winner" end,
})
assert(winner == "winner")
```

```lua
-- map: concurrent, ordered; limit bounds in-flight tasks
local doubled = lur.async.map({ 1, 2, 3, 4 }, function(n, i)
  lur.async.sleep(5)
  return n * 2
end, { limit = 2 })
assert(#doubled == 4 and doubled[4] == 8)

-- drop_errors: failed items are omitted instead of raising
local kept = lur.async.map({ 1, 2, 3 }, function(n)
  if n == 2 then error("skip") end
  return n
end, { drop_errors = true })
assert(#kept == 2 and kept[2] == 3)
```

### lur.state

Process-wide state shared across the VM pool, primitives only.

| Call | Returns | Notes |
| --- | --- | --- |
| `get(key)` | value | |
| `set(key, value)` | | `nil` deletes |
| `incr` / `decr(key, n?)` | | atomic; integer step, default 1 |
| `add(key, value)` | bool | set-if-absent |
| `cas(key, expected, new)` | bool | compare-and-set; `nil` = absent |
| `update(key, fn)` | | optimistic CAS retry; `fn` runs unlocked |
| `ttl(key)` | `ms, exists` | |
| `fetch(key, { ttl_ms, stale_ms? }, fn)` | `value, "hit"\|"miss"\|"stale"` | a cache; see below |

```lua
lur.state.set("hits", 0)
assert(lur.state.incr("hits", 2) == 2)
assert(lur.state.decr("hits", 1) == 1)
lur.state.update("hits", function(n) return (n or 0) + 1 end)
assert(lur.state.get("hits") == 2)
lur.state.set("hits", nil)
assert(lur.state.get("hits") == nil)
lur.state.set("x", 10)
assert(lur.state.cas("x", 10, 20) == true)   -- matched: 10 -> 20
assert(lur.state.cas("x", 10, 30) == false)  -- stale: value is now 20
assert(lur.state.get("x") == 20)
assert(lur.state.add("once", "hello") == true)
assert(lur.state.add("once", "world") == false)
assert(lur.state.get("once") == "hello")
```

Expiry and memory:

- Writes (`set`/`add`/`cas`/`update`/`incr`/`decr`) take `{ ttl_ms = n, evictable = bool }`
  last, with the same expiry rules as the [`lur.kv` expiry table](#expiry) except that
  `add` also clears an existing expiry and `evictable` is state-only.
- `--state-max-entries` / `--state-max-bytes` (default unlimited) drop only `evictable`
  entries, expired first, then least recently used; a write that cannot fit raises.

`fetch` is a read-through cache:

- `fn` runs on a miss (it may await I/O; unlocked, not retried) and its value is stored
  as evictable. `nil` from `fn` is not cached.
- If `fn` raises and a value within `stale_ms` of expiry exists, that value comes back
  as `"stale"` instead.
- A stale value is invisible to everything but `fetch`.

```lua
local calls = 0
local function load() calls = calls + 1; return "page" end
local v, status = lur.state.fetch("page", { ttl_ms = 60000 }, load)
assert(v == "page" and status == "miss")
v, status = lur.state.fetch("page", { ttl_ms = 60000 }, load)
assert(v == "page" and status == "hit" and calls == 1)

lur.state.incr("window", 1, { ttl_ms = 60000 })  -- fixed-window counter
local ms, exists = lur.state.ttl("window")
assert(exists and ms > 0)
```

## Server mode

### lur.serve

Only under `lur serve`; handlers are registered once at load time. On `SIGTERM` /
`SIGINT`, in-flight requests and cron runs drain within `--shutdown-grace`. The examples
below need a running server, so they are shown but not run.

#### Routing: `lur.serve.http(method, path, handler)`

`method` is `"GET"`…/`"ANY"`.

- **Path params**: `:name` segments (e.g. `/users/:id`) bind into `req.params`. A trailing
  `:name?` is optional (`/feed/:tag?` matches `/feed` and `/feed/lua`; absent is `nil`);
  it may not share a static/param shape with another route of the same method, e.g.
  `/feed`.
- **Precedence**: the most specific route wins regardless of order (more static segments,
  then a concrete method over `ANY`).
- **Response**: `handler(req)` returns a table:

  | Field | Default | Notes |
  | --- | --- | --- |
  | `status` | `200` | must be in `100..=599` |
  | `headers` | none | a name maps to a string or an array of strings (repeated header, e.g. two `Set-Cookie`) |
  | `body` | empty | |

- No `Content-Type` is inferred.
- An invalid header name/value (including CR/LF), or `Content-Length` /
  `Transfer-Encoding`, is a **500**.

```lua ignore
lur.serve.http("POST", "/echo", function(req)
  local data = req.json()
  return {
    headers = {
      ["Content-Type"] = "application/json",
      ["Set-Cookie"] = { lur.cookie.serialize("seen", "1"), "theme=dark" },
    },
    body = lur.json.encode(data),
  }
end)

lur.serve.http("GET", "/feed/:tag?", function(req)
  return { body = "tag: " .. (req.params.tag or "all") }
end)
```

#### `lur.serve.cron(spec, handler, opts?)`

`spec` is a 6-field cron (`sec min hour dom mon dow`).

| `opts` | Default | Meaning |
| --- | --- | --- |
| `name` | `cron[<spec>]` | job name, shown in logs |
| `overlap` | `false` | `false` skips a tick while the previous run is going |
| `timeout` | `--timeout` | ms; overrides the global per-event timeout |

```lua ignore
lur.serve.cron("0 */5 * * * *", function()
  lur.log.info("tick")
end, { name = "ticker", overlap = false })
```

#### `lur.serve.abort(status, message, data?)`

Ends the handler at once with an HTTP error.

- `status` must be an integer in `400..=599` (`abort(200, …)` is an argument error),
  `message` a string, `data` any JSON-encodable value.
- Uncaught, the client gets that status with `Content-Type: application/json` and
  `{"error": message, "data": data}` (`data` omitted when `nil`); not-encodable `data` is
  an ordinary error (a 500).
- It raises a table `{ kind = "abort", status, message, data }` that `pcall` can catch;
  side effects made before the abort (db writes, …) are **not** rolled back.
- Only valid inside an HTTP handler: in a cron job or one-shot script it is an ordinary
  runtime error.
- Raised inside a `lur.async` task, `pcall` around the combinator sees a string, not the
  table (use it in the handler itself).

```lua ignore
lur.serve.http("GET", "/items/:id", function(req)
  local id = tonumber(req.params.id)
  if id > 1000 then
    lur.serve.abort(410, "gone", { id = id })
  end
  return { body = "item " .. id }
end)
```

#### The `req` object

| Field | Meaning |
| --- | --- |
| `method`, `path` | request method and path |
| `origin`, `url` | `scheme://host` and the full URL, from `Host` and `X-Forwarded-Proto`; `nil` without a valid `Host`; client-controlled, so for links only |
| `params` | path params |
| `query` | last value per key |
| `query_all` | all values |
| `headers` | lowercased names |
| `cookies` | parsed `Cookie`; empty table if absent |
| `body` | raw bytes |
| `json()` | decoded JSON body |
| `form()`, `form_all()` | urlencoded body, see [Forms](#forms) |
| `read(n)` | streams large bodies in chunks; after that, `body`/`json()`/`form()`/`form_all()`/`valid_body` raise |
| `valid_query`, `valid_params`, `valid_body` | see [Validation helpers](#validation-helpers) |

##### Forms

`req.form()` parses an `application/x-www-form-urlencoded` body into `{key = last value}`;
`req.form_all()` returns `{key = {values...}}`, like `query` / `query_all`.

- The media type is matched case-insensitively; parameters such as `; charset=utf-8` are
  ignored.
- Decoding is the query-string decoding: `+` is a space, `%XX` is decoded, a malformed `%`
  stays literal, a key without `=` has value `""`, an empty body gives `{}`; `;` is not a
  separator and `%00` yields a NUL byte.
- The body is first read as lossy UTF-8, so invalid bytes in it become U+FFFD (bytes
  produced by `%XX` escapes are kept raw).
- Any other content type (or none) raises an ordinary error naming it, so branch on
  `req.headers["content-type"]` first if you accept several.
- Multipart is not supported. `--max-body` applies as usual.

```lua ignore
lur.serve.http("POST", "/search", function(req)
  local f = req.form()          -- { q = "luau", tag = "b" } for q=luau&tag=a&tag=b
  local all = req.form_all()    -- { q = { "luau" }, tag = { "a", "b" } }
  return { body = f.q .. " " .. #all.tag }
end)
```

##### Validation helpers

`req.valid_query(schema, opts?)`, `req.valid_params(schema, opts?)` and
`req.valid_body(schema, opts?)` run [`lur.validate`](#lurvalidate) over client input and
return the converted value (a fresh table, except `valid_body` may return `nil`; see
below).

| Helper | Reads |
| --- | --- |
| `req.valid_query` | `req.query` (and `req.query_all` for `list(...)` fields) |
| `req.valid_params` | `req.params` |
| `req.valid_body` | the request body, decoded by `Content-Type` |

- `opts.fail_status` is an integer `400..=599`, default `400`, checked at call time. On a
  validation failure they abort with that status, `message = "validation failed"` and
  `data = issues`:
  `{"error":"validation failed","data":[{"path":"limit","message":"must be <= 100"}]}`.
- Use `fail_status = 404` for path params.
- For an `object` schema, `list(...)` fields in `valid_query` read every value
  (`query_all`) and other fields the last one (`query`).
- Anything that is not a validation failure (a bad schema argument, a timeout) propagates
  unchanged.
- A bare `lur.validate.parse` that fails inside a handler stays a **500** on purpose: use
  `req.valid_*` for client input.

`valid_body` picks the decoder from `Content-Type`:

- Only `application/json` and `*+json` types count as JSON (so `text/json` is a 415) and
  decode as `req.json()`.
- Urlencoded forms are validated with `list(...)` fields of an `object` schema fed from
  `form_all`.
- With several `Content-Type` headers the last one wins, as for `req.headers`.
- A JSON `null` (top level or as an object field) counts as absent, as in
  [Missing values and defaults](#missing-values-and-defaults).

| Case | Status | Message | Notes |
| --- | --- | --- | --- |
| Any other content type, including a missing type and `multipart/*` | **415** | `unsupported content type` | `data = { content_type }` (no `data` when the header is missing) |
| Body is not valid JSON | **400** | `invalid JSON body` | always 400, regardless of `fail_status` |
| Empty JSON body | **400** | `invalid JSON body` | always 400, unless the schema accepts `nil` (`v.optional(...)`), in which case it returns `nil` |
| Validation failure | `opts.fail_status` (default 400) | `validation failed` | `data = issues` |

```lua ignore
lur.serve.http("GET", "/users/:id", function(req)
  local v = lur.validate
  local p = req.valid_params(v.object({ id = v.int({ min = 1 }) }), { fail_status = 404 })
  return { body = "user " .. p.id }
end)

lur.serve.http("GET", "/users", function(req)
  local v = lur.validate
  local q = req.valid_query(v.object({
    limit = v.int({ min = 1, max = 100, default = 20 }),
    tag = v.optional(v.list(v.str())), -- absent => nil (a plain list is required)
  }))
  if not q.tag then lur.serve.abort(422, "need at least one tag") end
  return { body = lur.json.encode(q) }
end)

lur.serve.http("POST", "/signup", function(req)
  local v = lur.validate
  -- JSON or urlencoded; anything else is a 415
  local b = req.valid_body(v.object({
    email = v.str(),
    age = v.int({ min = 13 }),
    interests = v.optional(v.list(v.str())),
  }), { fail_status = 422 })
  return { status = 201, body = lur.json.encode(b) }
end)
```

## Diagnostics

Errors show the script path, failing line, and a source snippet (rustc-style), then a
stack traceback. Server handler and cron errors render the same way (a handler error
still returns `500`). Capability argument mistakes name the function, e.g.
`lur.crypto.sha256: argument #1 must be string, got table`.

Output is colorized only on a terminal; a non-empty [`NO_COLOR`](https://no-color.org)
disables it.
