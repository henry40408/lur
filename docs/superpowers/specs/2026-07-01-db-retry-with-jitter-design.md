# DB write retry-with-jitter — design

Date: 2026-07-01
Status: approved, ready for implementation planning

## Problem

`lur.db` and `lur.kv` single-statement writes and write transactions currently rely
solely on SQLite's `PRAGMA busy_timeout = 5000ms` to survive lock contention. That is
reliable for a *single* waiter, but under 3+ concurrent writers hammering the same file
it thundering-herds: SQLite's built-in busy polling wakes waiters on a fixed cadence, so
they collide repeatedly and can exhaust the 5 s timeout, surfacing `SQLITE_BUSY`
("database is locked") as an error.

This is an *error*, not a lost update — atomicity holds; the write just fails instead of
waiting successfully. It was surfaced by the kv-counter concurrency test (#55): 4 writers
flaked CI, so the shipped test was reduced to 2 writers.

**Why it matters:** server mode runs a pool of VMs that can write the same DB file
concurrently, so heavy write load could raise spurious busy errors.

## Goal

Add bounded application-level retry-with-jitter around the write-lock contention points,
the standard remedy for `busy_timeout`'s unfairness to multiple waiters. Lower
`busy_timeout` so the jitter — not SQLite's fixed-cadence polling — does the herd-breaking.

Non-goals: no configurability (retry policy is hardcoded); read paths (`db.query`) are
untouched; no new third-party dependency.

## Design

### Retry helper (`src/capabilities/db.rs`)

A `pub(crate)` async helper operating at the **sqlx layer** — before errors are converted
to lur-voiced `mlua::Error`, so the typed busy error is still visible:

```rust
async fn retry_busy<T, F, Fut>(op: F) -> sqlx::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = sqlx::Result<T>>,
```

- Loop: call `op().await`. If the returned `sqlx::Error` is busy/locked and attempts
  remain, back off and retry; otherwise return the error unchanged.
- **Busy detection** (`is_busy`): the error's `as_database_error().code()` is `"5"`
  (`SQLITE_BUSY`) or `"6"` (`SQLITE_LOCKED`).
- **Policy (hardcoded):** at most **5 attempts** (1 initial + 4 retries). **Full jitter**
  exponential backoff: after the n-th failure sleep `random(0, min(cap, base·2ⁿ))` with
  `base = 5 ms`, `cap = 200 ms`. Randomness is drawn from the existing `getrandom`
  dependency (no new crate).
- On exhaustion the last error is returned as-is; the caller keeps its existing
  lur-voiced message (message shape unchanged).
- Worst-case latency bound ≈ 5 × (200 ms `busy_timeout`) + accumulated jitter ≈ 1.5 s.

### Application points

Retry is applied only where user code has **not yet run** or the retried body is a **pure
function**, so a retry never duplicates side effects:

- **`db.exec`** — wrap `q.execute(&pool)`. Single statement; whole-statement retry is
  safe (it either committed or did not).
- **`kv.incr` / `decr`** — wrap the UPSERT `fetch_optional` inside `incr_by`. The
  "existing value is not an integer" business logic stays **outside** the retry (a logic
  error must not be retried).
- **`kv.add` / `cas`** — wrap each op's single write statement.
- **`begin_immediate`** — wrap `acquire()` + `BEGIN IMMEDIATE`, acquiring a fresh
  connection per attempt. A busy failure here means the `db.tx` user body has not run yet
  and the `kv.update` transform is a pure function, so retry is safe. This covers both
  transaction paths. Under WAL the lock holder's `COMMIT` cannot be starved by other
  writers, so no retry is needed past this point.

Not wrapped: `db.query` (read-only), and statements *inside* an open transaction
(`db.tx` / `kv.update` bodies, their `COMMIT`).

### `busy_timeout` change

In `open_pool`, lower `busy_timeout` from `5000 ms` to **`200 ms`**. A single waiter still
passes cleanly via `busy_timeout`; multiple waiters are separated by app-level
retry-with-jitter instead of colliding on SQLite's fixed polling cadence.

## Testing

- Bump the #55 kv-counter concurrency test from 2 writers back to **4 writers** — a
  regression guard that would flake without the retry.
- Add a `db.exec` high-concurrency write test: multiple workers writing the same table,
  asserting every write succeeds with no "database is locked" surfacing.
- Unit-test `is_busy` for the busy/locked error-code recognition.
- Jitter backoff is not unit-tested deterministically; the concurrency stress tests are
  the primary guard, with backoff correctness covered by review and boundary checks.

## Documentation

- `README.md` — update the `lur.db` / `lur.kv` sections: replace "wait out lock contention
  via a 5 s busy_timeout" with the new "200 ms busy_timeout + bounded retry-with-jitter"
  behavior.
- `ARCHITECTURE.md` — update the storage invariant note to reflect the retry layer.

## Alternatives considered

- **Retry only single-statement atomic writes, leave transactions untouched.** Simpler but
  leaves `db.tx` / `kv.update` exposed under high concurrency. Rejected.
- **Retry the whole transaction including re-running the user body.** For `db.tx` the body
  may have external side effects (http, log) that re-running would duplicate. Rejected in
  favor of retrying only the lock-acquisition step.
- **Keep `busy_timeout = 5000 ms` and add retries on top.** Worst-case latency becomes very
  long (5 s × attempts) and conflicts with jitter's herd-breaking intent. Rejected.
- **Make the retry policy CLI/config-configurable.** YAGNI; sensible hardcoded defaults
  first, add knobs only if a real need appears.
