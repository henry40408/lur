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

- **Pure-compute capabilities** (`json`, `base64`, `crypto`, `cookie`, `time`, `url`, `html`, `feed`) are not
  policy-gated and take/return raw bytes; callers bridge with `hex`/`base64`.
- **`lur.crypto`**: `hmac_md5` and `random_hex` omitted on purpose (extinct /
  composable). `constant_eq` returns early on length mismatch (length isn't secret).
- **`lur.html`**: `scraper::Html` (and `dom_query`) are `!Send` because tendril uses
  non-atomic refcounts, while mlua's `send` mode needs `Send` userdata. A node stores the
  source text plus an `ego_tree::NodeId`; parsed trees live in an 8-slot per-thread LRU and
  are re-parsed on a miss. Parsing is deterministic, so ids stay valid; the cache only
  affects speed. Rejected: `unsafe impl Send` (denied by lint, unsound), eager conversion
  to Lua tables (loses `select` on sub-nodes; fragment re-parse drops `<td>`). Known cost:
  holding many live docs on one thread re-parses on access. Invalid UTF-8 input is replaced
  rather than rejected, since it is usually a raw HTTP body.
- **`lur.feed`**: RSS/Atom are written with `quick-xml`'s `Writer` (closures guarantee
  balanced tags; text and attributes are escaped), not hand-built strings; we only strip
  characters XML 1.0 forbids. `rss`/`atom_syndication` were rejected: two typed models to
  map from Lua tables and less control over output. Escaped text instead of CDATA (no
  `]]>` edge case); `quick-xml`'s `Reader` is the intended base for a future `lur.xml`. No feed parsing
  (`lur.xml`), HTML sanitizing, or date formatting API yet — dates are epoch ms and the
  serializers format them. Atom/JSON items without `guid`/`link` raise instead of
  inventing an id.
- **`lur.cookie`**: no percent-encoding (a value containing `%` would be ambiguous).
  `SameSite=None` without `secure` raises rather than silently adding `Secure`.
- **`lur.time`**: integer milliseconds everywhere. Formatting was first left to
  `os.date("!…")`, but that only knows local time and UTC, and feeds need RFC 2822 with
  offsets and scraped sites need `Asia/Taipei`-style zones, so `format*`/`parse` take an
  optional `tz` (IANA via `chrono-tz`, or a fixed offset). `parse` without a format is
  deliberately a short fixed list (no guessing `dd/mm` vs `mm/dd`: ambiguous input needs an
  explicit `fmt`). DST gap raises; overlap picks the earlier instant.
- **`lur.url`**: thin wrapper over the `url` crate (WHATWG, already in the tree via
  `reqwest`). `parse` returns a plain table (no userdata, no setters). `encode_query`
  sorts keys for deterministic output; `decode_query` keeps the last duplicate to match
  `req.query`. No percent-encode helper yet: `join` and `encode_query` cover URL building.
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
- **kv expiry is a per-key absolute `expires_at`, not a TTL the database enforces.** `now`
  comes from Rust so both backends agree and a skewed Postgres clock can't change results;
  every op filters on it (lazy expiry), and deletion is housekeeping (on open, then hourly
  on writes). Postgres/DynamoDB-style "TTL lags, filter on read" is the same shape.
  Unit is `ttl_ms` because Redis `EX` is seconds and a bare `60` would silently mean 60 ms;
  `ttl_ms <= 0` raises rather than meaning "now" or "never".
- **TTL semantics follow Redis where it has an answer.** `set` without `ttl_ms` clears the
  expiry (like `SET`; the alternative, `KEEPTTL`, is opt-in there). `incr` keeps the expiry,
  as Redis `INCR` does. `lur` adds `ttl_ms` to `incr` that applies only if the key has no
  expiry — Redis `EXPIRE NX`, folded into the same statement because the usual
  `INCR`-then-`EXPIRE` pair leaks a counter that never expires if the second call is lost.
  That makes a rate limit a fixed window (opened by the first hit, not extended by later
  ones); `renew_ttl = true` is the idle-timeout variant. Rejected: always-refresh as the
  default (a client that keeps retrying is never released), a separate `window_ms`/`nx`
  knob, and a `kv.hit` rate-limit helper for now (the `incr` form reads better; revisit).
  `cas`/`update` keep the existing expiry — Redis has no equivalent, but clearing it would
  turn a rate-limit window or a session with an absolute lifetime into a permanent key.
  Forgetting `ttl_ms` on `incr` is the sharp edge: the counter never resets until a later
  `incr` supplies one (it is added then, so old counters heal).
