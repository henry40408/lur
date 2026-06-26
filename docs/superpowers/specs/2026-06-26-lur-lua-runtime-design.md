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
Shared core: Lua VM (mlua, safe mode) · host modules (lur.*) · policy/sandbox
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
| `state` | Short-term (in-memory) + long-term (storage backend) state | `dashmap`, `sqlx`/`rusqlite` |
| `serve` | Server mode: `Source` trait (http/cron/...) driving the event loop + VM pool | `axum`/`hyper`, cron lib |
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
3. Run `app.lua` once to collect handler registrations (`lur.serve.http(...)`, etc.).
4. Start each `Source` (HTTP listener, cron scheduler, ...) and enter the event loop.
5. On an incoming event → borrow a VM from the pool → run the matching handler →
   return the VM to the pool.
6. All cross-request / cross-event state lives host-side (see §6); the VM itself is stateless.

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

-- capability modules (policy-gated)
lur.http.get(url, opts)   -- async; returns { status, body, headers }
lur.http.post(url, opts)
lur.fs.read(path)         -- path allowlist enforced; returns raw bytes
lur.fs.write(path, data)  -- writes raw bytes as-is
lur.log.info/warn/error() -- writes to stderr (stdout is the script's data channel)

-- pure functions (always safe)
lur.json.encode/decode()
lur.base64.encode/decode()

-- state (see §6)
lur.state.get/set/incr/update()   -- short-term, host memory, shared across requests, atomic
lur.db.query/exec()               -- long-term, raw SQL
lur.kv.get/set/delete()           -- long-term, persistent KV

-- server registration (server mode only)
lur.serve.http(path, handler)     -- handler(req) -> { status, body, headers }
lur.serve.cron(spec, handler)     -- handler()
-- lur.serve.socket(...) etc. for future extension

-- concurrency
lur.spawn_all({ fn1, fn2, ... })  -- run multiple Lua functions concurrently, collect results
```

### Removed / restricted native libraries

- **strict**: remove `os.execute`, `io`, `package`, `require`, `loadfile`, `dofile`,
  `load`, `os.getenv`, `os.remove`, `os.rename`, etc. Keep pure libs `string`/`table`/
  `math`/`utf8`/`print` (`print` goes to stdout).
- **loose**: optionally restore parts of `os`/`io` (still recommend the managed `lur.*` versions).

Key point: **removing `io`/`os` does not remove I/O capability** — it removes
"unmanaged ambient access" and re-provides the same operations through policy-managed
`lur.fs` / `lur.stdin` / `lur.http`. stdin/stdout are fixed pipes the host opens and
hands to the script; the script cannot choose "which fd / which file to open", so they
remain safe to provide even under strict.

### Byte semantics & Unicode

The runtime is **byte-transparent by default; Unicode is opt-in**.

- A **Lua string is an arbitrary byte sequence with no encoding**. mlua maps strings
  to/from Rust as bytes in both directions and never transcodes implicitly.
- All data-carrying APIs are **binary-safe / encoding-agnostic**: `lur.stdin.read[(n)]`,
  `lur.fs.read`, and `lur.http` response bodies return **raw bytes**; `lur.stdout.write`
  and `lur.fs.write` write the bytes as-is. No UTF-8 validation is performed on these paths.
- For **text / codepoint handling**, scripts use Lua 5.4's built-in `utf8` library
  (kept as a pure, safe native): `utf8.len`, `utf8.char`, `utf8.codepoint`, `utf8.codes`.
- **The only place UTF-8 is assumed is the JSON boundary.** JSON is defined as UTF-8, so
  `lur.json.encode` requires strings to be valid UTF-8 and errors on invalid bytes. To put
  binary data into JSON, the script must `lur.base64.encode` it first.

In one sentence: `lur` passes strings around as bytes everywhere and only assumes UTF-8 at
`lur.json.encode` and within the `utf8.*` library.

## 5. Sandbox (Approach A: capability injection, in-process)

Both modes share the same enforcement:

1. **Clean environment**: do not pollute `_G`; give the script a custom global table
   (only `lur` + allowlisted pure libs) and run the chunk under mlua's environment mechanism.
2. **Safe mode**: mlua does not load unsafe C functions / FFI.
3. **Memory cap**: `Lua::set_memory_limit`.
4. **Timeout / interrupt**: VM instruction-count hook; on timeout or a cancellation
   signal, raise an error and abort.
5. **Capability gate**: every host-function entry point checks `Policy` (host/path not
   in the allowlist → raise a Lua error with a clear message).
6. **Layer-B reserved**: `Policy` has an `os_hardening: Option<OsHardening>` field for a
   future landlock/seccomp/sandbox-exec layer; not implemented in v1.

### Policy structure (conceptual)

```
Policy {
  profile: Loose | Strict,
  fs_allow: Vec<PathRule>,          // readable/writable paths
  net_allow: Vec<HostRule>,         // allowed network hosts
  env_allow: Vec<String>,           // allowed environment-variable names
  limits: { timeout, memory, max_concurrency, per_event_timeout },
  os_hardening: Option<OsHardening> // reserved, None in v1
}
```

The profile is selected and overridden by flags or a config file. `loose` defaults to
permissive; `strict` defaults to tight (allowlists empty by default).

**The shipped default profile is `strict` — secure by default, like Deno.** A bare
`lur script.lua` has zero ambient access; capabilities are granted explicitly via
`--allow-net`/`--allow-fs`/`--allow-env`, or all at once via `-A`/`--allow-all`. Two
low-friction escape hatches keep personal use ergonomic without weakening the shipped
default: (1) `-A` for a one-off full-access run of a trusted script; (2) a user config
(`~/.config/lur/config`) may set `default_profile = loose` so an individual's own machine
defaults permissive, while the binary remains safe-by-default for everyone else. See §12
for the concrete CLI surface.

## 6. State Layer

Under a VM pool, which VM handles which event is non-deterministic, so **all
cross-request state must live host-side** and be exposed via `lur.*` so every VM in the
pool shares it. The Lua state inside each VM is treated as stateless.

### Short-term state `lur.state`
- A host-side concurrent KV (`DashMap` or `Mutex<HashMap>`), **process-scoped**, shared
  across requests, gone when the process exits.
- Because multiple VMs access it concurrently, it must offer **atomic** operations, not
  just get/set:
  - `lur.state.incr(key)` → returns the incremented value (the counter API relies on this)
  - `lur.state.update(key, fn)` → atomic read-modify-write
  - `lur.state.get/set(key[, val])`

### Long-term state `lur.db` + `lur.kv`
- A `StorageBackend` trait; v1 implements **SQLite** (connection pool), interface
  reserved for **Postgres**.
- `lur.db.query(sql, ...)` / `lur.db.exec(sql, ...)`: raw SQL + parameter binding.
- `lur.kv.get/set/delete`: a persistent key-value abstraction backed by a single table.
- Both coexist: use `lur.db` when you want SQL, `lur.kv` when you just need simple persistence.

## 7. Concurrency Model

- `tokio` underneath; `lur.http.*` is async (mlua async + tokio).
- `lur.spawn_all({...})` wraps multiple Lua functions into tokio tasks, runs them
  concurrently, and collects results, letting a script fire many requests then await them together.
- Concurrency limit controlled by `policy.limits.max_concurrency`.
- Server mode: VM-pool size aligns with `max_concurrency`; event parallelism is driven by tokio.

## 8. Error Handling & Exit Codes

| Situation | Behavior |
|---|---|
| Script error | `pcall` catches it → print traceback to stderr → exit 1 |
| Policy violation | Raise a Lua error with a clear message (host/path/env blocked) |
| Timeout | exit 124 (aligned with GNU `timeout`) |
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
  `incr`/`update` atomicity (no lost updates).
- **storage**: SQLite round-trip (`lur.db` and `lur.kv`).
- **performance**: a `criterion` benchmark suite gates regressions on the perf-sensitive paths
  (VM cold start, host-call boundary, sandbox-hook overhead, state contention, storage, server
  throughput). See §13 for the full plan, budgets, and CI gate.

## 10. Key Dependencies (candidates; verify versions per the 7-day cooldown rule in CLAUDE.md before release)

- `mlua` (Lua 5.4, features: async, send, safe)
- `tokio` (async runtime)
- `clap` (CLI parsing)
- `reqwest` (HTTP client for `lur.http`)
- `serde_json` (JSON encode/decode, output serialization)
- `dashmap` (short-term state concurrent KV)
- `rusqlite` or `sqlx` (SQLite backend; server mode prefers async `sqlx`)
- Server-mode HTTP: `axum` / `hyper`
- Cron scheduling: TBD (e.g. `tokio-cron-scheduler`)
- `criterion` (dev-dependency: micro-benchmarks; see §13)

## 11. Suggested Implementation Order

1. Shared core: `policy` + `runtime` (single VM, injection mechanism, sandbox enforcement).
2. one-shot mode + basic host modules (`args`/`stdin`/`json`/`log`/`fs`/`http`).
3. State layer: `lur.state` (short-term) → `lur.db`/`lur.kv` (SQLite).
4. Server mode: VM pool + `lur.serve.http` + event loop.
5. Server extension: `lur.serve.cron`.
6. Concurrency helper `lur.spawn_all`, profile config file, docs and examples.

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
| `--allow-net <host>` | Add a network host to the allowlist (repeatable) |
| `--allow-fs <path>` | Add a filesystem path to the allowlist (repeatable) |
| `--allow-env <name>` | Add an environment variable to the allowlist (repeatable) |
| `--timeout <dur>` | Execution timeout (e.g. `5s`, `2m`) |
| `--memory <size>` | Memory cap (e.g. `128m`) |
| `--max-concurrency <n>` | Concurrency limit |
| `--bind <addr>` | (server) listen address |
| `--db <path>` | SQLite database path |

### Defaults & precedence

- **Default profile is `strict`** (secure by default; see §5). A bare run has zero access.
- Permission resolution order (later overrides earlier): shipped default (`strict`) →
  user config (`~/.config/lur/config`, may set `default_profile` and standing allowlists)
  → command-line flags (`--allow-*`, `-A`, `--strict`/`--loose`).
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
| Sandbox overhead | a loop with vs. without the instruction hook + memory limit |
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
