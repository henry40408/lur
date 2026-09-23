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
| `--allow-net` | HOST | | Allow a host or `host:port` (repeatable). |
| `--allow-private` | — | off | Permit loopback/private/link-local addresses (SSRF guard off). |
| `--memory` | SIZE | `256m` | Per-VM memory cap; `0` = unlimited. |
| `--max-http-body` | SIZE | `16m` | Cap on a buffered `lur.http` response body. |
| `--max-concurrency` | N | unbounded | Cap on in-flight `lur.async.*` tasks per VM. |
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
  to `lur.null`; strings must be UTF-8 (base64 binary first).
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
  Divide by `1000` for `os.date`.
- **`lur.log`** — `info`/`warn`/`error(msg)` write `<level>: <msg>\n` to stderr (stdout is
  the data channel).
- **`lur.stdin`** — `read()` drains all bytes, `read(n)` reads up to `n` (`nil` at EOF),
  `lines()` iterates newline-stripped lines.
- **`lur.stdout`** — `write(bytes)`, `flush()`. Raw bytes, no implicit newline.
- **`lur.args`** — `positional` (1-indexed array) and `flags` (`--name value`/
  `--name=value` → `"value"`, bare `--flag` → `true`).

### Capabilities (policy-gated)

- **`lur.fs`** — `read(path) → bytes`, `write(path, bytes)`. Paths are canonicalized
  before the allowlist check, defeating `..` and symlink escapes.
- **`lur.http`** — `request(method, url, opts?)` plus `get`/`post`/`put`/`patch`/
  `delete`/`head(url, opts?)`. `opts`: `headers`, `query`, `body` **or** `json`, `timeout`
  (ms). Returns `{ status, body, headers, headers_all, json() }`. Every request and
  redirect hop is checked against the allowlist and the private-IP (SSRF) guard; TLS is
  always verified; the body is capped by `--max-http-body`.
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
  (read-modify-write; return `nil` to delete).
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
  raises → `{ ok, value | err }`). Lua runs one step at a time; tasks interleave only at
  I/O awaits. `--max-concurrency` caps in-flight tasks.
- **`lur.state`** — process-wide state shared across the VM pool, primitives only:
  `get(key)`, `set(key, value)` (`nil` deletes), `incr`/`decr(key, n?)` (atomic; integer
  step, default 1), `add(key, value) → bool` (set-if-absent), `cas(key, expected, new) →
  bool` (`nil` = absent), and `update(key, fn)` (optimistic CAS retry; `fn` runs
  unlocked).

### Server mode (`lur serve`)

Handlers are registered once at load time.

- **`lur.serve.http(method, path, handler)`** — `method` is `"GET"`…/`"ANY"`. `:name`
  path segments (e.g. `/users/:id`) bind into `req.params`. The most specific route wins
  regardless of order (more static segments, then a concrete method over `ANY`).
  `handler(req)` returns `{ status?, headers?, body? }` (`status` defaults to `200`, must
  be in `100..=599`; `body` defaults to empty). `headers` maps a name to a string or an
  array of strings (repeated header, e.g. two `Set-Cookie`). No `Content-Type` is
  inferred. An invalid name/value (including CR/LF), or `Content-Length` /
  `Transfer-Encoding`, is a **500**.
- **`lur.serve.cron(spec, handler, opts?)`** — 6-field cron (`sec min hour dom mon dow`).
  `opts`: `name`, `overlap` (default `false` = skip a tick while the previous run is
  going), `timeout` (ms).

`req` has `method`, `path`, `params`, `query` (last value per key), `query_all` (all
values), `headers` (lowercased), `cookies` (parsed `Cookie`; empty table if absent),
`body` (raw bytes), and `json()`. `read(n)` streams large bodies in chunks; after that,
`body`/`json()` raise.

```lua
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
