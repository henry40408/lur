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

Write your automation and services in [Luau](https://luau.org/); `lur` runs them
safely behind a capability sandbox.

`lur` has two modes from one binary:

- **one-shot** — `lur script.lua` runs a script once and exits (glue, ETL, cron jobs).
- **server** — `lur serve app.lua` runs a long-lived HTTP service with scheduled jobs.

The binary name, the Lua global prefix (`lur.*`), and the project are all `lur`.

## Design principles

- **Flexible first, sandboxed always.** Scripts may be yours (semi-trusted) or from
  the internet (untrusted), so every side effect goes through a capability policy.
- **Secure by default.** The shipped default is the `strict` profile: no filesystem,
  no network, no environment, no private-IP access until you grant it.
- **One binary, two modes, shared core.** A sandboxed Luau VM, host modules (`lur.*`),
  the policy layer, and a tokio async core are shared between both modes.

## Install

Requires a Rust toolchain (edition 2024).

```sh
git clone https://github.com/henry40408/lur
cd lur
cargo build --release
# binary at ./target/release/lur
```

### Docker

Published as a multi-arch (`linux/amd64`, `linux/arm64`) image on GHCR. The binary
is a static musl build on `distroless/static` (CA certificates included, runs as a
non-root user, no shell):

Tags: `:main` tracks the latest commit on `main`; releases publish `:X.Y.Z`, `:X.Y`,
and `:latest`. `lur --version` reports the published tag for a release, a
`<ref>-<short-sha>` marker (e.g. `main-1a2b3c4`) for the `:main` image, the `git
describe` output for a local source build, and `dev` only when none of those are
available.

```sh
docker run --rm ghcr.io/henry40408/lur:latest --version

# run a script (mount it; grant capabilities with the usual flags)
docker run --rm -v "$PWD/app.lua:/app.lua:ro" \
  ghcr.io/henry40408/lur:latest --allow-net example.com /app.lua
```

Build the image locally (cross-compiles both arches via `cargo-zigbuild`, no qemu):

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
  lur.log.info("tick every 5 minutes\n")
end)
```

```sh
lur serve app.lua --bind 0.0.0.0:8080
```

## The sandbox

Every capability is denied until a policy grants it. Two profiles select the baseline:

| Profile | Filesystem | Network | Env | Private IPs |
| --- | --- | --- | --- | --- |
| `strict` *(default)* | none | none | none | denied |
| `loose` (`-A`) | full | any host | all | allowed |

Standard Luau is sandboxed: `os.execute`, `io`, `loadfile`/`dofile`, and `package` are
absent, and `require`, `getfenv`, `setfenv`, and `loadstring` are removed (they would
reach the writable global environment and defeat per-request isolation). `string`,
`table`, `math`, `bit32`, `utf8`, and `coroutine` remain available. The global table is
frozen, and in server mode each request/job runs in a fresh environment whose writes are
discarded, so scripts cannot leak state across calls.

## CLI reference

```
lur <script.lua> [SCRIPT_ARGS...]      # one-shot
lur serve <app.lua> [FLAGS]            # server
```

`lur docs` prints the embedded usage guide.

`SIZE` accepts a binary (×1024) suffix: bare/`b`, `k`/`kb`, `m`/`mb`, `g`/`gb`
(e.g. `256m`). `DURATION` accepts `ms`, `s`, `m` (minutes), `h` (e.g. `500ms`, `2m`).

### Common flags

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--strict` | — | (shipped default) | Deny-by-default profile. |
| `--loose` | — | | Permissive profile (full access). Conflicts with `--strict`. |
| `-A`, `--allow-all` | — | | Alias for the loose profile. |
| `--allow-fs-read` | PATH | | Add a readable root (repeatable). |
| `--allow-fs-write` | PATH | | Add a writable root (repeatable). |
| `--allow-fs` | PATH | | Add a root to both read and write (repeatable). |
| `--allow-env` | NAME | | Allow reading an environment variable (repeatable). |
| `--allow-net` | HOST | | Allow a host or `host:port` (repeatable). |
| `--allow-private` | — | off | Permit loopback/private/link-local addresses (SSRF guard off). |
| `--memory` | SIZE | `256m` | Per-VM memory cap; `0` means unlimited. |
| `--max-http-body` | SIZE | `16m` | Cap on a buffered `lur.http` response body. |
| `--max-concurrency` | N | unbounded | Cap on concurrent `lur.async.*` tasks per VM. |
| `--db` | PATH or URL | | SQLite file, or a `postgres://`/`postgresql://` connection string, backing `lur.db` / `lur.kv` (the scheme selects the backend at first use). |
| `--config` | FILE | | Load a specific config file. Conflicts with `--no-config`. |
| `--no-config` | — | | Ignore all config — pure shipped strict, zero grants. |

