# `lur.time` — clocks and timestamp parsing

Status: design approved, ready for implementation plan.
Date: 2026-06-29.

## Motivation

Luau's sandboxed `os` table already exposes `os.time()` (whole-second Unix
time), `os.date()` (formatting, including UTC IMF-fixdate and ISO-8601), and
`os.clock()` (process CPU time). Two things it cannot do, and that real services
routinely need, are missing:

1. **Sub-second and monotonic timing.** `os.time()` is whole seconds only, and
   `os.clock()` measures CPU time, not elapsed wall-clock time. Logging
   timestamps, measuring handler latency, rate-limiting, and token TTLs all want
   millisecond resolution and a clock that does not jump when the wall clock is
   adjusted (NTP, DST).
2. **Parsing timestamps.** `os.date` only *formats*; it cannot turn an inbound
   RFC 3339 timestamp (from a JSON API) or an HTTP-date header
   (`Last-Modified`, `Date`, cookie `Expires`) back into a number.

`lur.time` fills exactly these gaps as a pure-compute capability. It does **not**
re-wrap formatting — `os.date("!…")` already produces RFC 3339 and IMF-fixdate
strings, so duplicating that would violate "explicit over magic" and add no
value.

## Positioning

- **Pure-compute capability, not policy-gated.** Time and clocks are not a
  sandbox-arbitrated resource (`os.time`/`os.date` are already exposed). Like
  `base64`/`crypto`/`cookie`, it is available in both `strict` and `loose`
  profiles with no grant.
- **Errors raise.** Failures raise a Lua error `lur.time.<fn>: <detail>`
  (catchable with `pcall`), matching the existing capability convention.
- **Install order.** Installed in `capabilities::install` immediately after
  `cookie` (the pure-compute cluster: null · log · json · base64 · crypto ·
  cookie · time · …). The capability-order line in ARCHITECTURE.md gains `time`.
- **Unit: milliseconds, `i64`.** Every value `lur.time` produces or accepts is
  integer milliseconds. This is the point of the capability (sub-second
  precision), avoids float-precision concerns, matches the conventional "epoch
  millis" unit, and keeps TTL/rate-limit arithmetic in integers. Interop with
  `os.date` (which takes seconds) is an explicit `// 1000`, documented.

## API surface

```
lur.time.now_ms()           → i64   -- current Unix time in milliseconds
lur.time.monotonic_ms()     → i64   -- ms since a fixed process reference; diffs only
lur.time.parse_rfc3339(s)   → i64   -- RFC 3339 / ISO 8601 → epoch milliseconds
lur.time.parse_http_date(s) → i64   -- HTTP-date → epoch milliseconds
```

### `now_ms() → i64`

`SystemTime::now()` minus `UNIX_EPOCH`, as integer milliseconds. The (practically
impossible) case of a system clock set before the Unix epoch raises
`lur.time.now_ms: system clock is before the unix epoch`.

### `monotonic_ms() → i64`

Milliseconds elapsed since a process-fixed reference `Instant`, captured once on
first use (a `std::sync::LazyLock<Instant>`). The **absolute value is
meaningless** — only the difference between two readings is, giving elapsed time
that is immune to wall-clock adjustments. Documented as such.

### `parse_rfc3339(s) → i64`

Parses an RFC 3339 / ISO 8601 timestamp (e.g. `2026-06-29T12:00:00Z`,
`2026-06-29T12:00:00.500+02:00`) into epoch milliseconds, using chrono's
`DateTime::parse_from_rfc3339`. Sub-second input is preserved to millisecond
resolution. A malformed string raises `lur.time.parse_rfc3339: <detail>`.

### `parse_http_date(s) → i64`

Parses an HTTP-date (`Thu, 29 Jun 2026 12:00:00 GMT`) into epoch milliseconds,
using the `httpdate` crate, which accepts all three formats HTTP recipients must
handle (IMF-fixdate, obsolete RFC 850, and asctime). The crate yields a
`SystemTime`; the result is its millisecond offset from `UNIX_EPOCH`. HTTP-date
has second resolution, so the milliseconds are always `…000`. A malformed string
raises `lur.time.parse_http_date: <detail>`.

## Dependencies

- **chrono** — already a direct dependency (`0.4.45`, used by `lur.serve.cron`);
  `parse_rfc3339` reuses it. No new crate.
- **httpdate** — promoted from a transitive dependency (already in the lockfile
  at `1.0.3`, pulled in via hyper) to a direct dependency, pinned to the
  same `1.0.3`. This adds **no** new code to the dependency tree — it only makes
  an already-present, mature, years-old crate directly usable. Satisfies the
  7-day cooldown (1.0.3 is long-published) and `cargo deny`.

## Worked examples

```lua
-- measure handler latency, immune to clock adjustments
local t0 = lur.time.monotonic_ms()
do_work()
lur.log.info(("took %dms"):format(lur.time.monotonic_ms() - t0))

-- parse an external API timestamp, keep it as a number
local ts = lur.time.parse_rfc3339("2026-06-29T12:00:00Z")   -- epoch ms
-- to display via os.date, convert ms → s:
local human = os.date("!%Y-%m-%d %H:%M:%S", ts // 1000)

-- decide if a cached resource is stale from its Last-Modified header
local modified = lur.time.parse_http_date(req.headers["last-modified"])
if lur.time.now_ms() - modified > 60000 then refresh() end
```

## Testing

Lua-script integration tests in `tests/capabilities.rs` via the existing
`run(src)` helper (matching the `crypto`/`cookie` convention):

- `now_ms`: returns a value greater than a known recent epoch-ms floor (e.g.
  `> 1.7e12`); two successive calls are non-decreasing.
- `monotonic_ms`: two successive calls are non-decreasing; after
  `lur.async.sleep(5)` the later reading is strictly greater than the earlier.
- `parse_rfc3339`: `"1970-01-01T00:00:00Z"` → `0`; `"1970-01-01T00:00:00.500Z"`
  → `500`; an offset form parses to the correct UTC instant; a malformed string
  raises.
- `parse_http_date`: `"Thu, 01 Jan 1970 00:00:00 GMT"` → `0`; a known later
  date parses correctly; a malformed string raises.

## Documentation

- README.md "Data & I/O" gains a `lur.time` entry: the four functions, the
  millisecond unit, and the `// 1000` interop note for `os.date`.
- ARCHITECTURE.md capability-order diagram and list include `time`.

## Out of scope (possible follow-ups)

- Timestamp **formatting** (`os.date("!…")` already covers RFC 3339 and
  IMF-fixdate).
- Time zones other than UTC / civil-time arithmetic.
- Duration arithmetic helpers (plain integer-millis math suffices).
- Sleeping (`lur.async.sleep(ms)` already exists).
- Migrating the existing `chrono` dependency to `jiff` — a separate project
  gated on replacing the `cron` crate, unrelated to this capability.
