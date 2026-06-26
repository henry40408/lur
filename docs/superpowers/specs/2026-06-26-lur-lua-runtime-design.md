# lur — Lua Script Runtime Design

- **Date**: 2026-06-26
- **Status**: Design under review
- **Name**: `lur` (Lua Runtime) — the binary name and the Lua global prefix are the same
- **Language/Edition**: Rust (edition 2024)

## 1. Purpose & Scope

`lur` is a Lua script runtime written in Rust. The goal is to let the user write
work in Lua and have `lur` execute it safely. It covers two execution modes:

1. **one-shot mode** (`lur script.lua`) — run-once automation / glue scripts:
   call APIs, process files, ETL, run once when triggered by a scheduler.
2. **server mode** (`lur serve app.lua`) — long-running, event-driven services:
   HTTP handlers, scheduled jobs (cron-like), and future-extensible socket / queue
   sources.

Design principles: **flexibility first, sandbox second but mandatory**. Scripts come
from both "written by me (semi-trusted)" and "from others / the internet (untrusted)",
so two switchable profiles are provided: `loose` / `strict`. Follow YAGNI: v1 only
builds explicitly-needed capabilities; everything else is reserved behind trait
interfaces.

### Non-goals (out of scope for v1)

- OS-level sandbox hardening (landlock/seccomp/sandbox-exec) — interface reserved, not implemented.
- Storage backends other than SQLite (e.g. Postgres) — trait reserved, not implemented.
- socket / message-queue trigger sources — `Source` trait reserved, not implemented.
- Advanced custom exit-code protocol — optional, not done by default.

## 2. Overall Architecture

One binary, two execution modes, shared core.

```
Shared core: Lua VM (mlua / Luau backend, sandboxed — see §14) · host modules (lur.*) · policy/sandbox
             · tokio async · state layer (short-term + long-term)
   ├─ one-shot mode   lur script.lua       build a single VM, exit when done
   └─ server mode     lur serve app.lua    build a VM pool, long-running event loop
```

### Module breakdown (each single-purpose, clear interface, independently testable)

| Module | Responsibility | Depends on |
|---|---|---|
| `cli` | Parse args, select profile, set up stdin/stdout, dispatch mode | `clap` |
| `policy` | `Policy` struct: capability switches, fs/network allowlist, resource limits; `loose`/`strict` presets | — |
| `runtime` | Owns `mlua::Lua` (safe) + tokio; VM lifecycle; injects `lur.*`; wires timeout/memory/interrupt | `mlua`, `tokio` |
| `host` | The `lur.*` capability modules; each takes `Policy` to gate | `reqwest`, etc. |
| `state` | Short-term (in-memory) + long-term (storage backend) state | `dashmap`, `sqlx` |
| `serve` | Server mode: `Source` trait (http/cron/...) driving the event loop + VM pool | `axum`/`hyper`, `cron` |
| `output` | Map the script's `return` value to an exit code; stdout stays script-controlled | — |

## 3. Execution Modes

### one-shot
1. `cli` parses `lur [OPTIONS] script.lua [args...]` and determines the profile.
2. `runtime` builds a single Lua VM and injects `lur.*` per `Policy`.
3. Read stdin (if the script uses it).
4. `pcall` the script chunk; stdout is whatever the script wrote (`print` /
   `lur.stdout.write`), byte-for-byte.
5. Map the script's `return` value to an exit code (see §8). The return value is **not**
   auto-printed — following the Unix convention (like `lua`/`python`/shell), stdout is
   entirely script-controlled.
6. Flush stdout and exit.

Input channels: CLI args + flags (`lur.args`), stdin (`lur.stdin`), environment
variables (`lur.env`, allowlisted).
Output channels: stdout (script-controlled, byte-safe, via `print` / `lur.stdout.write`),
stderr (`lur.log`), exit code (mapped from the return value). To emit a structured
result, the script does it explicitly: `print(lur.json.encode(result))`.

### server
1. `cli` parses `lur serve [OPTIONS] app.lua`.
2. `serve` builds a **VM pool** (a pre-warmed set of stateless Lua states used round-robin).
   Probe-verified: with mlua's `send` feature a pooled VM can be checked out across a multi-threaded
   tokio runtime (held exclusively per use) and run async Lua, across distinct worker threads.
3. Run `app.lua` once to collect handler registrations (`lur.serve.http(...)`, etc.).
4. Start each `Source` (HTTP listener, cron scheduler, ...) and enter the event loop.
5. On an incoming event → borrow a VM from the pool → run the matching handler →
   return the VM to the pool.
6. All cross-request / cross-event state lives host-side (see §6); the VM itself is stateless.

#### HTTP routing & the `req` object

Routes are declared `lur.serve.http(method, path, handler)`:

- **method** is matched case-insensitively and normalized to upper case (`"get"` ≡ `"GET"`);
  `"ANY"` is the wildcard that matches every method. To serve several specific methods,
  register the handler more than once (no list syntax in v1 — YAGNI).
- **path** uses `:name` segments for path parameters, e.g. `/users/:id`.
- **route resolution**: a **static segment beats a dynamic `:param`** (`/users/me` wins over
  `/users/:id`), so matching never depends on registration order. Registering the **same
  `(method, path)` twice is an error at load time** (fail-fast, no silent last-wins). A request
  matching **no** route gets an automatic **404** (distinct from a registration conflict).

The handler receives one `req` table and returns `{ status, body, headers }`:

| `req` field | Value |
|---|---|
| `req.method` | the request method, upper-cased (`"GET"`) |
| `req.path` | the request path (`"/users/42"`) |
| `req.params.NAME` | path parameters from `:name`; **percent-decoded to raw bytes**, always strings (no `tonumber`) |
| `req.query.NAME` | query parameters; **case-sensitive** (`?Foo` ≠ `?foo`); repeated keys → last value, full list via `req.query_all.NAME` |
| `req.headers.NAME` | headers; **case-insensitive, keys normalized to lower case** — read e.g. `req.headers["content-type"]` |
| `req.body` | the **full** request body as raw bytes (sugar for `req.read()`); bounded by `limits.max_body` |
| `req.read([n])` | mirrors `lur.stdin.read([n])`: no arg → read the whole body; `n` → read up to `n` bytes; EOF → `nil` |
| `req.json()` | explicit shorthand for `lur.json.decode(req.body)` — decoded only when called, no `Content-Type` magic; materializes the full body, so it can't be combined with chunked `req.read(n)` |

`req.body` and chunked `req.read(n)` are **mutually exclusive** — the body is a one-shot stream,
so once `read(n)` has consumed part of it `req.body` is no longer available. A body larger than
`limits.max_body` is rejected at the host edge with **413 Payload Too Large** before the handler
runs, so the VM never allocates it; to read a very large body whole, raise `--max-body` (and accept
the memory cost), otherwise stream it with `req.read(n)` to a sink. Streaming response bodies are a
reserved extension, not in v1.

#### Cron scheduling

