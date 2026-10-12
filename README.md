# lur

> A small, sandboxed Lua runtime written in Rust.

[![CI](https://github.com/henry40408/lur/actions/workflows/ci.yml/badge.svg)](https://github.com/henry40408/lur/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/henry40408/lur/graph/badge.svg)](https://codecov.io/gh/henry40408/lur)
[![Release](https://img.shields.io/github/v/release/henry40408/lur)](https://github.com/henry40408/lur/releases/latest)
[![License](https://img.shields.io/github/license/henry40408/lur)](LICENSE.txt)
[![Rust toolchain](https://img.shields.io/badge/dynamic/toml?url=https://raw.githubusercontent.com/henry40408/lur/main/rust-toolchain.toml&query=$.toolchain.channel&label=rust%20toolchain&logo=rust)](https://www.rust-lang.org/)
[![Docker](https://img.shields.io/badge/docker-ghcr.io-blue.svg)](https://ghcr.io/henry40408/lur)
[![Casual Maintenance Intended](https://casuallymaintained.tech/badge.svg)](https://casuallymaintained.tech/)
[![Vibe Coded](https://img.shields.io/badge/vibe_coded-Claude-d97757?logo=anthropic&logoColor=white)](https://claude.com/claude-code)

Write automation and services in [Luau](https://luau.org/); `lur` runs them behind a
capability sandbox. One binary, two modes:

- **one-shot** — `lur script.lua` runs a script once and exits (glue, ETL, cron jobs).
- **server** — `lur serve app.lua` runs a long-lived HTTP service with scheduled jobs.

## Design principles

- **Sandboxed always.** Scripts may be semi-trusted or untrusted, so every side effect
  goes through a capability policy.
- **Secure by default.** The default `strict` profile grants no filesystem, network,
  environment, or private-IP access.
- **One core.** Both modes share the sandboxed Luau VM, the `lur.*` host modules, the
  policy layer, and a tokio async core.

## Install

Requires a Rust toolchain (edition 2024).

```sh
git clone https://github.com/henry40408/lur
cd lur
cargo build --release   # binary at ./target/release/lur
```

### Docker

A multi-arch (`linux/amd64`, `linux/arm64`) image on GHCR: a static musl binary on
`distroless/static` (CA certificates, non-root, no shell). `:main` tracks `main`;
releases publish `:X.Y.Z`, `:X.Y`, and `:latest`.

`lur --version` reports the release tag, `<ref>-<short-sha>` (e.g. `main-1a2b3c4`) for
`:main`, `git describe` for a local build, or `dev` as a last resort.

```sh
docker run --rm ghcr.io/henry40408/lur:latest --version

# mount the script; grant capabilities with the usual flags
docker run --rm -v "$PWD/app.lua:/app.lua:ro" \
  ghcr.io/henry40408/lur:latest --allow-net example.com /app.lua
```

Build locally (cross-compiles via `cargo-zigbuild`, no qemu):

```sh
LOAD=true PLATFORMS=linux/amd64 ./scripts/docker-build.sh   # single arch into local docker
./scripts/docker-build.sh                                   # validate both arches
```

## Quick Start

### One-shot

```lua
-- hello.lua
lur.stdout.write("hello, " .. (lur.args.positional[1] or "world") .. "\n")
```

```sh
lur hello.lua there          # → hello, there
```

### Server

```lua
-- app.lua
lur.serve.http("GET", "/health", function(req)
  return { status = 200, body = "ok" }
end)

lur.serve.cron("0 */5 * * * *", function()
  lur.log.info("tick every 5 minutes")
end)
```

```sh
lur serve app.lua
```

## The sandbox

Every capability is denied until a policy grants it. Two profiles set the baseline:

| Profile | Filesystem | Network | Env | Private IPs |
| --- | --- | --- | --- | --- |
| `strict` *(default)* | none | none | none | denied |
| `loose` (`-A`) | full | any host | all | allowed |

`os.execute`, `io`, `loadfile`/`dofile`, and `package` are absent; `require`, `getfenv`,
`setfenv`, and `loadstring` are removed (they would reach the writable global
environment). `string`, `table`, `math`, `bit32`, `utf8`, and `coroutine` remain. Globals
are frozen, and in server mode each request/job runs in a fresh environment whose writes
are discarded, so no state leaks across calls.

## CLI reference

```
lur <script.lua> [SCRIPT_ARGS...]      # one-shot
lur serve <app.lua> [FLAGS]            # server
lur docs                               # print the embedded usage guide
```

`SIZE` takes a binary (×1024) suffix: bare/`b`, `k`/`kb`, `m`/`mb`, `g`/`gb` (e.g.
`256m`). `DURATION` takes `ms`, `s` (or bare), `m` (minutes), `h` (e.g. `500ms`, `2m`).

### Common flags

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--strict` | — | (default) | Deny-by-default profile. Conflicts with `--loose`. |
| `--loose`, `-A`/`--allow-all` | — | | Permissive profile (full access). |
| `--allow-fs-read` | PATH | | Add a readable root (repeatable). |
| `--allow-fs-write` | PATH | | Add a writable root (repeatable). |
| `--allow-fs` | PATH | | Add a read + write root (repeatable). |
| `--allow-env` | NAME | | Allow reading an environment variable (repeatable). |
| `--allow-net` | HOST | | Allow a host or `host:port`; IPv6 as `::1`, `[::1]`, or `[::1]:port` (repeatable). |
| `--allow-private` | — | off | Permit loopback/private/link-local/CGNAT/multicast addresses, incl. IPv6 forms embedding them (SSRF guard off). |
| `--memory` | SIZE | `256m` | Per-VM memory cap; `0` = unlimited. |
| `--max-http-body` | SIZE | `16m` | Cap on a buffered `lur.http` response body. |
| `--http-cache-max-entries` | N | `1024` | Cap on cached `lur.http` responses; least recently used are evicted; `0` = unlimited. |
| `--http-cache-max-bytes` | SIZE | `64m` | Cap on the total size of cached `lur.http` responses; `0` = unlimited. |
| `--state-max-entries` | N | `0` | Cap on `lur.state` entries; only `evictable` ones are dropped for room, otherwise the write raises; `0` = unlimited. |
| `--state-max-bytes` | SIZE | `0` | Cap on the total size of `lur.state`; same rule; `0` = unlimited. |
| `--max-concurrency` | N (>= 1) | unbounded | Cap on in-flight `lur.async.*` tasks per VM. |
| `--db` | PATH or URL | | SQLite file or `postgres://`/`postgresql://` URL backing `lur.db` / `lur.kv`. |
| `--config` | FILE | | Load a specific config file. Conflicts with `--no-config`. |
| `--no-config` | — | | Ignore all config — pure strict, zero grants. |

### One-shot only

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--timeout` | DURATION | none | Wall-clock limit. |

Exit status: the script's top-level `return` sets it (a number as-is, `nil`/`false` →
`1`, otherwise or no return → `0`); an uncaught error → `1`; timeout → `124`; exceeding
`--memory` → `137`; unreadable script or bad flags/config → `2`.

### Server only

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--bind` | ADDR | `127.0.0.1:8080` | Listener address (env `BIND`; the container image sets `0.0.0.0:8080`). |
| `--pool-size` | N | CPU count | Pre-warmed VMs; caps concurrent requests. |
| `--timeout` | DURATION | none | Per-request limit; timeout → `503`. |
| `--max-body` | SIZE | none | Max request body; larger → `413`. |
| `--shutdown-grace` | DURATION | `10s` | Drain window on `SIGTERM`/`SIGINT`. |
| `--log-format` | `full`/`compact`/`pretty`/`json` | `full` | Log format (env `LOG_FORMAT`). Filter with `RUST_LOG` (default `error,lur=info`). |

### Config file

Without `--config`/`--no-config`, `lur` reads `$XDG_CONFIG_HOME/lur/config` (else
`~/.config/lur/config`) if it exists. TOML:

```toml
default_profile = "strict"   # or "loose"

[allow]
net      = ["api.github.com", "10.0.0.5:6379"]
fs_read  = ["~/data", "/var/log"]
fs_write = ["./out"]
env      = ["API_KEY", "DEBUG"]
```

The profile is last-wins (`--strict`/`--loose`/`-A` override `default_profile`);
allowlists are additive (config ∪ flags). `~` in config paths expands to `$HOME`.

## Lua API

Everything lives under the `lur` global. Failures, including policy denials, raise a
Lua error (catch with `pcall`).

### Data & I/O

- **`lur.null`** — sentinel distinct from `nil` (which means *absent*); round-trips JSON
  `null` and SQL `NULL`.
- **`lur.json`** — `encode(value) → string`, `decode(text) → value`. JSON `null` decodes
  to `lur.null`; strings must be UTF-8 (base64 binary first). `decode(text, { null = "drop" })`
  omits object keys whose value is `null` and removes `null` array elements (compacting the
  array); `"keep"` is the default, and a bare top-level `null` stays `lur.null`.
- **`lur.base64`** — `encode(bytes) → string`, `decode(text) → bytes`.
- **`lur.crypto`** — `sha256`/`sha512`/`sha1`/`md5(data) → bytes`;
  `hmac_sha256`/`hmac_sha512`/`hmac_sha1(key, msg) → bytes`; `hex.encode(bytes)` /
  `hex.decode(text)`; `random_bytes(n)` (OS CSPRNG); `constant_eq(a, b) → bool`
  (timing-safe). Digests are raw bytes. `sha1`/`md5` are for legacy interop only.
- **`lur.cookie`** — `parse(header) → { name = value, … }` reads a `Cookie` header
  (lenient: malformed segments skipped, later duplicate wins, no decoding).
  `serialize(name, value, opts?) → string` builds one `Set-Cookie` value (no prefix);
  `opts`: `domain`/`path`/`expires` (string), `max_age` (integer seconds),
  `secure`/`http_only` (boolean), `same_site` (`"Strict"`/`"Lax"`/`"None"`). An invalid
  name, a value with `;`/CR/LF, or `same_site="None"` without `secure=true` raises.
  Format `expires` with `os.date("!%a, %d %b %Y %H:%M:%S GMT", t)`.
- **`lur.time`** — integer milliseconds throughout. `now_ms()` (Unix time),
  `monotonic_ms()` (for elapsed-time differences), `parse_rfc3339(text)` (UTC offset
  such as `Z` required) and `parse_http_date(text)` → epoch ms; malformed input raises.
  Divide by `1000` for `os.date`. Timezone-aware: `format_rfc3339(ms, tz?)`,
  `format_rfc2822(ms, tz?)`, `format(ms, strftime, tz?)`, `parse_rfc2822(text)`, and
  `parse(text, fmt?, tz?)`. `tz` is an IANA name (`"Asia/Taipei"`), a fixed offset
  (`"+08:00"`), or `nil`/`"UTC"`. `parse` without `fmt` accepts RFC 3339, RFC 2822,
  HTTP-date, then `YYYY-MM-DD[ T]HH:MM[:SS[.f]]` and `YYYY/MM/DD[ HH:MM[:SS]]`; wall-clock
  text with no offset is read in `tz`. A local time in a DST gap raises; in an overlap the
  earlier instant wins. `try_parse(text, fmt?, tz?)` is `parse` that returns `nil` for
  unparseable (or `nil`) text; a bad `fmt` or `tz` still raises.
- **`lur.url`** — `parse(url) → { href, scheme, host?, port?, username?, password?, path,
  query?, fragment? }` (`port` only when explicit and non-default; invalid URL raises),
  `join(base, relative) → string` (resolves relative links, `base` must be absolute),
  `encode_query(table) → string` (keys sorted; an array value repeats the key; string,
  number and boolean values) and `decode_query(text) → table` (leading `?` optional, last
  duplicate wins, `+` is a space).
- **`lur.charset`** — `decode(bytes, hint?) → string` and `encode(text, label) → bytes` for
  non-UTF-8 pages (Big5, GBK, Shift_JIS, … via `encoding_rs`, WHATWG labels). `hint` is a
  charset label or a whole `Content-Type` value. Precedence: BOM, then `hint`, then a
  `<meta charset>` in the first 1 KiB, then UTF-8; invalid bytes become U+FFFD; an unknown
  label raises. `encode` writes unmappable characters as `&#NNN;` and refuses UTF-16.
- **`lur.html`** — `parse(html) → doc` (invalid UTF-8 is replaced; decode other encodings with
  `lur.charset` first). Nodes (the doc and its
  elements) share methods: `select(css) → array`, `select_one(css) → node | nil`
  (descendants only; invalid selector raises), `text(opts?)` (`{ trim = true }` strips
  surrounding whitespace; `lur.html` only), `text(css) → string | nil` (shorthand for
  `select_one(css):text()`; `nil` when nothing matches), `html()`, `inner_html()`, `tag()`,
  `attr(name) → string | nil`, `attrs()`, `parent()`, `children()` (elements only).
  Call with `:`. Parsing follows the HTML5 algorithm, so `<td>` outside a `<table>` is dropped.
- **`lur.html.sanitize(html, opts?) → string`** — strips scripts, styles, event handlers and
  unsafe URLs with `ammonia`'s allowlist; only `http`, `https` and `mailto` links survive and
  links get `rel="noopener noreferrer"`. `opts.base` (absolute URL) resolves relative
  `href`/`src`; without it they are kept as written. Broken markup is repaired.
- **`lur.xml`** — `parse(xml) → doc`. Same node methods as `lur.html` (`select`, `select_one`,
  `text`, `tag`, `attr`, `attrs`, `parent`, `children`) but selection takes a slash path:
  `a/b` (children), `//a` (descendants, also mid-path), `*` (any name); results are in document
  order and deduplicated. Names match exactly as written (`dc:creator`); namespaces are not
  resolved. Malformed XML raises with line/column; unknown entities such as `&nbsp;` are kept
  as text; `DOCTYPE` is ignored, so custom entities are not expanded. A BOM or the
  declaration's `encoding` selects the charset; otherwise invalid UTF-8 is replaced.
- **`lur.feed`** — `rss(meta, items)`, `atom(meta, items)`, `json(meta, items)` → string
  (RSS 2.0 / Atom 1.0 / JSON Feed 1.1). `meta`: `title` (required), `link`, `feed_url`,
  `description`, `language`, `id`, `updated`. Each item: `title` (required), `link`, `guid`,
  `date`, `updated`, `summary`, `content` (HTML), `author`, `categories` (array of strings),
  `enclosure` (`{ url, type?, length? }`). Dates are epoch ms. RSS needs `meta.link`; Atom
  needs `meta.id` or `meta.link`; Atom/JSON items need `guid` or `link`. XML is escaped and
  characters illegal in XML 1.0 are dropped. An optional third argument `{ skip_invalid = true }`
  skips items whose `title` is missing (these raise by default) and also drops empty or blank
  titles, which are accepted by default; other item errors still raise.
- **`lur.validate`** — validate and convert untrusted input (query strings, JSON bodies).
  Small composable schemas plus two entry points; pure compute, no policy, same in one-shot
  and serve mode. `parse(schema, value) → converted` raises on failure;
  `try(schema, value) → true, converted | false, err` returns the same `err`. The result is
  always a new table; the input is never modified.
  Schemas (`opts` optional): `str{ pattern?, min_len?, max_len?, default? }` (`pattern` is a Lua
  pattern, as in `string.find`, unanchored; lengths count characters; numbers are not
  coerced), `int{ min?, max?, default?, clamp? }` (a number with an integral value, or a string
  that is strictly `-?%d+`; |n| must be <= 2^53), `number{ min?, max?, default?, clamp? }`
  (finite only; strings must be decimal floats), `bool{ default? }` (a boolean, or the strings
  `"true"`/`"false"`), `one_of{ ... }` (strict equality over strings/numbers/booleans),
  `list(item, { min_items?, max_items? }?)`, `object(fields, { strict? }?)` and
  `optional(schema)`. A plain function `function(value) → value` is a schema anywhere one is
  accepted; raising rejects (the message, minus its position, becomes the issue message).
  Values are required unless wrapped in `optional(...)` or given a `default` (which fills a
  missing value as-is). For `int`/`number`/`bool`, `""` counts as missing, so `?limit=` falls
  back to the default. Unknown `object` keys are dropped; `strict = true` makes them issues.
  `clamp = true` clamps into `[min, max]` instead of failing.
  On failure `err` is a table `{ kind = "validation", message, issues = { { path, message }, … } }`
  (all issues are collected; paths read `a.b[2].c`, the root is `""`; at most 100 issues, then
  `truncated = true`). `tostring(err)` is `message`, an uncaught one prints that message, and it
  is catchable with `pcall`. Raised inside a `lur.async` task it reaches the combinator's caller
  as a plain string (its message), not a table; use `try` there. Bad arguments (unknown
  option, `min > max`) raise `lur.validate.<fn>: …`. Schemas nest at most 64 levels.
  `optional(x)` only waives "required": an inner `default` still fills. `default` values are
  returned as given, not checked against `min`/`max`/`pattern`. A function validator's result
  is used as is, so returning (or mutating) its input table aliases it — the input is not
  deep-copied there.

```lua
local v = lur.validate
local q = v.parse(v.object({
  q = v.str({ min_len = 1 }),
  limit = v.int({ min = 1, max = 100, default = 20 }),
}), { q = "luau", limit = "5" })           -- { q = "luau", limit = 5 }
local ok, err = v.try(v.int({ max = 10 }), "11")  -- false, err.issues[1].message == "must be <= 10"
```
- **`lur.log`** — `info`/`warn`/`error(msg)` write `<level>: <msg>\n` to stderr (stdout is
  the data channel).
- **`lur.stdin`** — `read()` drains all bytes, `read(n)` reads up to `n` (`nil` at EOF),
  `lines()` iterates newline-stripped lines.
- **`lur.stdout`** — `write(bytes)`, `flush()`. Raw bytes, no implicit newline.
- **`lur.args`** — `positional` (1-indexed array) and `flags` (`--name value`/
  `--name=value` → `"value"`, bare `--flag` → `true`).

### Capabilities (policy-gated)

- **`lur.fs`** — `read(path) → bytes`, `write(path, bytes)`. Paths are canonicalized
  before the allowlist check, defeating `..` and symlink escapes, and files are opened
  confined to the granted root, so a symlink swapped in mid-call cannot escape either.
- **`lur.http`** — `request(method, url, opts?)` plus `get`/`post`/`put`/`patch`/
  `delete`/`head(url, opts?)`. `opts`: `headers`, `query`, `body` **or** `json` **or** `form`
  (flat table of string/number/boolean, url-encoded with sorted keys; `Content-Type` set
  unless `headers` has one), `timeout` (ms), `cache`. `res.json({ null = "drop" })` as in `lur.json.decode`. Returns `{ status, body, headers, headers_all, json(), text(), html() }`; the helpers are
  closures called with a dot, like `json()` (`res.text()`). `text()` decodes `body`
  by a BOM, then the `Content-Type` charset, then `<meta charset>` in the first 1 KiB, then
  UTF-8 (an unknown header charset falls back to the `<meta>`/UTF-8 steps; invalid bytes
  become U+FFFD), the `lur.charset` rules, and
  `html()` is `lur.html.parse(res.text())`. Every request and
  redirect hop is checked against the allowlist and the private-IP (SSRF) guard; TLS is
  always verified; the body is capped by `--max-http-body`. Proxy environment variables
  (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) are ignored.
  `cache = { ttl_ms = 60000, vary? }` (GET only; in memory, no `--db`) stores 2xx responses
  and adds `res.cached`; requests with `Authorization`/`Cookie` bypass it unless named in
  `vary`, and the policy check runs before the lookup. `lur.http.cache_clear()` drops all
  entries and returns the count. The cache is bounded by `--http-cache-max-entries` and
  `--http-cache-max-bytes`: an insert over either cap evicts expired entries, then the least
  recently used; a single response larger than the byte cap is simply not cached.
- **`lur.env`** — `lur.env(name) → string | nil`; `nil` for both denied and unset, so it
  is not an oracle.

### Storage (requires `--db`)

`--db` takes a SQLite path or a `postgres://`/`postgresql://` URL; the scheme picks the
backend at first use. There is no SQL-portability layer — placeholders and types are
native to the backend.

- **`lur.db`** — `exec(sql, ...params) → { rows_affected, last_insert_id }`,
  `query(sql, ...params) → rows` (tables keyed by column), and `tx(fn)`, which runs
  `fn(tx)` on a pinned connection, committing on return and rolling back on error.
  Placeholders: `?` on SQLite, `$1, $2, …` on Postgres. JSON-encode tables before binding.
- **`lur.kv`** — `get(key) → bytes | nil`, `set(key, bytes)`, `delete(key)`, plus atomic
  `add(key, value) → bool` (set-if-absent), `cas(key, expected, new) → bool` (`nil`
  expected = must be absent, `nil` new = delete), `incr`/`decr(key, n?)` (integer
  counters, step 1; `get` returns them as decimal strings), and `update(key, fn)`
  (read-modify-write; return `nil` to delete). Expiry: pass `{ ttl_ms = n }` (positive
  milliseconds) as the last argument of `set`/`add`/`cas`/`update`/`incr`/`decr`; expired
  keys read as absent. `set` without `ttl_ms` clears the expiry, `cas`/`update` keep it,
  `incr`/`decr` set it only when the key has none (fixed window; `renew_ttl = true`
  resets it every call). `expire(key, ms) → bool`; `ttl(key) → ms, exists`.
- **SQLite contention** — write transactions use `BEGIN IMMEDIATE`; a 5 s `busy_timeout`
  plus up to 5 jittered attempts on single-statement writes, lock acquisition, and open
  absorb "database is locked".
- **Postgres row types** — only `int2`/`int4`/`int8` (integer), `float4`/`float8`
  (number), and `text`/`varchar`/`bpchar`/`name`/`bytea` (string) map to Lua. Anything
  else (`numeric`, `timestamptz`, `jsonb`, `uuid`, `bool`, arrays, …) raises
  `lur.db: unsupported column type '<T>' in column '<name>'; CAST it to text (e.g.
  <name>::text)`.
- **`last_insert_id` is SQLite-only** — always `0` on Postgres; use
  `INSERT … RETURNING id` via `db.query`.
- **TLS** — append `?sslmode=require` (or another `sslmode`) to the Postgres URL.
- **`db.tx` / `kv.update` can fail** — on Postgres they run at `SERIALIZABLE` and may
  raise a serialization conflict with a stable, locale-independent message containing
  `SQLSTATE 40001`; on SQLite they may raise once busy retries run out. `lur` does
  **not** retry them:

  ```lua
  local ok, err = pcall(function()
    return lur.db.tx(function(tx) --[[ … ]] end)
  end)
  ```

Postgres integration tests need a local Postgres: `docker compose up -d`.

### Concurrency

- **`lur.async`** — `sleep(ms)`, and combinators over arrays of zero-arg functions:
  `all` (fail-fast), `race` (first to settle), `any` (first to succeed), `settled` (never
  raises → `{ ok, value | err }`). `map(list, fn, opts?)` applies `fn(item, index)` to
  every array element concurrently and returns results in input order; `opts.limit`
  (integer ≥ 1) caps that call's in-flight tasks, `opts.drop_errors` omits failed items
  instead of failing fast. Lua runs one step at a time; tasks interleave only at I/O
  awaits. `--max-concurrency` caps in-flight tasks.
- **`lur.state`** — process-wide state shared across the VM pool, primitives only:
  `get(key)`, `set(key, value)` (`nil` deletes), `incr`/`decr(key, n?)` (atomic; integer
  step, default 1), `add(key, value) → bool` (set-if-absent), `cas(key, expected, new) →
  bool` (`nil` = absent), and `update(key, fn)` (optimistic CAS retry; `fn` runs
  unlocked). Writes (`set`/`add`/`cas`/`update`/`incr`/`decr`) take `{ ttl_ms = n,
  evictable = bool }` last, with `lur.kv`'s expiry rules (`set`/`add` clear it,
  `cas`/`update`/`incr`/`decr` keep it, `incr`/`decr` set it only when absent unless
  `renew_ttl = true`); `ttl(key) → ms, exists`. `fetch(key, { ttl_ms, stale_ms? }, fn) →
  value, "hit"|"miss"|"stale"` runs `fn` on a miss (async, unlocked, not retried) and stores
  the value as evictable; if `fn` raises and a value within `stale_ms` of expiry exists, that
  value comes back as `"stale"`. `nil` from `fn` is not cached. `--state-max-entries` /
  `--state-max-bytes` (default unlimited) drop only `evictable` entries, expired first, then
  least recently used; a write that cannot fit raises. A stale value is invisible to
  everything but `fetch`.

### Server mode (`lur serve`)

Handlers are registered once at load time.

- **`lur.serve.http(method, path, handler)`** — `method` is `"GET"`…/`"ANY"`. `:name`
  path segments (e.g. `/users/:id`) bind into `req.params`; a trailing `:name?` is optional
  (`/feed/:tag?` matches `/feed` and `/feed/lua`, absent = `nil`; it may not share a
  static/param shape with another route of the same method, e.g. `/feed`). The most specific route wins regardless of order (more static segments, then a
  concrete method over `ANY`).
  `handler(req)` returns `{ status?, headers?, body? }` (`status` defaults to `200`, must
  be in `100..=599`; `body` defaults to empty). `headers` maps a name to a string or an
  array of strings (repeated header, e.g. two `Set-Cookie`). No `Content-Type` is
  inferred. An invalid name/value (including CR/LF), or `Content-Length` /
  `Transfer-Encoding`, is a **500**.
- **`lur.serve.cron(spec, handler, opts?)`** — 6-field cron (`sec min hour dom mon dow`).
  `opts`: `name`, `overlap` (default `false` = skip a tick while the previous run is
  going), `timeout` (ms).
- **`lur.serve.abort(status, message, data?)`** — ends the handler at once with an HTTP
  error. `status` must be an integer in `400..=599` (`abort(200, …)` is an argument error),
  `message` a string, `data` any JSON-encodable value. Uncaught, the client gets that
  status with `Content-Type: application/json` and `{"error": message, "data": data}`
  (`data` omitted when `nil`); not-encodable `data` is an ordinary error (a 500). It raises
  a table `{ kind = "abort", status, message, data }` that `pcall` can catch; side effects
  made before the abort (db writes, …) are **not** rolled back. Only valid inside an HTTP
  handler: in a cron job or one-shot script it is an ordinary runtime error. Raised inside
  a `lur.async` task, `pcall` around the combinator sees a string, not the table (use it
  in the handler itself).

`req` has `method`, `path`, `origin` and `url` (`scheme://host` and the full URL, from `Host`
and `X-Forwarded-Proto`; `nil` without a valid `Host`; client-controlled, so for links only),
`params`, `query` (last value per key), `query_all` (all
values), `headers` (lowercased), `cookies` (parsed `Cookie`; empty table if absent),
`body` (raw bytes), `json()`, `form()` and `form_all()`. `read(n)` streams large bodies in
chunks; after that, `body`/`json()`/`form()`/`form_all()`/`valid_body` raise.

`req.form()` parses an `application/x-www-form-urlencoded` body (the media type is matched
case-insensitively, parameters such as `; charset=utf-8` are ignored) into `{key = last
value}`; `req.form_all()` returns `{key = {values...}}`, like `query` / `query_all`. Decoding
is the query-string decoding: `+` is a space, `%XX` is decoded, a malformed `%` stays
literal, a key without `=` has value `""`, an empty body gives `{}`. Any other content type
(or none) raises an ordinary error naming it, so branch on `req.headers["content-type"]`
first if you accept several. Multipart is not supported. `--max-body` applies as usual.

`req.valid_body(schema, opts?)` validates the body like `valid_query`, picking the decoder
from `Content-Type`: `application/json` and `+json` types decode as `req.json()`;
urlencoded forms are validated with `list(...)` fields of an `object` schema fed from
`form_all`. Anything else, including a missing type and `multipart/*`, aborts with **415**
`unsupported content type`. A body that is not valid JSON aborts with **400**
`invalid JSON body` (always 400, regardless of `fail_status`); so does an empty JSON body,
unless the schema accepts `nil` (`v.optional(...)`), in which case it returns `nil`.
Validation failures abort with `opts.fail_status` (default 400) and `data = issues`.

`req.valid_query(schema, opts?)` and `req.valid_params(schema, opts?)` run
`lur.validate.parse` over `req.query` / `req.params` and return the converted value (a
fresh table). On failure they abort with `opts.fail_status` (integer `400..=599`, default
`400`; checked at call time) and `message = "validation failed"`, `data = issues`:
`{"error":"validation failed","data":[{"path":"limit","message":"must be <= 100"}]}`.
Use `fail_status = 404` for path params. For an `object` schema, `list(...)` fields in
`valid_query` read every value (`query_all`) and other fields the last one (`query`).
Anything that is not a validation failure (a bad schema argument, a timeout) propagates
unchanged. A bare `lur.validate.parse` that fails inside a handler stays a **500** on
purpose: use `req.valid_*` for client input.

```lua
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
  local b = req.valid_body(v.object({ email = v.str(), age = v.int({ min = 13 }) }))
  return { status = 201, body = lur.json.encode(b) } -- 415 unless JSON or urlencoded
end)

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
```

On `SIGTERM`/`SIGINT`, in-flight requests and cron runs drain within `--shutdown-grace`.

### Diagnostics

Errors show the script path, failing line, and a source snippet (rustc-style), then a
stack traceback. Server handler and cron errors render the same way (a handler error
still returns `500`). Capability argument mistakes name the function, e.g.
`lur.crypto.sha256: argument #1 must be string, got table`.

Output is colorized only on a terminal; a non-empty [`NO_COLOR`](https://no-color.org)
disables it.

## Development

```sh
cargo nextest run        # tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo bench --bench runtime
```

CI runs lint, tests, coverage (Codecov), and a benchmark report on every push and PR.
Internals: [ARCHITECTURE.md](ARCHITECTURE.md).

## License

Licensed under the [MIT License](LICENSE.txt).
