# lur guide

`lur` runs Luau in a sandbox: `lur script.lua [args]` runs once;
`lur serve app.lua` runs an HTTP server. Capabilities live under `lur.*`, gated
by a policy (default `strict` — deny-all). Flags and the sandbox model are in
the [README](../README.md).

Every example below runs in the test suite.

## Data & I/O

### lur.json

JSON `null` decodes to `lur.null` (distinct from `nil`, which means absent).
UTF-8 only — base64 binary first.

```lua
local s = lur.json.encode({ ok = true, n = 3 })
local v = lur.json.decode(s)
assert(v.ok == true and v.n == 3)
assert(lur.json.decode("null") == lur.null)
```

### lur.base64

```lua
local enc = lur.base64.encode("hi")
assert(enc == "aGk=")
assert(lur.base64.decode(enc) == "hi")
```

### lur.crypto

Hashing, HMAC, hex, CSPRNG bytes, constant-time compare. Digests are raw
bytes; `sha1`/`md5` are legacy-interop only.

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

```lua
local jar = lur.cookie.parse("a=1; b=2")
assert(jar.a == "1" and jar.b == "2")

local set = lur.cookie.serialize("sid", "xyz", { http_only = true, path = "/" })
assert(set:find("sid=xyz", 1, true) == 1)
assert(set:find("HttpOnly", 1, true))
```

### lur.html / lur.feed

Scrape with CSS selectors; emit a feed.

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

`lur.feed.rss` / `.atom` / `.json` take `(meta, items)`; item `date` is epoch milliseconds.

### lur.time

Clocks and timestamp parsing missing from `os.*`, in integer milliseconds.

```lua
assert(lur.time.now_ms() > 0)

local a = lur.time.monotonic_ms()
local b = lur.time.monotonic_ms()
assert(b >= a)

assert(lur.time.parse_rfc3339("1970-01-01T00:00:01Z") == 1000)
assert(lur.time.parse_http_date("Thu, 01 Jan 1970 00:00:01 GMT") == 1000)
```

Timezone-aware formatting and lenient parsing (`tz` is an IANA name, `"+08:00"`, or
`nil` for UTC). Wall-clock text with no offset is read in `tz`.

```lua
local ms = lur.time.parse("2026-10-05 14:30", nil, "Asia/Taipei")
assert(ms == lur.time.parse("2026-10-05T06:30:00Z"))
assert(lur.time.format_rfc3339(ms, "Asia/Taipei") == "2026-10-05T14:30:00.000+08:00")
assert(lur.time.format_rfc2822(ms) == "Mon, 5 Oct 2026 06:30:00 +0000")
assert(lur.time.format(ms, "%Y/%m/%d %H:%M", "Asia/Taipei") == "2026/10/05 14:30")
assert(lur.time.parse_rfc2822("Thu, 01 Jan 1970 00:00:01 +0000") == 1000)
assert(lur.time.parse("05/10/2026", "%d/%m/%Y") == lur.time.parse("2026-10-05"))
```

### lur.url

Parse and build URLs; resolve the relative links found in scraped pages.

```lua
local u = lur.url.parse("https://example.com:8443/a?x=1#top")
assert(u.host == "example.com" and u.port == 8443 and u.query == "x=1")

assert(lur.url.join("https://e.com/blog/1", "../about") == "https://e.com/about")

local q = lur.url.encode_query({ b = "x y", a = { 1, 2 } })
assert(q == "a=1&a=2&b=x+y")
assert(lur.url.decode_query("?b=x+y").b == "x y")
```

### lur.log

`info`/`warn`/`error` write `<level>: <msg>\n` to **stderr** (stdout is the
data channel).

```lua
lur.log.info("starting")
lur.log.warn("careful")
lur.log.error("oops")
```

### lur.io

`lur.stdout.write(bytes)` / `flush()` write raw bytes (no newline).
`lur.stdin.read()` drains input, `read(n)` reads up to `n` (`nil` at EOF),
`lines()` iterates newline-stripped lines.

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

## State & arguments

### lur.args

`lur.args.positional` is a 1-indexed array; `lur.args.flags` maps
`--name value` / `--name=value` to the string and a bare `--flag` to `true`.

```lua
assert(type(lur.args.positional) == "table")
assert(type(lur.args.flags) == "table")
```

### lur.state

Process-wide state shared across the VM pool (primitives only): `get`/`set`
(`nil` deletes), `incr`/`decr` (atomic), `update` (optimistic CAS), `cas`
(compare-and-set), `add` (set-if-absent).

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

## Capabilities (policy-gated)

### lur.fs

`read(path) → bytes`, `write(path, bytes)`. Paths are canonicalized before the
allowlist check, so `..`/symlink escapes fail. Grant with `--allow-fs-read`/
`--allow-fs-write`/`--allow-fs` (or `-A`).

```lua
lur.fs.write("./note.txt", "hello")
assert(lur.fs.read("./note.txt") == "hello")
```

### lur.env

