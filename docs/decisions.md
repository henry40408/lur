# Design decisions

Rationale, rejected alternatives, and known limitations that aren't obvious from the code.
For *what* the code does, see [ARCHITECTURE.md](../ARCHITECTURE.md).

## Runtime & sandbox

- **Luau, not PUC Lua 5.4.** Untrusted scripts are in the threat model, and Luau's
  `sandbox(true)` (readonly globals, dangerous stdlib removed) is hardened at Roblox scale;
  a hand-rolled 5.4 sandbox would make every bug a lur CVE. Costs: Lua 5.1 dialect (no
  integer subtype, `goto`, `<close>`), smaller ecosystem. Revisit only if scripts become
  semi-trusted-only *and* 5.4 semantics become a hard requirement.
- **Only `require` is a real escape** among the four stripped globals (it loads `.luau`
  off disk). `loadstring`/`getfenv`/`setfenv` can't reach `io`/`os`; they're stripped for
  least surface and to stop global bleed on pooled VMs. A policy-gated `require` via
  mlua's `create_require_function` is the natural future replacement for dropping it.
- **The deadline interrupt must keep raising.** Its error is an ordinary catchable Lua
  error; a fire-once abort would be swallowed by `while true do pcall(...) end`.
- **Upvalues persist across requests on a pooled VM** and can't be prevented (inherent to
  closures). `fresh_env` only isolates globals. Rule: never stash request data in an
  upvalue; use `lur.state`/`lur.kv`.
- **`app.lua` top level runs once per pooled VM**, so it must be idempotent
  (`CREATE TABLE IF NOT EXISTS`, etc.).
- **Numbers are f64.** Integers above 2^53 lose precision; store large IDs as text.

## Capabilities

- **Pure-compute capabilities** (`json`, `base64`, `crypto`, `cookie`, `time`) are not
  policy-gated and take/return raw bytes; callers bridge with `hex`/`base64`.
- **`lur.crypto`**: `hmac_md5` and `random_hex` omitted on purpose (extinct /
  composable). `constant_eq` returns early on length mismatch (length isn't secret).
- **`lur.cookie`**: no percent-encoding (a value containing `%` would be ambiguous).
  `SameSite=None` without `secure` raises rather than silently adding `Secure`.
- **`lur.time`**: integer milliseconds everywhere; no formatting API since
  `os.date("!…")` already covers RFC 3339 / IMF-fixdate.
- **Server responses**: no `Content-Type` inference ("explicit over magic"); an invalid
  header fails the whole response (500) instead of being dropped.
- **`lur.http` SSRF guard**:
  - The private-address list is curated (loopback, private, link-local, unique-local,
    CGNAT, cloud-metadata ranges, multicast, …), not the full IANA special-purpose
    registry. IPv6 forms embedding an IPv4 address (mapped, compatible, NAT64, 6to4) are
    judged by the embedded address; Teredo `2001::/32` is not unpacked.
  - IP literals skip `SsrfResolver` (the connector dials them directly), so `url_allowed`
    is their only private-address gate, for the first request and every redirect hop.
    Hostnames are filtered by the resolver, which also pins the connection to the vetted IP.
  - System proxy env vars (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`) are ignored on
    purpose: through a proxy the name is resolved remotely and bypasses the resolver.
  - Allowlist rules match IPs by address, strictly: an IPv4 rule does not match its
    IPv4-mapped IPv6 spelling, nor the reverse.
- **Diagnostics**: chunk name is the CLI path as typed (not canonicalized or
  basenamed). Exit codes deliberately not split per error kind.
- **`lur docs`**: hand-rolled ANSI renderer over `pulldown-cmark` (no default features);
  `termimad` rejected for its dependency weight. 16-color SGR only.

## Storage

- **Enum dispatch, not `dyn` trait.** Exactly two backends, one per deployment, no plugin
  model; no runtime backend switching. Promoting the enum to a trait later is mechanical.
- **kv value model is defined at the seam** (opaque bytes vs integer counter). Don't
  harmonize by changing the SQLite schema; a future SQLite→Postgres migration should read
  through one `Backend` and write through the other, not copy tables.
- **No SQL translation.** Each backend uses its native dialect and placeholders (`?` is a
  jsonb operator in Postgres, so translating would be ambiguous).
- **Postgres non-core column types raise** instead of being stringified: sqlx returns the
  binary wire format with no generic to-text, and text forms like `timestamptz` depend on
  server `TimeZone`/`DateStyle`. Users cast (`col::text`). Lua strings bind as `text` if
  valid UTF-8, else `bytea`; mismatched target columns need an explicit `$1::bytea` cast.
- **Postgres `db.tx`/`kv.update` use `SERIALIZABLE`** because the database may be shared
  with non-lur writers; an advisory lock only serializes cooperating writers. Hence they're
  fallible and never auto-retried (the body may have side effects).
- **TLS via rustls**, not native-tls: no OpenSSL system dependency.
- **SQLite retry** wraps only lock acquisition and single statements; re-running a
  transaction body was rejected (duplicated side effects).
- **Cancellation cleanup is a rollback-on-drop guard.** Rejected: sqlx's `.begin()` (issues
  a deferred `BEGIN`, losing `BEGIN IMMEDIATE`; Postgres would need `SET TRANSACTION`) and
  Postgres server-side timeouts (Postgres-only, and would kill legitimately slow
  transforms). `db.tx` closures hold `Weak` refs so cancellation drops the transaction
  immediately instead of waiting for Luau GC.
- **`db.tx` takes the write lock on SQLite even when read-only** (`BEGIN IMMEDIATE`) —
  deliberate.

## Known limitations / deferred

- Sandbox: no OS-level hardening (landlock/seccomp); `lur.fs` has a canonicalize-then-open
  TOCTOU window (needs `openat2`/`O_NOFOLLOW`).
- Allowlists: no subdomain wildcards, CIDR ranges, path globs, or env-name prefixes.
- A `lur.db` write inside a `kv.update` transform blocks on the lock; on Postgres it hangs
  unless `--timeout` is set.
- Cron: UTC only (no timezone setting), in-memory schedule, no missed-run replay, no
  `@every` interval syntax.
- Not implemented: streaming response bodies / `lur.http` downloads, form bodies, retries,
  cookie jar, proxies, a TLS-verification opt-out, managed migrations, kv/state TTL,
  in-memory DB, named SQL params, socket/queue trigger sources, `lur.serve.on_start`,
  configurable SQLite `busy_timeout`, machine-readable (`--error-format=json`) diagnostics,
  symmetric/asymmetric crypto and KDFs, signed cookies, `lur docs <section>`.
- `chrono` → `jiff` migration is blocked on replacing the `cron` crate.