Jobs are declared `lur.serve.cron(spec, handler[, opts])`. The cron `Source` is built on the
**`cron` crate** as a pure next-fire calculator — lur owns the timer loop and feeds fires into its
own event loop / VM pool, rather than delegating to a separate scheduler (rationale in §10).

- **Expression grammar** — **6-field** `sec min hour dom mon dow` (seconds-precision, verified by
  probe). Ranges/lists/steps/named days (`MON-FRI`) and the macros `@hourly`/`@daily`/`@weekly`/
  `@monthly`/`@yearly` are supported natively. Note this is **not** 5-field crontab — `"0 * * * *"`
  is invalid; hourly is `"0 0 * * * *"`. An interval syntax (`@every 30s`, a Go-cron-ism, not
  standard and not a cron expression) is **reserved**; for now use seconds-cron (`*/30 * * * * *`).
- **Timezone** — fires are computed in **UTC** by default; a global `--timezone`/config setting
  selects an IANA zone. Per-job timezone and DST-aware named-zone caveats (skipped/duplicated
  wall-clock times across transitions) are **reserved** until that lands.
- **Overlap — single-flight by default** (`opts.overlap = false`): if the previous run of the same
  job is still in flight when the next tick is due, that tick is **skipped**, not queued. Jobs that
  are safe to run concurrently set `opts.overlap = true`. The job's identity for this tracking is
  `opts.name` (auto-generated from the spec if omitted; the name also tags logs).
- **Missed ticks — fire-forward, no backfill.** The schedule is **in-memory** (process lifetime):
  lur always computes the *next future* fire from now, so ticks missed during downtime, delay, or an
  overrun are **not replayed** (no thundering herd on restart). This is best-effort wall-clock
  scheduling, **not a durable job queue**; durable/persisted schedules and missed-run replay (which
  could lean on §6 SQLite) are **reserved**.
- **Errors & timeout** — a handler error is caught and logged via `lur.log.error` (tagged with the
  job name) and **must not bring down the server** (§8); there is **no auto-retry** (the next tick is
  the retry). A job is bounded by `per_event_timeout` (or `opts.timeout`); on timeout the §5
  interrupt aborts it and frees the VM.
- **Scope & lifecycle** — `lur.serve.*` (cron included) is **valid only under `lur serve`**; calling
  it in one-shot mode is a registration error. Cron jobs draw from the shared VM pool and count
  against `max_concurrency` (a dedicated cron budget + jitter is reserved). On shutdown, scheduling
  stops and in-flight jobs **drain** within the grace period before being aborted.

## 4. `lur.*` API Surface (everything flat under `lur.`)

The only global is `lur`. Categorized below (all on the same level):

```lua
-- per-run / context (data that changes per execution)
lur.args.flags.NAME       -- --name value (table, read-only)
lur.args.positional       -- positional-argument array
lur.stdin.read()          -- read all of stdin (raw bytes as a Lua string)
lur.stdin.read(n)         -- read up to n bytes; n=1 reads byte-by-byte (binary-safe)
lur.stdin.lines()         -- line-by-line iterator
lur.stdout.write(data)    -- write raw bytes to stdout, no newline, binary-safe
lur.stdout.flush()        -- flush stdout
lur.env("API_KEY")        -- allowlisted environment variable

-- capability modules (policy-gated); see §4 "HTTP client" for opts/response detail
lur.http.request(method, url[, opts])           -- core; async
lur.http.get/post/put/patch/delete/head(url[, opts]) -- sugar over request()
--   opts  = { headers, query, body | json, timeout, redirect }
--   reply = { status, body(raw bytes), headers, headers_all, json() }
lur.fs.read(path)         -- path allowlist enforced; returns raw bytes
lur.fs.write(path, data)  -- writes raw bytes as-is
lur.log.info/warn/error() -- writes to stderr (stdout is the script's data channel)

-- pure functions (always safe)
lur.json.encode/decode()
lur.base64.encode/decode()

-- state (see §6)
lur.state.get/set/incr/update()   -- short-term, host memory, shared, atomic; primitive values only (§6)
lur.db.query/exec/tx()            -- long-term, raw SQL (? params); tx(fn) = pinned-conn transaction
lur.kv.get/set/delete()           -- long-term, persistent KV
lur.null                          -- singleton sentinel for SQL NULL / JSON null (≠ nil; see §6)

-- server registration (server mode only); see §3 for the req/response shape
lur.serve.http(method, path, handler) -- method "GET".."ANY", path "/users/:id"; handler(req) -> { status, body, headers }
lur.serve.cron(spec, handler[, opts]) -- spec = 6-field cron "sec min hour dom mon dow"; handler(); opts{ name, overlap, timeout }
-- lur.serve.socket(...) etc. for future extension

-- concurrency (run multiple Lua functions concurrently; see §7)
lur.async.all({ fn1, fn2, ... })     -- all must succeed → results in order, else re-raise the first error
lur.async.race({ fn1, fn2, ... })    -- first to settle wins (ok or err) → its result, or raise if it errored; rest cancelled
lur.async.settled({ fn1, fn2, ... }) -- wait for all, never raises → per-task { ok=true, value=… } / { ok=false, err=… }
lur.async.any({ fn1, fn2, ... })     -- first to *succeed* wins; if all fail → raise an aggregate error; rest cancelled
```

### Removed / restricted native libraries

- **strict**: remove `os.execute`, `io`, `package`, `require`, `loadfile`, `dofile`,
  `load`, `loadstring`, `getfenv`, `setfenv`, `os.getenv`, `os.remove`, `os.rename`, etc. Keep
  pure libs `string`/`table`/`math`/`utf8`/`coroutine`/`print` (`coroutine` is pure control flow —
  no ambient I/O — and underpins the async layer, see §7; `print` goes to stdout).
  **Probe finding — `sandbox(true)` is *not* sufficient by itself**: it drops `os.execute`,
  `os.getenv`, `io`, `package`, `load`, `dofile`, `loadfile`, but **leaves `require`, `loadstring`,
  `getfenv`, `setfenv` present** (and keeps pure `os.time`/`collectgarbage`/`bit32`). These are not
  equally dangerous — only one is a real hole:
  - **`require` *is* a capability escape** — probe-verified it carries an active filesystem resolver
    (`./`, `../`, `@` prefixes) and **loaded + executed an on-disk `.luau` file**, i.e. ambient
    read-and-execute that bypasses `lur.fs`. lur's clean-environment step (§5.1) **must** remove it.
    Mechanics (probe-verified): there is no Cargo flag for this, but **(v1)** dropping the global —
    `require = nil` before `sandbox(true)` — removes it cleanly, and **(future)** mlua's
    `Lua::create_require_function` + the `Require` trait can install a deny-all or **policy-gated**
    resolver (a deny-all returns *"require is not supported in this context"*) — the natural future
    replacement that gates module loading by policy instead of dropping it.
  - **`loadstring`, `getfenv`, `setfenv` are *not* capability escapes** — probe-verified that the code
    and environments they yield still **cannot see `io`/`os`** (they expose only a writable but
    capability-free global namespace). lur's security is the capability boundary (`lur.*`), not hiding
    language features, so these grant nothing extra; lur strips them anyway for least-surface / defense-in-depth.
