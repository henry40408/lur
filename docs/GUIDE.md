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

### lur.io

`lur.stdout.write(bytes)` / `lur.stdout.flush()` is the data channel.
`lur.stdin.read()` drains all input.

## State & arguments

### lur.args
### lur.state

## Capabilities (policy-gated)

### lur.fs
### lur.env
### lur.http

## Storage

### lur.db
### lur.kv

## Concurrency

### lur.async

## Server mode

### lur.serve
