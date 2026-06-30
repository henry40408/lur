# lur guide

`lur` runs Luau in a sandbox. Two modes share one core: one-shot
`lur script.lua [args]` runs a script to completion; `lur serve app.lua` serves
it as a long-running HTTP server. Capabilities live under the `lur.*` global;
each is gated by a policy (default profile is `strict` — deny-all). See the
[README](../README.md) for the full flag set and the sandbox model.

Every example below is run as part of the test suite, so it stays correct.

## Data & I/O

### lur.json

Encode/decode JSON. JSON `null` becomes `lur.null` (a sentinel distinct from
`nil`, since a `nil` value means the key is absent); UTF-8 only — base64 binary.

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

Pure-compute hashing, HMAC, hex, CSPRNG bytes, and constant-time compare.
Digests are raw bytes — bridge through `hex` or `lur.base64`. `sha1`/`md5` are
legacy-interop only.

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

Parse a `Cookie` header into a table; build one `Set-Cookie` value. Values are
raw bytes (base64 arbitrary data).

```lua
local jar = lur.cookie.parse("a=1; b=2")
assert(jar.a == "1" and jar.b == "2")

local set = lur.cookie.serialize("sid", "xyz", { http_only = true, path = "/" })
assert(set:find("sid=xyz", 1, true) == 1)
assert(set:find("HttpOnly", 1, true))
```

### lur.time

Clocks and timestamp parsing that fill the gaps in `os.*`. All values are
integer milliseconds.

```lua
assert(lur.time.now_ms() > 0)

local a = lur.time.monotonic_ms()
local b = lur.time.monotonic_ms()
assert(b >= a)

assert(lur.time.parse_rfc3339("1970-01-01T00:00:01Z") == 1000)
assert(lur.time.parse_http_date("Thu, 01 Jan 1970 00:00:01 GMT") == 1000)
```

### lur.log

`info`/`warn`/`error` write to **stderr** (stdout is the data channel); each
call emits `<level>: <msg>\n`.

```lua
lur.log.info("starting\n")
lur.log.warn("careful\n")
lur.log.error("oops\n")
```

### lur.io

`lur.stdout.write(bytes)` / `flush()` is the data channel (raw bytes, no
newline). `lur.stdin.read()` drains all input, `read(n)` reads up to `n` (`nil`
at EOF), and `lines()` iterates newline-stripped lines.

```lua
lur.stdout.write("data\n")
lur.stdout.flush()
```

```lua ignore
-- Reading stdin needs piped input; run as: echo hi | lur read.lua
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

Process-wide shared state across the VM pool (primitives only): `get`/`set`
(`nil` deletes), `incr` (atomic add), `update` (optimistic CAS).

```lua
lur.state.set("hits", 0)
assert(lur.state.incr("hits", 2) == 2)
lur.state.update("hits", function(n) return (n or 0) + 1 end)
assert(lur.state.get("hits") == 3)
lur.state.set("hits", nil)
assert(lur.state.get("hits") == nil)
```

## Capabilities (policy-gated)

### lur.fs
### lur.env
### lur.http

## Storage

### lur.db
### lur.kv

## Concurrency

### lur.async

`sleep(ms)` and combinators over arrays of zero-arg functions: `all` (fail-fast),
`race`/`any` (first to settle/succeed), `settled` (never raises). Lua runs one
step at a time; tasks interleave at I/O await points.

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
```

## Server mode

### lur.serve
