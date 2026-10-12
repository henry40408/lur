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
and the whole `lur.*` tree (nested tables included) are frozen, so scripts cannot monkeypatch
`lur.*`; in server mode each request/job runs in a fresh environment whose writes
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
Lua error (catch with `pcall`). Every `lur.*` table is read-only.

Sections: [Data & I/O](#data--io) · [Validation](#validation-lurvalidate) ·
[Capabilities](#capabilities-policy-gated) · [Storage](#storage-requires---db) ·
[Concurrency](#concurrency) · [Server mode](#server-mode-lur-serve) ·
[Diagnostics](#diagnostics)

### Data & I/O

| Module | Purpose |
| --- | --- |
| [`lur.null`](#lurnull) | sentinel for JSON `null` / SQL `NULL` |
| [`lur.json`](#lurjson) | encode / decode JSON |
| [`lur.base64`](#lurbase64) | base64 encode / decode |
| [`lur.crypto`](#lurcrypto) | digests, HMAC, hex, random bytes |
| [`lur.cookie`](#lurcookie) | `Cookie` / `Set-Cookie` helpers |
| [`lur.time`](#lurtime) | clocks, parsing and formatting |
| [`lur.url`](#lururl) | URL parse / join / query strings |
| [`lur.charset`](#lurcharset) | non-UTF-8 text |
| [`lur.html`](#lurhtml) | HTML parse, CSS selectors, sanitize |
| [`lur.xml`](#lurxml) | XML parse, slash-path selection |
| [`lur.feed`](#lurfeed) | RSS / Atom / JSON Feed output |
| [`lur.log`](#lurlog), [`lur.stdin`](#lurstdin), [`lur.stdout`](#lurstdout), [`lur.args`](#lurargs) | process I/O |

#### `lur.null`

Sentinel distinct from `nil` (which means *absent*); round-trips JSON `null` and SQL
`NULL`.

#### `lur.json`

| Call | Returns |
| --- | --- |
| `encode(value)` | string |
| `decode(text, opts?)` | value |

- JSON `null` decodes to `lur.null`; strings must be UTF-8 (base64 binary first).
- `decode(text, { null = "drop" })` omits object keys whose value is `null` and removes
  `null` array elements (compacting the array); `"keep"` is the default, and a bare
  top-level `null` stays `lur.null`.

#### `lur.base64`

`encode(bytes) → string`, `decode(text) → bytes`.

#### `lur.crypto`

| Call | Returns | Notes |
| --- | --- | --- |
| `sha256` / `sha512` / `sha1` / `md5(data)` | bytes | `sha1`/`md5` are for legacy interop only |
| `hmac_sha256` / `hmac_sha512` / `hmac_sha1(key, msg)` | bytes | |
| `hex.encode(bytes)` / `hex.decode(text)` | string / bytes | |
| `random_bytes(n)` | bytes | OS CSPRNG |
| `constant_eq(a, b)` | bool | timing-safe |

Digests are raw bytes.

```lua
print(lur.crypto.hex.encode(lur.crypto.sha256("hello")))
```

#### `lur.cookie`

| Call | Returns | Notes |
| --- | --- | --- |
| `parse(header)` | `{ name = value, … }` | reads a `Cookie` header |
| `serialize(name, value, opts?)` | string | builds one `Set-Cookie` value (no prefix) |

- `parse` is lenient: malformed segments are skipped, a later duplicate wins, nothing is
  decoded.
- `serialize` options:

  | Option | Type |
  | --- | --- |
  | `domain`, `path`, `expires` | string |
  | `max_age` | integer seconds |
  | `secure`, `http_only` | boolean |
  | `same_site` | `"Strict"` / `"Lax"` / `"None"` |

- An invalid name, a value with `;`/CR/LF, or `same_site="None"` without `secure=true`
  raises.
- Format `expires` with `os.date("!%a, %d %b %Y %H:%M:%S GMT", t)`.

```lua
print(lur.cookie.serialize("sid", "abc", { path = "/", http_only = true, max_age = 3600 }))
```

#### `lur.time`

Integer milliseconds throughout (divide by `1000` for `os.date`). Malformed input raises.

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
| `parse(text, fmt?, tz?)` | epoch ms | see below |
| `try_parse(text, fmt?, tz?)` | epoch ms or `nil` | `parse` that returns `nil` for unparseable (or `nil`) text; a bad `fmt` or `tz` still raises |

- `tz` is an IANA name (`"Asia/Taipei"`), a fixed offset (`"+08:00"`), or `nil`/`"UTC"`.
- `parse` without `fmt` accepts RFC 3339, RFC 2822, HTTP-date, then
  `YYYY-MM-DD[ T]HH:MM[:SS[.f]]` and `YYYY/MM/DD[ HH:MM[:SS]]`; wall-clock text with no
  offset is read in `tz`.
- A local time in a DST gap raises; in an overlap the earlier instant wins.

```lua
local ms = lur.time.parse("2024-03-01 12:00", nil, "Asia/Taipei")
print(lur.time.format_rfc3339(ms, "+08:00"))
```

#### `lur.url`

| Call | Returns | Notes |
| --- | --- | --- |
| `parse(url)` | `{ href, scheme, host?, port?, username?, password?, path, query?, fragment? }` | `port` only when explicit and non-default; an invalid URL raises |
| `join(base, relative)` | string | resolves relative links; `base` must be absolute |
| `encode_query(table)` | string | keys sorted; an array value repeats the key; string, number and boolean values |
| `decode_query(text)` | table | leading `?` optional, last duplicate wins, `+` is a space |

```lua
print(lur.url.join("https://example.com/a/b", "../c?x=1"))
print(lur.url.encode_query({ b = 2, a = { "x", "y" } }))
```

#### `lur.charset`

For non-UTF-8 pages (Big5, GBK, Shift_JIS, … via `encoding_rs`, WHATWG labels).

| Call | Returns |
| --- | --- |
| `decode(bytes, hint?)` | string |
| `encode(text, label)` | bytes |

- `hint` is a charset label or a whole `Content-Type` value.
- Precedence: BOM, then `hint`, then a `<meta charset>` in the first 1 KiB, then UTF-8.
- Invalid bytes become U+FFFD; an unknown label raises.
- `encode` writes unmappable characters as `&#NNN;` and refuses UTF-16.

#### `lur.html`

`parse(html) → doc`. Invalid UTF-8 is replaced; decode other encodings with `lur.charset`
first. Parsing follows the HTML5 algorithm, so `<td>` outside a `<table>` is dropped.

Nodes (the doc and its elements) share these methods; call them with `:`.

| Method | Returns | Notes |
| --- | --- | --- |
| `select(css)` | array | descendants only |
| `select_one(css)` | node or `nil` | descendants only; for both, an invalid selector raises |
| `text(opts?)` | string | `{ trim = true }` strips surrounding whitespace; `lur.html` only |
| `text(css)` | string or `nil` | shorthand for `select_one(css):text()`; `nil` when nothing matches |
| `html()`, `inner_html()`, `tag()` | string | |
| `attr(name)` | string or `nil` | |
| `attrs()` | table | |
| `parent()` | node or `nil` | `nil` for the document and the root element |
| `children()` | array | elements only |

`lur.html.sanitize(html, opts?) → string` strips scripts, styles, event handlers and unsafe
URLs with `ammonia`'s allowlist.

- Only `http`, `https` and `mailto` links survive, and links get
  `rel="noopener noreferrer"`.
- `opts.base` (absolute URL) resolves relative `href`/`src`; without it they are kept as
  written.
- Broken markup is repaired.

```lua
local doc = lur.html.parse('<ul><li class="a">one</li><li>two</li></ul>')
print(doc:text("li.a"), #doc:select("li"))
print(lur.html.sanitize('<a href="javascript:x" onclick="y">hi</a><script>1</script>'))
```

#### `lur.xml`

`parse(xml) → doc`. Same node methods as `lur.html` (`select`, `select_one`, `text`,
`tag`, `attr`, `attrs`, `parent`, `children`) but selection takes a slash path.

| Path | Meaning |
| --- | --- |
| `a/b` | children |
| `//a` | descendants (also mid-path) |
| `*` | any name |

- Results are in document order and deduplicated.
- Names match exactly as written (`dc:creator`); namespaces are not resolved.
- Malformed XML raises with line/column.
- Unknown entities such as `&nbsp;` are kept as text; `DOCTYPE` is ignored, so custom
  entities are not expanded.
- A BOM or the declaration's `encoding` selects the charset; otherwise invalid UTF-8 is
  replaced.

```lua
local doc = lur.xml.parse("<r><a>1</a><b><a>2</a></b></r>")
print(#doc:select("//a"), doc:select_one("r/a"):text())
```

#### `lur.feed`

`rss(meta, items)`, `atom(meta, items)`, `json(meta, items)` → string (RSS 2.0 / Atom 1.0 /
JSON Feed 1.1).

| Field | Where | Notes |
| --- | --- | --- |
| `title` | `meta`, item | required |
| `feed_url`, `description`, `language`, `updated` | `meta` | optional |
| `link`, `id` | `meta` | optional (see format requirements below) |
| `guid`, `link` | item | optional (see format requirements below) |
| `date`, `updated`, `summary`, `content` (HTML), `author` | item | optional |
| `categories` | item | array of strings |
| `enclosure` | item | `{ url, type?, length? }` |

- Dates are epoch ms.
- RSS needs `meta.link`; Atom needs `meta.id` or `meta.link`; Atom/JSON items need `guid`
  or `link`.
- XML is escaped and characters illegal in XML 1.0 are dropped.
- An optional third argument `{ skip_invalid = true }` skips items whose `title` is missing
  (these raise by default) and also drops empty or blank titles, which are accepted by
  default; other item errors still raise.

```lua
print(lur.feed.rss(
  { title = "Blog", link = "https://example.com/" },
  { { title = "Hello", link = "https://example.com/1", date = lur.time.now_ms() } }
))
```

#### `lur.log`

`info`/`warn`/`error(msg)` write `<level>: <msg>\n` to stderr (stdout is the data channel).

#### `lur.stdin`

`read()` drains all bytes, `read(n)` reads up to `n` (`nil` at EOF), `lines()` iterates
newline-stripped lines.

#### `lur.stdout`

`write(bytes)`, `flush()`. Raw bytes, no implicit newline.

#### `lur.args`

`positional` (1-indexed array) and `flags` (`--name value`/`--name=value` → `"value"`,
bare `--flag` → `true`). Read-only, like every `lur.*` table; copy into a local table to
modify.

### Validation (`lur.validate`)

Validate and convert untrusted input (query strings, JSON bodies) with small composable
schemas. Pure compute, no policy, same in one-shot and serve mode. The result is always a
new table; the input is never modified.

#### Entry points

| Call | Returns | On failure |
| --- | --- | --- |
| `parse(schema, value)` | converted value | raises `err` |
| `try(schema, value)` | `true, converted` or `false, err` | returns the same `err` |

#### Schemas

`opts` is always optional.

| Schema | Options | Accepts |
| --- | --- | --- |
| `str{ … }` | `pattern?`, `min_len?`, `max_len?`, `default?` | strings only (numbers are not coerced); `pattern` is a Lua pattern as in `string.find`, unanchored; lengths count characters |
| `int{ … }` | `min?`, `max?`, `default?`, `clamp?` | a number with an integral value, or a string that is strictly `-?%d+`; \|n\| must be <= 2^53 |
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

#### Missing values and defaults

- Values are required unless wrapped in `optional(...)` or given a `default` (which fills
  a missing value as-is).
- For `int`/`number`/`bool`, `""` counts as missing, so `?limit=` falls back to the default.
- `optional(x)` only waives "required": an inner `default` still fills.
- A `lur.null` (JSON `null`) given for an object field or the root counts as absent:
  `optional` yields `nil`, `default` fills, and a required value reports `is required`.
  Function validators still receive the raw `lur.null`, and `null` list items are not
  absent (they fail normally).
- `default` values are returned as given, not checked against `min`/`max`/`pattern`.
- A function validator's result is used as is, so returning (or mutating) its input table
  aliases it — the input is not deep-copied there.

#### Errors

On failure `err` is a table:

```lua
{ kind = "validation", message = "…", issues = { { path = "a.b[2].c", message = "…" }, … } }
```

- All issues are collected; paths read `a.b[2].c`, the root is `""`.
- At most 100 issues are kept, then `truncated = true`.
- `tostring(err)` is `message`, an uncaught one prints that message, and it is catchable
  with `pcall`.
- Raised inside a `lur.async` task it reaches the combinator's caller as a plain string
  (its message), not a table; use `try` there.

#### Examples

```lua
local v = lur.validate
local q = v.parse(v.object({
  q = v.str({ min_len = 1 }),
  limit = v.int({ min = 1, max = 100, default = 20 }),
}), { q = "luau", limit = "5" })           -- { q = "luau", limit = 5 }
print(q.q, q.limit)
```

```lua
local v = lur.validate
local ok, err = v.try(v.int({ max = 10 }), "11")
print(ok, err.issues[1].message)           -- false   must be <= 10
```

### Capabilities (policy-gated)

#### `lur.fs`

`read(path) → bytes`, `write(path, bytes)`.

- Paths are canonicalized before the allowlist check, defeating `..` and symlink escapes.
- Files are opened confined to the granted root, so a symlink swapped in mid-call cannot
  escape either.

#### `lur.http`

`request(method, url, opts?)` plus `get`/`post`/`put`/`patch`/`delete`/`head(url, opts?)`.

| `opts` | Type | Meaning |
| --- | --- | --- |
| `headers` | table | request headers |
| `query` | table | query parameters |
| `body` / `json` / `form` | one of | raw body, JSON-encoded value, or a flat table of string/number/boolean (url-encoded with sorted keys; `Content-Type` set unless `headers` has one) |
| `timeout` | ms | per-request timeout; no default of its own (the global `--timeout` still applies) |
| `cache` | table | see [Caching](#caching) |

Returns `{ status, body, headers, headers_all, json(), text(), html() }`. The helpers are
closures: call them with a dot, e.g. `res.text()`, not `res:text()`.

| Helper | Behaviour |
| --- | --- |
| `res.json(opts?)` | decodes `body`; `opts` as in `lur.json.decode` |
| `res.text()` | decodes `body` with the `lur.charset` rules: a BOM, then the `Content-Type` charset, then `<meta charset>` in the first 1 KiB, then UTF-8 (an unknown header charset falls back to the `<meta>`/UTF-8 steps; invalid bytes become U+FFFD) |
| `res.html()` | `lur.html.parse(res.text())` |

Guards:

- Every request and redirect hop is checked against the allowlist and the private-IP
  (SSRF) guard.
- TLS is always verified; the body is capped by `--max-http-body`.
- Proxy environment variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) are ignored.

##### Caching

`cache = { ttl_ms = 60000, vary? }`

- GET only; in memory, no `--db`. Stores 2xx responses and adds `res.cached`.
- Requests with `Authorization`/`Cookie` bypass it unless named in `vary`, and the policy
  check runs before the lookup.
- `lur.http.cache_clear()` drops all entries and returns the count.
- The cache is bounded by `--http-cache-max-entries` and `--http-cache-max-bytes`: an
  insert over either cap evicts expired entries, then the least recently used; a single
  response larger than the byte cap is simply not cached.

#### `lur.env`

`lur.env(name) → string | nil` (`lur.env` is itself the function). `nil` for both denied
and unset, so it is not an oracle.

### Storage (requires `--db`)

`--db` takes a SQLite path or a `postgres://`/`postgresql://` URL; the scheme picks the
backend at first use. There is no SQL-portability layer — placeholders and types are
native to the backend.

#### `lur.db`

| Call | Returns | Notes |
| --- | --- | --- |
| `exec(sql, ...params)` | `{ rows_affected, last_insert_id }` | |
| `query(sql, ...params)` | rows (tables keyed by column) | |
| `tx(fn)` | | runs `fn(tx)` on a pinned connection, committing on return and rolling back on error |

- Placeholders: `?` on SQLite, `$1, $2, …` on Postgres. JSON-encode tables before binding.
- **`last_insert_id` is SQLite-only** — always `0` on Postgres; use
  `INSERT … RETURNING id` via `db.query`.
- **Postgres row types** — only `int2`/`int4`/`int8` (integer), `float4`/`float8`
  (number), and `text`/`varchar`/`bpchar`/`name`/`bytea` (string) map to Lua. Anything
  else (`numeric`, `timestamptz`, `jsonb`, `uuid`, `bool`, arrays, …) raises
  `lur.db: unsupported column type '<T>' in column '<name>'; CAST it to text (e.g.
  <name>::text)`.
- **TLS** — append `?sslmode=require` (or another `sslmode`) to the Postgres URL.
- **SQLite contention** — write transactions use `BEGIN IMMEDIATE`; a 5 s `busy_timeout`
  plus up to 5 jittered attempts on single-statement writes, lock acquisition, and open
  absorb "database is locked".
- **`db.tx` / `kv.update` can fail** — on Postgres they run at `SERIALIZABLE` and may
  raise a serialization conflict with a stable, locale-independent message containing
  `SQLSTATE 40001`; on SQLite they may raise once busy retries run out. `lur` does
  **not** retry them:

  ```lua
  local ok, err = pcall(function()
    return lur.db.tx(function(tx) --[[ … ]] end)
  end)
  ```

#### `lur.kv`

| Call | Returns | Notes |
| --- | --- | --- |
| `get(key)` | bytes or `nil` | counters come back as decimal strings |
| `set(key, bytes)` | | |
| `delete(key)` | | |
| `add(key, value)` | bool | atomic set-if-absent |
| `cas(key, expected, new)` | bool | atomic; `nil` expected = must be absent, `nil` new = delete |
| `incr` / `decr(key, n?)` | | atomic integer counters, step 1 |
| `update(key, fn)` | | atomic read-modify-write; return `nil` to delete |
| `expire(key, ms)` | bool | |
| `ttl(key)` | `ms, exists` | |

Expiry: pass `{ ttl_ms = n }` (positive milliseconds) as the last argument of
`set`/`add`/`cas`/`update`/`incr`/`decr`; expired keys read as absent.

| Call | Effect on an existing expiry |
| --- | --- |
| `set` without `ttl_ms` | clears it |
| `cas` / `update` | keep it |
| `incr` / `decr` | set it only when the key has none (fixed window); `renew_ttl = true` resets it every call |

### Concurrency

#### `lur.async`

`sleep(ms)`, and combinators over arrays of zero-arg functions:

| Call | Behaviour |
| --- | --- |
| `all(fns)` | fail-fast |
| `race(fns)` | first to settle |
| `any(fns)` | first to succeed |
| `settled(fns)` | never raises → `{ ok, value \| err }` |
| `map(list, fn, opts?)` | applies `fn(item, index)` to every array element concurrently; results in input order |

- `map` options: `opts.limit` (integer ≥ 1) caps that call's in-flight tasks;
  `opts.drop_errors` omits failed items instead of failing fast.
- Lua runs one step at a time; tasks interleave only at I/O awaits.
- `--max-concurrency` caps in-flight tasks.

```lua
local r = lur.async.map({ 1, 2, 3 }, function(n) lur.async.sleep(10); return n * 2 end,
  { limit = 2 })
print(table.concat(r, ","))
```

#### `lur.state`

Process-wide state shared across the VM pool, primitives only.

| Call | Returns | Notes |
| --- | --- | --- |
| `get(key)` | value | |
| `set(key, value)` | | `nil` deletes |
| `incr` / `decr(key, n?)` | | atomic; integer step, default 1 |
| `add(key, value)` | bool | set-if-absent |
| `cas(key, expected, new)` | bool | `nil` = absent |
| `update(key, fn)` | | optimistic CAS retry; `fn` runs unlocked |
| `ttl(key)` | `ms, exists` | |
| `fetch(key, { ttl_ms, stale_ms? }, fn)` | `value, "hit"\|"miss"\|"stale"` | see below |

- Writes (`set`/`add`/`cas`/`update`/`incr`/`decr`) take `{ ttl_ms = n, evictable = bool }`
  last, with the same expiry rules as the [`lur.kv` expiry table](#lurkv) except that `add`
  also clears an existing expiry and `evictable` is state-only.
- `fetch` runs `fn` on a miss (async, unlocked, not retried) and stores the value as
  evictable. If `fn` raises and a value within `stale_ms` of expiry exists, that value
  comes back as `"stale"`. `nil` from `fn` is not cached.
- `--state-max-entries` / `--state-max-bytes` (default unlimited) drop only `evictable`
  entries, expired first, then least recently used; a write that cannot fit raises.
- A stale value is invisible to everything but `fetch`.

### Server mode (`lur serve`)

Handlers are registered once at load time. On `SIGTERM`/`SIGINT`, in-flight requests and
cron runs drain within `--shutdown-grace`.

#### Routing: `lur.serve.http(method, path, handler)`

`method` is `"GET"`…/`"ANY"`.

- **Path params**: `:name` segments (e.g. `/users/:id`) bind into `req.params`. A trailing
  `:name?` is optional (`/feed/:tag?` matches `/feed` and `/feed/lua`, absent = `nil`); it
  may not share a static/param shape with another route of the same method, e.g. `/feed`.
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

#### `lur.serve.cron(spec, handler, opts?)`

6-field cron (`sec min hour dom mon dow`).

| `opts` | Default | Meaning |
| --- | --- | --- |
| `name` | `cron[<spec>]` | job name, shown in logs |
| `overlap` | `false` | `false` skips a tick while the previous run is going |
| `timeout` | `--timeout` | ms; overrides the global per-event timeout |

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

##### Validation helpers

`req.valid_query(schema, opts?)`, `req.valid_params(schema, opts?)` and
`req.valid_body(schema, opts?)` run [`lur.validate`](#validation-lurvalidate) over client
input and return the converted value (a fresh table, except `valid_body` may return
`nil`; see below).

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

#### Server examples

```lua
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

lur.serve.cron("0 * * * * *", function() lur.log.info("tick") end, { name = "minutely" })
```

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
docker compose up -d     # Postgres for tests/pg.rs (skipped locally if down)
```

CI runs lint, tests, coverage (Codecov), and a benchmark report on every push and PR.
Internals: [ARCHITECTURE.md](ARCHITECTURE.md).

## License

Licensed under the [MIT License](LICENSE.txt).