### One-shot only

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--timeout` | DURATION | none | Wall-clock limit; the run exits non-zero on timeout. |

### Server only

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--bind` | ADDR | `0.0.0.0:8080` | Listener address. |
| `--pool-size` | N | CPU count | Pre-warmed VMs; caps concurrent requests. |
| `--timeout` | DURATION | none | Per-request limit; on timeout the request gets `503`. |
| `--max-body` | SIZE | none | Max request body; a larger request gets `413`. |
| `--shutdown-grace` | DURATION | `10s` | Drain window for in-flight work on `SIGTERM`/`SIGINT`. |

### Config file

If neither `--config` nor `--no-config` is given, `lur` looks for
`$XDG_CONFIG_HOME/lur/config` (falling back to `~/.config/lur/config`). A missing file
is not an error. The format is TOML:

```toml
default_profile = "strict"   # or "loose"

[allow]
net      = ["api.github.com", "10.0.0.5:6379"]
fs_read  = ["~/data", "/var/log"]
fs_write = ["./out"]
env      = ["API_KEY", "DEBUG"]
```

CLI flags and config merge as follows: the **profile** is last-wins (a `--strict`/
`--loose`/`-A` flag overrides `default_profile`), while **allowlists** are additive (the
config's standing grants are unioned with per-run flags). `~` in config paths expands
against `$HOME`.

## Lua API

Everything is exposed under the `lur` global. Functions raise a Lua error on failure
(catch with `pcall`); a policy denial raises just like any other error.

### Data & I/O

- **`lur.null`** — sentinel distinct from `nil`. Round-trips JSON `null` and SQL `NULL`
  (a `nil` in a table means the key is *absent*).
- **`lur.json`** — `encode(value) → string`, `decode(text) → value`. JSON `null`
  decodes to `lur.null`; strings must be valid UTF-8 (base64-encode binary first).
- **`lur.base64`** — `encode(bytes) → string`, `decode(text) → bytes`.
- **`lur.crypto`** — pure-compute crypto (no policy needed). Hashing
  `sha256`/`sha512`/`sha1`/`md5(data) → bytes`; HMAC `hmac_sha256`/`hmac_sha512`/
  `hmac_sha1(key, msg) → bytes`; `hex.encode(bytes) → string` / `hex.decode(text)
  → bytes`; `random_bytes(n) → bytes` from the OS CSPRNG; and `constant_eq(a, b)
  → bool` for timing-safe comparison. Digests are raw bytes — bridge to hex or
  `lur.base64` as the destination format needs. `sha1`/`md5` are for legacy
  interop only.
- **`lur.cookie`** — pure-compute cookie helpers (no policy needed).
  `parse(header) → { name = value, … }` reads a `Cookie` request header
  (lenient: malformed segments are skipped; on a duplicate name the later value
  wins; values are verbatim — no decoding). `serialize(name, value, opts?) →
  string` builds one `Set-Cookie` value (no `Set-Cookie:` prefix); `opts` may
  set `domain`/`path`/`expires` (string), `max_age` (integer seconds),
  `secure`/`http_only` (boolean), and `same_site` (`"Strict"`/`"Lax"`/`"None"`).
  Values are raw bytes (base64 them for arbitrary data); an invalid name, a
  value with `;`/CR/LF, or `same_site="None"` without `secure=true` raises.
  Produce `expires` with `os.date("!%a, %d %b %Y %H:%M:%S GMT", t)`.
- **`lur.time`** — pure-compute clocks and timestamp parsing (no policy needed),
  filling the gaps `os.*` cannot. `now_ms() → ms` is the current Unix time in
  milliseconds; `monotonic_ms() → ms` is a monotonic reading whose *difference*
  measures elapsed time immune to clock adjustments. `parse_rfc3339(text) → ms`
  and `parse_http_date(text) → ms` turn an RFC 3339 timestamp (a UTC offset such
  as a trailing `Z` is required) or an HTTP-date header into epoch milliseconds
  (malformed input raises). All values
  are integer milliseconds; divide by `1000` to feed `os.date` (which still
  handles formatting).
- **`lur.log`** — `info(msg)`, `warn(msg)`, `error(msg)`, written to stderr (stdout is
  reserved as the data channel). No implicit newline.
- **`lur.stdin`** — `read()` drains all bytes, `read(n)` reads up to `n` (`nil` at EOF),
  `lines()` iterates newline-stripped lines.
- **`lur.stdout`** — `write(bytes)`, `flush()`. Raw bytes, no implicit newline.
- **`lur.args`** — parsed argv: `lur.args.positional` (1-indexed array) and
  `lur.args.flags` (`--name value`/`--name=value` → `"value"`, bare `--flag` → `true`).

### Capabilities (policy-gated)

- **`lur.fs`** — `read(path) → bytes`, `write(path, bytes)`. Paths are canonicalized
  before the allowlist check, so `..` and symlink escapes are rejected.
- **`lur.http`** — `request(method, url, opts?)` plus `get`/`post`/`put`/`patch`/
  `delete`/`head(url, opts?)`. `opts` may set `headers`, `query`, `body` **or** `json`,
  and `timeout` (ms). The response is `{ status, body, headers, headers_all, json() }`.
  Every request and redirect hop is checked against the network allowlist and the
  private-IP guard (SSRF); TLS is always verified; the body is capped by `--max-http-body`.
- **`lur.env`** — `lur.env(name) → string | nil`. Returns `nil` for both "denied" and
  "unset" so it can't be used as an oracle.

### Storage (requires `--db`)

`--db` accepts either a SQLite file path or a `postgres://` / `postgresql://` connection
string; the scheme picks the backend at first use. Each engine is spoken natively — `lur`
adds no SQL-portability layer, so placeholders and types follow the backend you chose.

- **`lur.db`** — `exec(sql, ...params) → { rows_affected, last_insert_id }`,
  `query(sql, ...params) → array of row tables` (keyed by column name), and
  `tx(fn)` which runs `fn(tx)` on a pinned connection, committing on return and rolling
  back on error. SQLite write transactions use `BEGIN IMMEDIATE`; write-lock contention is
  handled by a 200 ms `busy_timeout` plus bounded retry-with-jitter (up to 5 attempts) on
  single-statement writes and lock acquisition, so concurrent writers wait successfully
  instead of raising a spurious "database is locked". Use native placeholders per
  backend — `?` on SQLite, `$1, $2, …` on Postgres (no translation between the two);
  tables must be JSON-encoded first.
- **`lur.kv`** — `get(key) → bytes | nil`, `set(key, bytes)`, `delete(key)` plus atomic
  ops: `add(key, value)` (set-if-absent; returns bool), `cas(key, expected, new)` (compare-
  and-set; `nil` expected = must-be-absent, `nil` new = delete; returns bool),
  `incr(key, n?)` / `decr(key, n?)` (integer counters; default step 1; counters read back
  via `get` as their decimal string), and `update(key, fn)` (read-modify-write; return `nil`
  from `fn` to delete). All backed by the shared pool for whichever backend `--db` selects.
- **Postgres row types** — only core scalar types map to Lua: integer (`int2`/`int4`/
  `int8`), number (`float4`/`float8`), string (`text`/`varchar`/`bytea`, …). A non-core
  column (e.g. `numeric`, `timestamptz`, `jsonb`, `uuid`, `bool`, arrays) raises
  `lur.db: unsupported column type '<T>' in column '<name>'; CAST it to text (e.g.
  <name>::text)` — cast it in the query rather than have `lur` guess a representation.
- **`last_insert_id` is SQLite-only** — Postgres has no `last_insert_rowid()`, so
  `db.exec(...).last_insert_id` is always `0` there; use
  `db.query("INSERT INTO t (...) VALUES (...) RETURNING id")` to get generated keys back.
- **TLS** — append `?sslmode=require` (or another `sslmode` value) to the Postgres
  connection string; it passes straight through to the driver.
- **`db.tx` / `kv.update` are fallible** — on Postgres both run at `SERIALIZABLE` and may
  raise a transient `40001` serialization conflict; on SQLite they may raise after
  exhausting the busy retry. Either way `lur` does **not** auto-retry them — wrap the call
  in `pcall` (or your own retry loop):

  ```lua
  local ok, err = pcall(function()
    return lur.db.tx(function(tx) --[[ … ]] end)
  end)
  ```

Running the Postgres integration tests locally needs a local Postgres:
`docker compose up -d`.

### Concurrency

- **`lur.async`** — `sleep(ms)`, and combinators over arrays of zero-arg functions:
  `all` (await all, fail-fast), `race` (first to settle), `any` (first to succeed),
  `settled` (await all, never raise → `{ ok, value | err }`). Lua still runs one step at
  a time; tasks interleave only at I/O await points. `--max-concurrency` caps in-flight
  tasks.
- **`lur.state`** — process-wide shared state across the VM pool, primitives only:
  `get(key)`, `set(key, value)` (`nil` deletes), `incr(key, n?)` / `decr(key, n?)`
  (atomic integer add/subtract; step defaults to 1; fractional or non-integer steps are
  rejected), `add(key, value)` (set-if-absent; returns bool), `cas(key, expected, new)`
  (value compare-and-set; `nil` means absent; returns bool), and `update(key, fn)`
  (optimistic CAS retry loop; `fn` runs with no lock held).

### Server mode (`lur serve`)

Registration happens once at load time; the registered handlers then serve traffic.

- **`lur.serve.http(method, path, handler)`** — `method` is `"GET"`…/`"ANY"`. Paths may
  contain `:name` segments (e.g. `/users/:id`) that bind into `req.params`; a more
  specific route (more static segments, then a concrete method over `ANY`) wins
  regardless of registration order. The `handler(req)` returns `{ status?, body? }`
  (`status` defaults to `200`, `body` to empty).
- **`lur.serve.cron(spec, handler, opts?)`** — `spec` is a 6-field cron expression
  (`sec min hour dom mon dow`). `opts` may set `name`, `overlap` (default `false` =
  single-flight, skip a tick if the previous run is still going), and `timeout` (ms).

The `req` object exposes `method`, `path`, `params`, `query` (last value per key),
`query_all` (all values), `headers` (lowercased), `cookies` (parsed `Cookie`
header; empty table when absent), `body` (raw bytes), and `json()`.
For large uploads, `read(n)` streams the body in chunks; once you start streaming,
`body`/`json()` are no longer available.

```lua
lur.serve.http("POST", "/echo", function(req)
  local data = req.json()
  return { status = 200, body = lur.json.encode(data) }
end)
```

`lur serve` drains in-flight requests and cron runs on `SIGTERM`/`SIGINT` within
`--shutdown-grace` before exiting.

### Diagnostics

Errors are reported against your script's path with the failing line and a
source snippet (rustc-style), followed by a stack traceback. Server handler and
cron errors are rendered the same way to stderr (and still become a `500`).
Capability functions report argument-type mistakes in their own voice, e.g.
`lur.crypto.sha256: argument #1 must be string, got table`. Type coercion is
unchanged — only the error message is clearer.

Output is colorized when stderr is a terminal, and plain when it is piped or
redirected. Set [`NO_COLOR`](https://no-color.org) (to any non-empty value) to
disable color even on a terminal.

## Development

```sh
cargo nextest run        # tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo bench --bench runtime
```

CI runs lint, tests, coverage (Codecov), and a benchmark report on every push and PR.

For how the runtime is put together internally, see [ARCHITECTURE.md](ARCHITECTURE.md).

## License

Licensed under the [MIT License](LICENSE.txt).
