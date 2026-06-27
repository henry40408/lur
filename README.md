# lur

A small, sandboxed Lua runtime written in Rust. Write your automation and services
in [Luau](https://luau.org/); `lur` runs them safely behind a capability sandbox.

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

## Quick start

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
lur serve app.lua --bind 127.0.0.1:8080
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
| `--db` | PATH | | SQLite file backing `lur.db` / `lur.kv`. |
| `--config` | FILE | | Load a specific config file. Conflicts with `--no-config`. |
| `--no-config` | — | | Ignore all config — pure shipped strict, zero grants. |

### One-shot only

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--timeout` | DURATION | none | Wall-clock limit; the run exits non-zero on timeout. |

### Server only

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--bind` | ADDR | `127.0.0.1:8080` | Listener address. |
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

### Storage (requires `--db <path>`)

- **`lur.db`** — `exec(sql, ...params) → { rows_affected, last_insert_id }`,
  `query(sql, ...params) → array of row tables` (keyed by column name), and
  `tx(fn)` which runs `fn(tx)` on a pinned connection, committing on return and rolling
  back on error. Use `?` placeholders; tables must be JSON-encoded first.
- **`lur.kv`** — `get(key) → bytes | nil`, `set(key, bytes)`, `delete(key)`. A simple
  key/value store sharing the same SQLite pool.

### Concurrency

- **`lur.async`** — `sleep(ms)`, and combinators over arrays of zero-arg functions:
  `all` (await all, fail-fast), `race` (first to settle), `any` (first to succeed),
  `settled` (await all, never raise → `{ ok, value | err }`). Lua still runs one step at
  a time; tasks interleave only at I/O await points. `--max-concurrency` caps in-flight
  tasks.
- **`lur.state`** — process-wide shared state across the VM pool, primitives only:
  `get(key)`, `set(key, value)` (`nil` deletes), `incr(key, n?)` (atomic add), and
  `update(key, fn)` (optimistic CAS retry loop; `fn` runs with no lock held).

### Server mode (`lur serve`)

Registration happens once at load time; the registered handlers then serve traffic.

- **`lur.serve.http(method, path, handler)`** — `method` is `"GET"`…/`"ANY"`; routing is
  exact-match in v1. The `handler(req)` returns `{ status?, body?, headers? }`
  (`status` defaults to `200`, `body` to empty).
- **`lur.serve.cron(spec, handler, opts?)`** — `spec` is a 6-field cron expression
  (`sec min hour dom mon dow`). `opts` may set `name`, `overlap` (default `false` =
  single-flight, skip a tick if the previous run is still going), and `timeout` (ms).

The `req` object exposes `method`, `path`, `params`, `query` (last value per key),
`query_all` (all values), `headers` (lowercased), `body` (raw bytes), and `json()`.
For large uploads, `read(n)` streams the body in chunks; once you start streaming,
`body`/`json()` are no longer available.

```lua
lur.serve.http("POST", "/echo", function(req)
  local data = req.json()
  return { status = 200, headers = { ["content-type"] = "application/json" },
           body = lur.json.encode(data) }
end)
```

`lur serve` drains in-flight requests and cron runs on `SIGTERM`/`SIGINT` within
`--shutdown-grace` before exiting.

## Development

```sh
cargo nextest run        # tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo bench --bench runtime
```

CI runs lint, tests, coverage (Codecov), and a benchmark report on every push and PR.

## License

No license has been declared for this project yet.