- **loose**: optionally restore parts of `os`/`io` (still recommend the managed `lur.*` versions).

Key point: **removing `io`/`os` does not remove I/O capability** — it removes
"unmanaged ambient access" and re-provides the same operations through policy-managed
`lur.fs` / `lur.stdin` / `lur.http`. stdin/stdout are fixed pipes the host opens and
hands to the script; the script cannot choose "which fd / which file to open", so they
remain safe to provide even under strict.

### Byte semantics & Unicode

The runtime is **byte-transparent by default; Unicode is opt-in**.

- A **Lua string is an arbitrary byte sequence with no encoding**. mlua maps strings
  to/from Rust as bytes in both directions and never transcodes implicitly (probe-verified: a
  `NUL` + invalid-UTF-8 byte string round-trips identical, length intact).
- All data-carrying APIs are **binary-safe / encoding-agnostic**: `lur.stdin.read[(n)]`,
  `lur.fs.read`, `lur.http` response bodies, and — in server mode — the request body
  (`req.body` / `req.read[(n)]`) return **raw bytes**; `lur.stdout.write`, `lur.fs.write`, and
  a handler's returned `body` write the bytes as-is. No UTF-8 validation is performed on these paths.
- **HTTP request fields carry no encoding either**: `req.params.*` and `req.query.*` are
  **percent-decoded to raw bytes** — not assumed to be UTF-8 and not validated; `req.headers.*`
  values are exposed as bytes. lur **never transcodes based on a `Content-Type: charset`** — a
  Shift_JIS / Big5 body arrives as the original bytes. v1 ships **no charset conversion** (iconv
  is a non-goal); a script that needs codepoints validates with `utf8.*`, and the only UTF-8
  assumption remains the JSON boundary below.
- For **text / codepoint handling**, scripts use the built-in `utf8` library (present in
  both PUC Lua 5.4 and the Luau backend; kept as a pure, safe native): `utf8.len`,
  `utf8.char`, `utf8.codepoint`, `utf8.codes`.
- **The only place UTF-8 is assumed is the JSON boundary.** JSON is defined as UTF-8, so
  `lur.json.encode` requires strings to be valid UTF-8 and errors on invalid bytes. To put
  binary data into JSON, the script must `lur.base64.encode` it first.

In one sentence: `lur` passes strings around as bytes everywhere and only assumes UTF-8 at
`lur.json.encode` and within the `utf8.*` library.

### HTTP client

`lur.http.request(method, url[, opts])` is the core call; `get`/`post`/`put`/`patch`/`delete`/`head`
are sugar over it (rarer methods go through `request`). Every request **and every redirect hop** is
validated against the §5 network allowlist + private-network deny.

**Request `opts`:**

| Field | Meaning |
|---|---|
| `headers` | name→value table, sent as given |
| `query` | table appended as a percent-encoded query string (`{q="lur"}` → `?q=lur`) |
| `body` | raw byte body (§4 byte-transparent); mutually exclusive with `json` |
| `json` | a Lua value encoded as a JSON body + `content-type: application/json` (hits the UTF-8 boundary) |
| `timeout` | per-request timeout, overriding the default (whole-script deadline / interrupt: §5) |
| `redirect` | `"follow"` (default; capped hop count, each hop re-checked) / `"manual"` / `"error"` |

`form`-urlencoded bodies, retries, a cookie jar, and proxies are **reserved** (explicit-first, YAGNI).

**Response:** `{ status, body, headers, headers_all, json() }`

- `status` — number; `body` — **raw bytes** (no auto-decoding).
- `headers` — **case-insensitive, lower-cased keys** (`res.headers["content-type"]`), matching the
  server `req.headers` convention (§3); `headers_all["set-cookie"]` is the array form for repeated headers.
- `res.json()` — **explicit** shorthand for `lur.json.decode(res.body)`, decoded **only when called**
  and **without** consulting `Content-Type` (the script asserts it is JSON; errors like `lur.json.decode`
  on invalid JSON / non-UTF-8). The runtime never auto-parses a body. The server side mirrors this with
  `req.json()`.

**Safety / resource limits:**

- **TLS verification is always on**; a script cannot disable certificate checks via `opts` (that is a
  host-level policy decision, not a capability the untrusted script holds). A policy-level opt-out is
  **reserved**.
- The response body is **buffered with a size cap** (`--max-http-body`) so a large download can't blow
  the VM memory cap; exceeding it errors. **Streaming downloads** (`res.read(n)` / writing straight to
  `lur.fs`) are **reserved**, symmetric with streaming response bodies on the server side.

### Example scripts

What a user actually writes — ordinary Lua, but all I/O flows through `lur.*` instead of
`os`/`io` (which strict removes, §4). These tie together the modes (§3), state (§6), and the
concurrency family (§7).

**one-shot — Unix filter (stdin → stdout):**

```lua
-- top_ips.lua  ──  cat access.log | lur top_ips.lua | sort -rn | head
local counts = {}
for line in lur.stdin.lines() do
  local ip = line:match("^(%d+%.%d+%.%d+%.%d+)")
  if ip then counts[ip] = (counts[ip] or 0) + 1 end
end
for ip, n in pairs(counts) do
  print(n .. "\t" .. ip)          -- stdout is the data channel; format is the script's call
end
-- no return → exit 0
```

**one-shot — call an API, emit a structured result:**

```lua
-- fetch.lua  ──  lur --allow-net api.github.com fetch.lua -- --repo cli/cli
local repo = lur.args.flags.repo or error("need --repo")
local res = lur.http.get("https://api.github.com/repos/" .. repo)
if res.status ~= 200 then
  lur.log.error("github returned " .. res.status)   -- diagnostics go to stderr
  return res.status                                 -- return number → that exit code (§8)
end
local data = res.json()                                    -- explicit shorthand for lur.json.decode(res.body)
print(lur.json.encode({ stars = data.stargazers_count }))  -- result is print'd for `jq` to consume
```

**one-shot — fan out four ways (`lur.async.all` / `.race` / `.settled` / `.any`):**

```lua
-- concurrency.lua
-- all: every fetch must succeed; fail-fast, results in order
local pages = lur.async.all({
  function() return lur.http.get("https://a.example/p").body end,
  function() return lur.http.get("https://b.example/p").body end,
})

-- race: whichever mirror settles first wins (success or failure); the rest are cancelled
local fastest = lur.async.race({
  function() return lur.http.get("https://mirror1.example/file").body end,
  function() return lur.http.get("https://mirror2.example/file").body end,
})

-- any: first mirror to *succeed* wins; raises only if every mirror fails
local ok = lur.async.any({
  function() return lur.http.get("https://primary.example/file").body end,
  function() return lur.http.get("https://backup.example/file").body end,
})

-- settled: probe many endpoints, keep going even if some fail
local checks = lur.async.settled({
  function() return lur.http.get("https://a.example/health").status end,
  function() return lur.http.get("https://b.example/health").status end,
})
for i, r in ipairs(checks) do
  if r.ok then lur.log.info("endpoint " .. i .. " -> " .. r.value)
  else         lur.log.warn("endpoint " .. i .. " failed: " .. tostring(r.err)) end
end
```