`lur.env(name) → string | nil` — `nil` for **both** denied and unset, so it is
not an oracle. Grant with `--allow-env` (or `-A`).

```lua
assert(lur.env("LUR_GUIDE_DEFINITELY_UNSET") == nil)
```

### lur.http

`request(method, url, opts?)` plus `get`/`post`/`put`/`patch`/`delete`/`head`.
`opts` may set `headers`, `query`, `body` **or** `json`, and `timeout` (ms).
Returns `{ status, body, headers, headers_all, json() }`. Each request and hop is
checked against the allowlist and SSRF guard; grant hosts with `--allow-net`.

`cache = { ttl_ms = … }` (GET only, needs `--db`) serves repeat requests from
`lur.kv` and sets `res.cached` (`true` on a hit). The policy check runs before
the lookup. Only 2xx responses are stored, and not ones that set a cookie. The key is
the method, final URL (query included) and all request headers; a request carrying
`Authorization`, `Cookie` or `Proxy-Authorization` bypasses the cache unless that
header is named in `vary`, e.g. `cache = { ttl_ms = 60000, vary = { "authorization" } }`.

```lua ignore
local res = lur.http.get("https://example.com", { timeout = 5000 })
assert(res.status == 200)

local posted = lur.http.post("https://api.example.com/items", {
  json = { name = "widget" },
})
local body = posted.json()

-- `request` takes any method
lur.http.request("OPTIONS", "https://api.example.com/items")
lur.http.put("https://api.example.com/items/1", { json = { name = "v2" } })
lur.http.patch("https://api.example.com/items/1", { json = { name = "v3" } })
lur.http.delete("https://api.example.com/items/1")
local probe = lur.http.head("https://example.com")
assert(probe.status == 200)

-- cached for 5 minutes (requires --db)
local feed = lur.http.get("https://example.com/feed.xml", { cache = { ttl_ms = 300000 } })
assert(feed.cached == false or feed.cached == true)
```

## Storage

### lur.db

Requires `--db`. `exec(sql, ...params) → { rows_affected, last_insert_id }`;
`query(sql, ...params)` → array of rows keyed by column; `tx(fn)` runs on a
pinned connection (commit on return, rollback on error). SQLite uses `?`
placeholders.

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
`get`/`set`/`delete`, plus atomic `add` (set-if-absent), `cas`
(compare-and-swap), `incr`/`decr` (integer counters), `update`
(read-modify-write), and expiry via `ttl_ms` (see below).

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
-- counters (incr/decr) are stored as integers, so cas never matches them

-- incr/decr: start from 0; optional step
assert(lur.kv.incr("hits")    == 1)
assert(lur.kv.incr("hits", 4) == 5)
assert(lur.kv.decr("hits", 2) == 3)

-- update: return the new value, or nil to delete. Inside the transform a
-- nested lur.kv call raises; a lur.db write blocks on the lock, so avoid it.
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

- `set(k, v)` without `ttl_ms` **clears** any expiry, like Redis `SET`.
- `cas` and `update` without `ttl_ms` **keep** the existing expiry; pass `ttl_ms` to replace it.
- `incr`/`decr` apply `ttl_ms` only when the key has no expiry yet, so the window
  opens at the first hit and later hits don't extend it (a fixed window). Add
  `renew_ttl = true` to reset the expiry on every call instead (idle timeout).
  A counter that predates expiries picks one up on its next `incr` with `ttl_ms`.
- `expire(k, ms) → bool` sets an expiry on a live key; `ttl(k) → ms, exists` reports
  it: `nil, false` absent · `nil, true` never expires · `ms, true` expiring.

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

Always pass `ttl_ms` to `incr` for a rate limit: without it the counter never resets.

### Postgres backend

`--db postgres://…` (or `postgresql://`) selects Postgres. Nothing is
translated: placeholders are `$1, $2, …`, and only core scalar types read back
(cast others, e.g. `col::text`). `db.tx` and `kv.update` run at `SERIALIZABLE`
and may raise on conflict — wrap them in `pcall` or your own retry:

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

`sleep(ms)` and combinators over arrays of zero-arg functions: `all`
(fail-fast), `race`/`any` (first to settle/succeed), `settled` (never raises).
Tasks interleave only at I/O awaits.

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

## Server mode

### lur.serve

Only under `lur serve`; registration happens once at load.
`serve.http(method, path, handler)` — `:name` path segments bind into
`req.params`; the handler returns `{ status?, headers?, body? }`, where a
header value is a string or an array of strings. `serve.cron(spec,
handler, opts?)` takes a 6-field cron spec and optional `name`/`overlap`/
`timeout`. `req` has `method`, `path`, `params`, `query`, `query_all`,
`headers`, `cookies`, `body`, `json()`, and `read(n)`.

```lua ignore
lur.serve.http("POST", "/echo", function(req)
  local data = req.json()
  return {
    headers = { ["Content-Type"] = "application/json" },
    body = lur.json.encode(data),
  }
end)

lur.serve.cron("0 */5 * * * *", function()
  lur.log.info("tick")
end, { name = "ticker", overlap = false })
```