- **`kv.ttl` returns `ms, exists`** rather than one three-state value: `nil, false` absent,
  `nil, true` no expiry, `ms, true` expiring. One-value callers get a number or `nil`
  (safe in arithmetic); Redis's `-1`/`-2` would make `ttl < 1000` true for a missing key.
- **`lur.http` `cache` is an in-memory map, not `lur.kv`.** A cache is disposable, so it
  shouldn't demand a database: it works in one-shot and under `serve` with no `--db`. It
  lives in `RuntimeConfig` (an `Arc` shared by the pool, like `lur.state`), not in a VM.
  Costs: lost on restart, not shared between processes, useless across one-shot runs.
  Rejected: kv-backed (needs `--db`, which users asked why a cache requires) and "kv when
  `--db` is set, memory otherwise" (the same script would have different persistence per
  environment). A persistent store can be added later as an explicit opt-in, not an
  environment-dependent fallback.
  `cache` is a table (`{ ttl_ms }`) so the unit is explicit and `vary` has a place. The
  allowlist/SSRF check runs before the lookup so a cached body never outlives the policy that
  allowed it. Requests with `Authorization`/`Cookie`/`Proxy-Authorization` bypass the cache
  unless the header is listed in `vary`, and responses that set a cookie aren't stored: the
  cache is shared by every request, so a credentialed response must not be served to someone
  else.
  **No size cap, on purpose; the script owns capacity.** lur keeps the levers sufficient:
  `ttl_ms` bounds lifetime (expired entries are really freed: on lookup, and by a sweep at
  most once a minute on insert), `--max-http-body` bounds each entry, `cache` is opt-in per
  call so scripts choose what to cache, and `lur.http.cache_clear()` flushes everything and
  reports the count. Rejected for now: a built-in LRU/byte cap (one more knob and eviction
  policy to get wrong; add it if scripts turn out to need it). Memory is therefore bounded by
  distinct keys per TTL window times `--max-http-body`; a script caching unbounded distinct
  URLs with a long `ttl_ms` can grow the process, and the VM `--memory` limit does not cover
  it.
  Not done: serve-stale-on-error, stampede protection (N concurrent misses fetch N times),
  honoring `Cache-Control`/`ETag`, per-key invalidation.
- **Release profile: `strip`, `lto = "fat"`, `codegen-units = 1`, never `panic = "abort"`.**
  Measured on macOS arm64: 19.0 MB default, 15.8 MB with `strip` alone, 14.2 MB with fat
  LTO, 13.4 MB with `codegen-units = 1`; `lto = "thin"` alone made it larger (19.8 MB).
  Compile time barely moved (~45–80 s clean) and the benchmarks stayed level or slightly
  faster. A Lua error raised from a Rust callback (e.g. `lur.json.decode("{bad")`) must
  unwind through the Rust frames to reach `pcall` and the diagnostics renderer; with
  `panic = "abort"` it is `panic in a function that cannot unwind` and exit 134 instead
  (also `-3.7 MB`, which is why it is tempting). `strip`/LTO/`codegen-units` produced
  byte-identical error output and tracebacks. Stripping drops Rust symbols, so a Rust
  panic backtrace is only addresses; Lua tracebacks are unaffected.
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
  cookie jar, proxies, a TLS-verification opt-out, managed migrations, in-memory DB, named SQL params, socket/queue trigger sources, `lur.serve.on_start`,
  configurable SQLite `busy_timeout`, machine-readable (`--error-format=json`) diagnostics,
  symmetric/asymmetric crypto and KDFs, signed cookies, `lur docs <section>`.
- `chrono` → `jiff` migration is blocked on replacing the `cron` crate.