**server — register handlers (running `app.lua` only collects registrations, §3):**

```lua
-- app.lua  ──  lur serve --bind 127.0.0.1:8080 --db ./app.db app.lua
lur.serve.http("GET", "/users/:id", function(req)
  local n = lur.state.incr("hits")            -- cross-request, atomic, host-side (§6)
  return { status = 200, body = "user " .. req.params.id .. ", hit #" .. n }
end)

lur.serve.http("POST", "/save", function(req)
  if req.headers["content-type"] ~= "application/json" then return { status = 415 } end
  lur.kv.set("last", req.body)                 -- req.body = full body as raw bytes (§6)
  return { status = 201 }
end)

lur.serve.http("ANY", "/webhook", function(req) -- ANY matches every method
  lur.log.info(req.method .. " " .. req.path .. "?q=" .. tostring(req.query.q))
  return { status = 204 }
end)

lur.serve.cron("0 0 * * * *", function()       -- 6-field: top of every hour (sec min hour dom mon dow)
  lur.log.info("hourly tick, hits=" .. (lur.state.get("hits") or 0))
end)

lur.serve.cron("*/30 * * * * *", sync, {       -- every 30s; single-flight + named for logs
  name = "inventory-sync", overlap = false, timeout = "20s",
})
```

## 5. Sandbox (Approach A: capability injection, in-process)

Both modes share the same enforcement:

1. **Clean environment**: do not pollute `_G`; give the script a custom global table
   (only `lur` + allowlisted pure libs) and run the chunk under mlua's environment mechanism.
   On the Luau backend, `Lua::sandbox(true)` additionally makes the base libraries readonly
   and drops dangerous stdlib (e.g. `os.execute` → nil) at the VM level (see §14). **It is not
   sufficient alone** — probe-verified that `sandbox(true)` leaves `require`/`loadstring`/`getfenv`/
   `setfenv` reachable. Of those, `require` is a **genuine hole** (it loaded an on-disk `.luau` file,
   bypassing `lur.fs`) and must be removed here; `loadstring`/`getfenv`/`setfenv` are capability-neutral
   (can't reach `io`/`os`) but are stripped too for least-surface (see §4 removal list).
2. **Safe mode**: mlua does not load unsafe C functions / FFI.
3. **Memory cap**: `Lua::set_memory_limit` (probe-verified to *enforce* on Luau — allocating past the
   cap raises a memory error, not just accept the call → §8 exit 137).
4. **Timeout / interrupt** (see *Timeout model* below): a **two-layer** kill — a deadline-checking
   periodic VM callback (`set_interrupt` on Luau; instruction-count `set_hook` on PUC) for CPU-bound
   code, plus a `tokio` wall-clock timeout wrapping the whole task for code parked on async I/O.
5. **Capability gate**: every host-function entry point checks `Policy` per the *Allowlist
   matching* rules below (host/path/IP not allowed → raise a Lua error with a clear message).
6. **Layer-B reserved**: `Policy` has an `os_hardening: Option<OsHardening>` field for a
   future landlock/seccomp/sandbox-exec layer; not implemented in v1.

### Policy structure (conceptual)

```
Policy {
  profile: Loose | Strict,
  fs_read_allow:  Vec<PathRule>,    // readable path roots (read & write are separate, like Deno)
  fs_write_allow: Vec<PathRule>,    // writable path roots
  net_allow: Vec<HostRule>,         // allowed network hosts (host or host:port)
  allow_private_net: bool,          // default false — block loopback/private/link-local IPs (SSRF)
  env_allow: Vec<String>,           // allowed environment-variable names
  limits: { timeout, memory, max_concurrency, per_event_timeout, max_body, max_http_body },
  os_hardening: Option<OsHardening> // reserved, None in v1
}
```

The profile is selected and overridden by flags or a config file. `loose` defaults to
permissive; `strict` defaults to tight (allowlists empty by default). The matching rules below
are **security-critical** under §1's untrusted-script threat model.

### Allowlist matching

**Network (`net_allow`).** An entry is `host` (any port) or `host:port` (that port only); matching
is **scheme-agnostic** (`lur.http` is the only egress, so http/https both count). Host matching is
**exact** in v1 — a lone `*` means **any host** (still subject to the private-network deny below),
but *subdomain* wildcards (`*.example.com`) and CIDR ranges are **reserved**; IP literals are accepted. Two non-negotiable hardenings against SSRF / DNS-rebinding on untrusted scripts:

- **Private-network default-deny**: even when the hostname is allowlisted, a connection whose
  **resolved IP** falls in loopback / private / link-local / metadata ranges (`127/8`, `::1`,
  `10/8`, `172.16/12`, `192.168/16`, `169.254/16`, `fc00::/7`, …) is **refused** unless
  `allow_private_net` is set (`--allow-private`) or that IP/host is explicitly allowlisted.
- **Per-hop re-check**: every redirect target is re-validated against `net_allow` (a 3xx to a
  non-allowlisted or private host is blocked), so an allowed host can't bounce egress elsewhere.
- (Pinning the checked IP through to connect — full rebinding defence — is **reserved**; v1 ships
  the range default-deny.)

Both hooks are **probe-verified implementable on `reqwest`**: a custom `redirect::Policy` sees and can
stop/allow each hop's URL (per-hop re-check), and a custom `dns::Resolve` that filters private IPs
fails the connection before it is made (private-network deny).

**Filesystem (`fs_read_allow` / `fs_write_allow`).** Read and write are **separate** allowlists
(`--allow-fs-read` / `--allow-fs-write`; `--allow-fs` is sugar for both). An entry that is a
**directory grants its whole subtree**; a **file grants only that file**; globs are reserved. The
escape-proofing rule: a requested path is **canonicalized** (resolve `.`/`..`/symlinks to the real
absolute path) **before** the prefix check, so `--allow-fs-read ./out` cannot be escaped via
`./out/../../etc/passwd` or a symlink pointing outside the root. Relative paths resolve against the
**launch cwd**; allowlist roots are canonicalized to absolute at startup; a write to a not-yet-existing
file canonicalizes its **parent**. Known limitation: canonicalize-then-open has a **TOCTOU** window
(a symlink swapped after the check) — full mitigation (`openat2`/`O_NOFOLLOW`) belongs to the §5
Layer-B OS-hardening, not v1.

**Environment (`env_allow`).** Exact name match (prefix wildcards reserved). `lur.env("X")` for a
non-allowlisted name returns **`nil`** (indistinguishable from unset) rather than erroring, so it
can't be used as an oracle for which variables exist.

**Layering is additive.** Allowlists from the user config and the CLI flags are **unioned**, not
overridden — config holds standing grants, flags add per-run ones. To ignore config entirely for an
untrusted run, `--no-config` falls back to pure shipped `strict` (zero grants). Scalar settings
(`default_profile`, `timeout`, `memory`) still follow last-wins precedence (§12).

**The shipped default profile is `strict` — secure by default, like Deno.** A bare
`lur script.lua` has zero ambient access; capabilities are granted explicitly via
`--allow-net`/`--allow-fs`/`--allow-env`, or all at once via `-A`/`--allow-all`. Two
low-friction escape hatches keep personal use ergonomic without weakening the shipped
default: (1) `-A` for a one-off full-access run of a trusted script; (2) a user config
(`~/.config/lur/config`) may set `default_profile = loose` so an individual's own machine
defaults permissive, while the binary remains safe-by-default for everyone else. See §12
for the concrete CLI surface.

### Timeout model

The interrupt callback (item 4) only fires **while Lua bytecode is running** — probe-verified (~2M
fires in a CPU loop vs ~0 during a 150 ms async park). When a script is **parked on async I/O**
(`lur.http`, `lur.db`, sleep) the VM has yielded to
tokio and no Lua executes, so the interrupt alone **cannot** kill a script stuck on a slow/hung
request. Enforcement therefore uses **two complementary layers** sharing one deadline:

1. **Deadline-checking interrupt (CPU guard, in-VM).** The periodic callback checks `now >= deadline`
   (or a cancel flag) and raises to abort — kills CPU-bound code like `while true do end`.
2. **`tokio::time::timeout` around the whole task (I/O guard, out-of-VM).** When the deadline elapses
   the future is dropped, cancelling the awaited async op and tearing the task down — kills code parked
   on I/O, where the interrupt can't fire.

The two layers cover **different** stuck-states, not the same one redundantly: the interrupt is the
*only* thing that can stop a CPU-bound loop (dropping a future can't preempt a thread synchronously
running Lua), while the `tokio` timeout is the *only* thing that can stop a script parked on I/O (no
Lua runs, so the interrupt never fires). So whether a script burns CPU *or* waits on a slow request,
it dies at the deadline.

- **Metric is wall-clock, not CPU.** A hard total-time ceiling for untrusted scripts; time spent
  awaiting an allowed-but-slow API **counts** against the timeout (intended).
- **Three nested layers, outer wins.** `limits.timeout` (one-shot) / `per_event_timeout` (server, per
  event) is the **outer** deadline over a whole script/handler; `lur.http` `opts.timeout` /
  `--http-timeout` is the **inner** per-request bound. If the outer deadline passes mid-request the
  whole run is cancelled regardless of the inner one.
- **A `pcall`-loop cannot outlive the deadline** — essential against untrusted scripts, which could
  otherwise neutralize a timeout with `while true do pcall(function() while true do end end) end`.
  Probe finding (in-repo, mlua/Luau): the interrupt-raised error is an **ordinary catchable** Lua error
  — a `pcall` *does* catch a single abort — so the defense is **not** uncatchability. It is that the
  deadline-interrupt **keeps raising on every callback** once `now >= deadline`, and an adversarial
  script must have some **outermost driving loop/recursion that cannot wrap itself in `pcall`** (the
  main chunk is unprotected from Lua's side); the moment an interrupt lands in that unprotected
  bytecode it propagates straight to the host. Interrupts fire densely enough that this happens within
  milliseconds — verified: `while true do pcall(hot loop) end` terminates with the deadline error. A
  *fire-once* abort would instead be swallowed, so the **keep-raising** behavior is load-bearing. (The
  `tokio` timeout is **not** a CPU-loop backstop — it can't preempt a non-yielding thread — it covers
  only the I/O-parked case above.)
- **One abort path, several sources.** Timeout, graceful shutdown (SIGTERM cancels in-flight), and
  (server) client disconnect (reserved) all funnel through the same deadline/cancel + future-drop
  mechanism. Exit/error mapping is §8 (one-shot timeout → 124; server per-event timeout → 5xx + log,
  no crash).
- **Defaults are profile-tunable** (strict should set sane deadlines; loose may leave them open);
  concrete numbers are TBD with the benchmark budgets (§13). The interrupt frequency is an overhead
  knob measured by the §13 sandbox-overhead benchmark.

## 6. State Layer

Under a VM pool, which VM handles which event is non-deterministic, so **all
cross-request state must live host-side** and be exposed via `lur.*` so every VM in the
pool shares it. The Lua state inside each VM is treated as stateless.

### Short-term state `lur.state`

A host-side concurrent KV (`DashMap` or `Mutex<HashMap>`), **process-scoped**, shared across requests,
gone when the process exits. Because many pooled VMs touch it concurrently it must offer **atomic**
operations, not just get/set.

**Value types — primitives only.** mlua values are bound to their own VM, so a live Lua `table` can't
be shared host-side across VMs. `lur.state` therefore stores only **`nil` / `boolean` / `number` (f64)
/ `string` (bytes)** — held VM-independently and copied in/out per VM; `table`/`function`/`userdata` →
**error**. For structured data, `lur.json.encode` it to a string yourself (consistent with `lur.kv`).
`set(key, nil)` deletes; `get` returns `nil` if absent; numeric counters share the f64 **2⁵³**
precision ceiling (§6 type mapping). TTL/expiry is reserved.

```lua
lur.state.get(key) / lur.state.set(key, val)   -- val is a primitive (or nil to delete)
lur.state.incr(key[, n])                        -- atomic +n (default 1); absent → starts at 0; returns the new value
lur.state.update(key, fn)                       -- atomic read-modify-write; see below
```

**`update(key, fn)` — optimistic, version-stamped (no lock held during `fn`).** The naive "lock the
key, run `fn` under the lock" design is rejected: an untrusted/slow `fn` would hold the lock (DoS),
`fn` re-entering `lur.state` would deadlock, and awaiting I/O under a lock would stall everyone.
Instead (the Clojure-`atom`/`swap!` model):

1. **read** snapshots the current `(value, version)` under a brief host-only lock, then unlocks;
2. **`fn(old)` runs with no host lock held**, computing the new value;
3. **write-back** takes the brief lock again and stores the new value **iff the key's version is still
   the snapshotted one** (then bumps the version); otherwise someone else wrote in between, so it
   **retries** from step 1.

Conflict detection compares a **monotonic per-key version counter, never the values** — this sidesteps
f64 equality traps (`NaN ≠ NaN` would livelock, `-0.0 == 0.0`, precision) and the ABA problem
(`5→6→5`), and the only locks are momentary pure-Rust sections, never spanning user code. Consequences:

- **`fn` must be a pure transform** `value → value` and **safe to re-run** (it may run more than once
  under contention).
- **`fn` must not perform I/O or re-enter `lur.state`** → doing so raises a clear error (not a
  deadlock). Need I/O? Do it **outside** `update` and pass the result in — `local d = lur.http.get(u).body;
  lur.state.update(k, function() return d end)` — so a retry never re-fires the request. (True
  serialization across an I/O call belongs to `lur.db.tx`, not `lur.state`.)
- **`fn` error** leaves the value unchanged and propagates. A runaway `fn` is killed by the §5 timeout,
  and since no lock is held there is none to leak.
- `incr` is the fn-free atomic fast path (host increments directly; no retry).

### Long-term state `lur.db` + `lur.kv`

A `StorageBackend` trait; v1 implements **SQLite** over **`sqlx`** (async, pooled). `sqlx` is chosen
over `rusqlite` because lur is already async, server mode needs a non-blocking pool, and `sqlx`'s
same-API multi-backend path is exactly the reserved **Postgres** route (§10). Queries use the
runtime `query()` API (not the compile-time `query!` macro), so no database is needed at build time.

**Core API.** Positional `?` placeholders bound from varargs (named params reserved):

```lua
local rows = lur.db.query("SELECT id, name FROM u WHERE age > ?", 18)
--  → { { id=1, name="a" }, … }   -- array of row tables keyed by column name (alias duplicate columns)
local r = lur.db.exec("INSERT INTO u(name) VALUES (?)", "c")
--  → { rows_affected = 1, last_insert_id = 3 }

lur.db.tx(function(tx)             -- one pinned connection; commit on return, rollback on error
  tx.exec("UPDATE acct SET bal = bal - ? WHERE id = ?", 100, 1)
  tx.exec("UPDATE acct SET bal = bal + ? WHERE id = ?", 100, 2)
end)
```

`lur.db.tx` is required because a pool would otherwise scatter a hand-written `BEGIN`/`COMMIT` across
different checked-out connections; the explicit `tx.` handle pins one connection and marks the
transaction boundary (an implicit context would be unsafe if the body spawned `lur.async` tasks).

**Type mapping.** Lua (Luau) values ↔ SQLite storage classes:

| Read (SQLite → Lua) | | Write (Lua → SQLite) | |
|---|---|---|---|
| NULL | **`lur.null`** sentinel | `nil` / `lur.null` | NULL |
| INTEGER | number (f64) | boolean | INTEGER 0/1 (reads back as number) |
| REAL | number | number | INTEGER if integral, else REAL |
| TEXT | string (bytes, not validated) | string | TEXT (`lur.blob()` for explicit BLOB is reserved) |
| BLOB | string (raw bytes) | table | **error** — encode it (`lur.json.encode`) yourself |

Two sharp edges to document: **(1)** SQL NULL maps to a unique singleton **`lur.null`**, not Lua
`nil` — assigning `nil` to a table key *deletes* it, so a NULL column would silently vanish and
couldn't round-trip. `lur.null` is also the runtime's **shared JSON null** (`lur.json` decodes JSON
`null` to it and encodes it back), so the DB-NULL and JSON-null boundaries use one sentinel. **(2)**
Luau has no integer subtype (§14), so INTEGER → f64 and values **> 2⁵³ lose precision** (as in JS) —
store exact large integers (snowflake IDs) as TEXT. (Probe-verified: `math.type` is absent and
`2⁵³ == 2⁵³+1`; `utf8` *is* present, so §3 holds.)

**`lur.kv`** — `get`/`set`/`delete` over an auto-created internal table `lur_kv(key TEXT PRIMARY KEY,
value BLOB)`; **keys are strings, values are raw bytes** (encode structured values yourself). TTL is
reserved.

**Pool, WAL & path.** SQLite is opened in **WAL** mode (concurrent readers + a single writer — writes
serialize); pool size defaults to `max_concurrency`. `--db <path>` selects the file; calling
`lur.db`/`lur.kv` with no `--db` is a clear error (in-memory DB reserved).

**Schema management — user-owned, idempotent DDL (no framework in v1).** Like peer script runtimes
(Val Town, redbean, Deno+SQLite), the script owns its application schema and lur stays out of the
migration business. lur only auto-creates/owns its internal `lur_kv` table; everything else is the
script's. Run DDL where the model already has an init point: at the top of a one-shot script, or in
`app.lua` at load time (§3, before handlers register):

```lua
lur.db.exec[[ CREATE TABLE IF NOT EXISTS u (id INTEGER PRIMARY KEY, name TEXT) ]]
```

`CREATE TABLE IF NOT EXISTS` handles first-time creation but does not *evolve* a schema; the
framework-free middle path for versioning is SQLite's built-in **`PRAGMA user_version`** (read it,
apply the matching steps, bump it — pure SQL, no tooling). A full managed migration runner is a
**reserved** extension that would lean on **`sqlx`'s** own `migrations/*.sql` + `_sqlx_migrations`
mechanism (e.g. `lur serve --migrations ./migrations`), not a hand-rolled one — most relevant to
long-running server apps.

- Both coexist: use `lur.db` when you want SQL, `lur.kv` when you just need simple persistence.

## 7. Concurrency Model

- `tokio` underneath; `lur.http.*` is async (mlua async + tokio) — probe-verified that mlua async
  functions run under the Luau backend on a tokio runtime.
- The `lur.async.*` family wraps multiple Lua functions into tokio tasks and runs them
  concurrently — fire many requests, then await them together. The four combinators mirror
  JS `Promise.all` / `Promise.race` / `Promise.allSettled` / `Promise.any`:
  - `lur.async.all{...}` — await every task; return results in argument order. If any task
    errors, the whole call **re-raises that error** (fail-fast); the rest are cancelled.
  - `lur.async.race{...}` — return as soon as the **first** task settles (success *or* failure):
    its value on success, or re-raise its error if that first-settling task failed. Rest cancelled.
  - `lur.async.settled{...}` — await every task but **never raise**; return a per-task array of
    `{ ok = true, value = … }` / `{ ok = false, err = … }`, so one failure doesn't discard the
    others' results.
  - `lur.async.any{...}` — return the **first task to *succeed***; only if **every** task fails
    does it raise an aggregate error. The remaining tasks are cancelled once one succeeds.

### Coroutines vs. the async layer

Luau's built-in `coroutine` library is kept under strict (it is pure control flow — no ambient
I/O, §4), but the two live at different layers and must not be conflated:

- **`coroutine.*` is the mechanism, not the concurrency API.** Use it for cooperative control
  flow — generators, lazy iterators, state machines. On its own it gives **no parallel I/O**:
  `resume`/`yield` only hand control between your own code; there is no scheduler behind it.
- **`lur.async.*` is the concurrency API**, built on top of `coroutine` + mlua-async + tokio:
  awaiting `lur.http.*` suspends the Lua coroutine and tokio drives it. This yields **concurrency,
  not parallelism** — on one VM Lua code still runs one piece at a time, interleaving only at I/O
  await points (exactly like single-threaded JS `Promise.all`). True parallelism comes from the
  VM pool spreading *events* across VMs (§3).
- **Footgun**: do **not** manually `coroutine.resume` a coroutine that internally awaits
  `lur.http` (or any async host call) — that yield must be driven by the runtime, and resuming it
  by hand hits a "yield across an async boundary" error. Drive concurrency through `lur.async.*`,
  not hand-rolled coroutine scheduling. (Verify the exact mlua-Luau behavior with a probe before
  implementing, per the workspace's pre-implementation rule.)

- Concurrency limit controlled by `policy.limits.max_concurrency`.
- Server mode: VM-pool size aligns with `max_concurrency`; event parallelism is driven by tokio.

## 8. Error Handling & Exit Codes

| Situation | Behavior |
|---|---|
| Script error | `pcall` catches it → print traceback to stderr → exit 1 |
| Policy violation | Raise a Lua error with a clear message (host/path/env blocked) |
| Timeout | one-shot → exit 124 (aligned with GNU `timeout`); server per-event → 5xx + log. A `pcall`-loop can't outlive the deadline (§5 *Timeout model*) |
| Out of memory | exit 137 |
| Normal completion | stdout is whatever the script wrote; exit code is mapped from the `return` value: a number → that exit code, `nil`/`false` → 1, anything else (including no `return`) → 0 |

Server mode: a single failing handler **must not bring down the whole server** — return
5xx / log it and keep serving.

## 9. Testing Strategy

- **Unit**: each host module with a mock policy (allowlist hit/miss, fs read/write, json round-trip).
- **Sandbox blocking**: under strict, `os.execute`/`io.open`/path escape/non-allowlisted
  host are all blocked; timeout and memory cap actually fire.
- **one-shot integration**: `assert_cmd` + sample `.lua` running the whole binary,
  asserting stdout/exit code (golden tests), including the return-value → exit-code mapping.
- **Byte/Unicode**: binary-safe round-trip (stdin bytes → `lur.stdout.write` → identical
  bytes); chunked `lur.stdin.read(n)`; `lur.json.encode` errors on non-UTF-8 and the
  base64 path works; `utf8.*` codepoint handling.
- **server integration**: start the server → hit HTTP → verify response and state change;
  verify cron triggering.
- **State concurrency**: hit the counter API concurrently from many requests, verify
  `incr`/`update` atomicity (no lost updates); `update` retries under contention and rejects a
  non-primitive value or an `fn` that performs I/O / re-enters `lur.state`.
- **storage**: SQLite round-trip (`lur.db` and `lur.kv`); type mapping (NULL ↔ `lur.null`, INTEGER
  precision, BLOB/TEXT bytes); `lur.db.tx` commit-on-return / rollback-on-error.
- **performance**: a `criterion` benchmark suite gates regressions on the perf-sensitive paths
  (VM cold start, host-call boundary, sandbox-hook overhead, state contention, storage, server
  throughput). See §13 for the full plan, budgets, and CI gate.

## 10. Key Dependencies (candidates; verify versions per the 7-day cooldown rule in CLAUDE.md before release)

- `mlua` (Luau backend, features: `luau`, async, send; see §14 for the backend decision)
- `tokio` (async runtime)
- `clap` (CLI parsing)
- `reqwest` (HTTP client for `lur.http`)
- `serde_json` (JSON encode/decode, output serialization)
- `dashmap` (short-term state concurrent KV)
- `sqlx` (async SQLite backend + connection pool; runtime `query()` API, no build-time DB; its
  multi-backend API is the reserved Postgres path, and its migration runner the reserved managed-
  migrations path — see §6). Chosen over `rusqlite` (sync) since lur is async and server mode needs a
  non-blocking pool.
- Server-mode HTTP: `axum` / `hyper`
- Cron scheduling: **`cron`** (the parser/next-fire calculator only; ≥ 0.17). lur drives the timer
  loop itself as a `Source`, rather than `tokio-cron-scheduler`, whose own scheduler loop, task
  spawning, and job registry would duplicate lur's event loop + VM pool + `max_concurrency`; its
  value-adds (interval jobs, persistent stores) are exactly the features lur defers, and since it
  depends on `cron` anyway, building on `cron` keeps that door open. See §3 *Cron scheduling*.
- `criterion` (dev-dependency: micro-benchmarks; see §13)

## 11. Suggested Implementation Order

1. Shared core: `policy` + `runtime` (single VM, injection mechanism, sandbox enforcement).
2. one-shot mode + basic host modules (`args`/`stdin`/`json`/`log`/`fs`/`http`).
3. State layer: `lur.state` (short-term) → `lur.db`/`lur.kv` (SQLite).
4. Server mode: VM pool + `lur.serve.http` + event loop.
5. Server extension: `lur.serve.cron` — `cron` Source (6-field, seconds-precision), single-flight
   (`overlap=false`) + fire-forward (no backfill), UTC default, `opts{ name, overlap, timeout }` (§3).
6. Concurrency helpers `lur.async.*` (`all`/`race`/`settled`/`any`), profile config file, docs and examples.

Throughout: stand up the `criterion` benchmark harness + CI perf gate alongside step 1 (start
with VM cold start, host-call boundary, and sandbox-hook overhead), and add a benchmark with each
new perf-sensitive feature as it lands — so performance is guarded continuously, not retrofitted (§13).

## 12. CLI Surface

The binary is `lur`. Following the convention of script interpreters (`lua`, `node`,
`python`, `ruby`, `php`, `bash`), running a script is **implicit** — no `run` subcommand;
you pass the file directly. Server mode uses the `serve` subcommand.

### one-shot mode

```console
# Simplest: run a script
$ lur fetch.lua

# Positional args (no leading dash) go right after the script
$ lur backup.lua /data /backup

# Flag-style args go after `--`, so they don't collide with lur's own flags
$ lur deploy.lua -- --env prod --force release.tar
#   in script: lur.args.flags.env=="prod", flags.force==true, positional[1]=="release.tar"

# Unix pipeline: stdin in, stdout out (binary-safe)
$ cat access.log | lur top_ips.lua | sort | head
$ lur transform.lua < input.json > output.json

# Structured result is the script's job (Unix convention), pipes into jq
$ lur stats.lua data.csv | jq .total
#   script ends with: print(lur.json.encode(result))

# Sandbox: default is strict (zero access); grant capabilities explicitly
$ lur --allow-net api.github.com --allow-fs ./out fetch.lua

# Trusted script, one-off full access
$ lur -A myscript.lua
```

### server mode

```console
$ lur serve app.lua
$ lur serve --bind 127.0.0.1:8080 --allow-net "*" --db ./app.db app.lua
```

### Flags (tentative)

| Flag | Effect |
|---|---|
| `-A`, `--allow-all` | Grant full access for this run (overrides strict default) |
| `--strict` / `--loose` | Select profile, overriding the default |
| `--config <file>` | Load a policy config file (default: `~/.config/lur/config`) |
| `--no-config` | Ignore the user config entirely → pure shipped `strict`, zero grants |
| `--allow-net <host>` | Add a network host (`host` or `host:port`) to the allowlist (repeatable) |
| `--allow-private` | Permit connections to loopback/private/link-local IPs (off by default; see §5 SSRF) |
| `--allow-fs-read <path>` | Add a readable path root (repeatable) |
| `--allow-fs-write <path>` | Add a writable path root (repeatable) |
| `--allow-fs <path>` | Sugar: add the path to **both** read and write allowlists (repeatable) |
| `--allow-env <name>` | Add an environment variable to the allowlist (repeatable) |
| `--timeout <dur>` | Execution timeout (e.g. `5s`, `2m`) |
| `--memory <size>` | Memory cap (e.g. `128m`) |
| `--max-concurrency <n>` | Concurrency limit |
| `--bind <addr>` | (server) listen address |
| `--max-body <size>` | (server) max request-body size; larger requests get 413 (e.g. `2m`) |
| `--max-http-body <size>` | max buffered `lur.http` response-body size; larger responses error (e.g. `16m`) |
| `--db <path>` | SQLite database path |

### Defaults & precedence

- **Default profile is `strict`** (secure by default; see §5). A bare run has zero access.
- Resolution layers: shipped default (`strict`) → user config (`~/.config/lur/config`) →
  command-line flags. **Allowlists combine additively** (config standing grants ∪ flag per-run
  grants; see §5); **scalar settings** (`default_profile`, `timeout`, `memory`, …) follow
  **last-wins**. `--no-config` drops the config layer for an untrusted run.
- **Config format is TOML** (`~/.config/lur/config`):

  ```toml
  default_profile = "loose"     # individual machine may opt permissive; binary stays strict-by-default
  [allow]
  net      = ["api.github.com", "10.0.0.5:6379"]
  fs_read  = ["~/data"]
  fs_write = ["./out"]
  env      = ["API_KEY"]
  ```

- Argument passing: bare tokens after the script are positional (`lur.args.positional`);
  everything after `--` is parsed into `lur.args.flags` (`--key value` / bare `--flag` → true).

## 13. Benchmarking & Performance Guarding

Goal: keep performance at a known-good level **throughout** development — no silent regressions.
This operationalizes the workspace rule "baseline before a perf change, compare after; regressions
must not be committed." Stand the harness up **early** (with the first runtime work) and add a
benchmark with each perf-sensitive feature, so nothing is retrofitted.

**Approach: `criterion` + CI gate.** One `criterion` suite in `benches/`; CI runs it on each PR and
compares to the `main` baseline with `critcmp`; a regression beyond the threshold fails the build.
Wall-clock is the metric — tame CI noise with a consistent runner, warm-up, and adequate samples;
don't chase zero variance. Inputs are fixtures checked into the repo (no network in benches).

### What to benchmark

| Area | Benchmark |
|---|---|
| VM lifecycle | cold start: build VM + inject `lur.*` + teardown (dominates one-shot latency) |
| Lua execution | CPU-bound loop — canary for `mlua`/Lua upgrades |
| Host-call boundary | `lur.json.encode` / `lur.state.get` round-trips — the cost *we* add |
| Sandbox overhead | a loop with vs. without the interrupt callback + memory limit |
| State | `incr`/`update` under concurrency — atomic-op + contention cost |
| Storage | `lur.kv` / `lur.db` round-trip (SQLite) |
| Server mode | HTTP req/s + p50/p99 latency (run on demand, not in the per-PR gate) |

### How CI gets the baseline

`critcmp` needs **both** the PR's numbers and `main`'s to compare, so the gate is one self-contained
CI job that produces both on the **same runner, in the same run** — this cancels the cross-machine
noise that would otherwise poison a wall-clock metric:

1. check out `main` → `cargo bench` → save as the `main` baseline;
2. check out the PR → `cargo bench` → save as `pr`;
3. `critcmp main pr` with the regression threshold → a regression exits non-zero and fails the job.

This re-runs `main` on every PR (≈2× bench time), accepted deliberately: lur's micro-benchmarks are
short, and same-runner/same-moment measurement matters more than the saved minutes. The job is a
**required check** run on a fixed runner spec, parallel to `fmt`/`clippy`/`nextest` — it gates merge
but doesn't block the functional tests.

### Regression gate & budgets

- A **> 5%** regression on a tracked benchmark fails CI; calibrate the 5% once the first runs reveal real noise.
- A perf-affecting PR carries before/after numbers; an intentional trade-off is called out and the baseline re-blessed.
- Budgets are **TBD until the first baseline lands**; initial guardrail intents: one-shot cold start in single-digit ms,
  host-call overhead in low single-digit µs, sandbox-hook overhead < ~10%. Measured numbers replace these once the suite runs.

## 14. Decision Record — Lua VM backend: Luau (not PUC Lua 5.4)

**Status**: Decided — 2026-06-26. **Backend**: Luau via `mlua`'s `luau` feature.

**Context.** §1 explicitly admits untrusted scripts ("from others / the internet"), so the
sandbox (§5) is security-critical, not best-effort. Two backends were weighed, both reachable
through the same `mlua` binding: stock **PUC Lua 5.4** (with a fully hand-rolled sandbox) vs
**Luau**, Roblox's Lua 5.1 fork built specifically to run untrusted code.

**Decision.** Use the **Luau** backend. The deciding factor is that the threat model already
includes untrusted scripts, and Luau moves the most dangerous, hardest-to-maintain part of §5
— making globals readonly and removing dangerous stdlib — into a VM hardened against a
Roblox-scale attack surface, instead of into lur's own code (every bug in a hand-rolled sandbox
is a lur CVE). Migration cost is low: the same `mlua` crate switches backends by feature flag.

**Evidence** (in-repo probe, `mlua` 0.11.6 with `luau`; all §5 primitives confirmed working):

| §5 primitive | Luau backend |
|---|---|
| capability injection (`lur.*`) | `globals().set` — ok, callable post-sandbox |
| readonly globals | `Lua::sandbox(true)` — base libraries become readonly |
| remove dangerous stdlib | the sandbox drops them (`os.execute` → `nil`) |
| timeout / interrupt | `set_interrupt` (raise to abort) — replaces PUC `set_hook` |
| memory cap | `set_memory_limit` — shared by both backends |

**Consequences.**

- **§5 wording**: the periodic interrupt is `set_interrupt` on Luau (not the instruction-count
  `set_hook` of PUC Lua) — same semantics: a periodic callback that aborts by raising.
- **Dialect**: Luau is Lua 5.1 + its own extensions, **not 5.4**. The `utf8` library is present
  (so §3 holds), but other 5.4-specific semantics — the integer subtype, bitwise operators,
  `<close>` to-be-closed variables, `goto` — differ and must be re-checked before relying on them.
  Luau also *adds* features (optional gradual typing, `+=`, `continue`, string interpolation).
- **Ecosystem**: smaller third-party library pool than PUC Lua — largely moot because lur exposes
  capabilities through `lur.*`, not arbitrary `require` / C modules.

**Revisit if** the threat model later narrows to semi-trusted-only scripts **and** full Lua 5.4
semantics or the larger ecosystem become hard requirements — then PUC Lua 5.4 with the hand-rolled
§5 sandbox is the documented fallback.
