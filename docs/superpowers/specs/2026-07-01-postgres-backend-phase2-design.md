# PostgreSQL backend (Phase 2 of PostgreSQL support) — design

Date: 2026-07-01
Status: approved, ready for implementation planning

This is **Phase 2 of two** for PostgreSQL support. Phase 1 (#57, shipped) extracted a
storage-backend seam — `enum Backend { Sqlite(SqliteBackend) }` with a neutral method
surface — as a pure refactor. Phase 2 adds the `Postgres` variant behind that seam. The
whole point of the Phase 1 boundary is that `db.rs` / `kv.rs` call sites do **not** change
here; Phase 2 extends the enum and its match arms and adds one new backend file.

## Background / why

A concrete need arrived: connect `lur` to an operator's **existing** PostgreSQL. The
brainstorm settled the shape (recorded here and in the project backlog memory): one binary
speaks either SQLite or Postgres, selected at startup by the `--db` connection target; both
`lur.db` and `lur.kv` reach full parity; the Postgres connection is operator-trusted.

## Guiding principle: respect the native engine, no surprise translation

`lur.db` does **not** try to make SQL portable across backends. SQL is already
dialect-specific (DDL, types, functions), so the user writes the dialect of the engine they
chose. `lur` adds **no** translation layer on top. This principle drives the placeholder and
type-mapping decisions below.

## Goals

- Add a `Postgres` backend behind the Phase 1 seam with **full parity** for `lur.db` and
  `lur.kv` at the *contract* level.
- Startup backend selection by `--db` connection target; no dynamic switching (unchanged
  from the Phase 1 non-goal).
- Complete documentation of the new behavior — especially the fallible transactional-API
  contract (see below), which is a deliberate, documented change to already-shipped
  `db.tx` / `kv.update` semantics. The project is in active development; the behavior change
  is acceptable and the bar is documentation completeness, not behavior stability.

## Non-goals

- **No SQL portability / placeholder translation.** Native dialect per backend (below).
- **No dynamic SQLite⇄Postgres switching.** One deployment, one backend (Phase 1 non-goal).
- **No data-migration tooling.** The kv logical model stays backend-neutral at the seam
  (Phase 1 commitment) so a future one-way migration is cheap, but nothing is built now.
- **No connection-pool / isolation tuning knobs.** Sensible fixed defaults; add knobs only
  if a real need appears (YAGNI).

## Global Constraints

- **Dependency cooldown (CLAUDE.md):** enabling sqlx's `postgres` + rustls TLS features
  pulls **new transitive crates** (rustls, ring/aws-lc, webpki, etc.). Every newly-added
  crate version MUST be checked to be ≥7 days old at pin time (`cargo info` / crates.io);
  pin an older version if the resolved one is too new.
- **`cargo fmt` + `cargo clippy --all-targets -- -D warnings` + `cargo nextest run` +
  `cargo deny check` all green.** `cargo deny` MUST still pass with the new TLS crates
  (license/advisory/source gate).
- **SQLite path unchanged.** No edit to `storage/sqlite.rs` behavior or to any existing
  SQLite test. The retry layer (`retry_busy`/`is_busy`/`jitter_delay`) stays SQLite-only.
- **`db.rs` / `kv.rs` call sites unchanged.** Phase 2 touches only `storage/mod.rs` (new
  variants + match arms), the new `storage/postgres.rs`, and the config plumbing
  (`main.rs`, `runtime.rs`, `Shared`).

## Backend selection

`--db` stays a single flag. `build_config` (`src/main.rs`) resolves it once, failing fast on
a malformed Postgres URL:

- value begins with `postgres://` or `postgresql://` → **Postgres** (the whole string is the
  connection target);
- otherwise → **SQLite** file path.

`RuntimeConfig.db_path: Option<PathBuf>` becomes `RuntimeConfig.db: Option<StorageTarget>`:

```rust
pub enum StorageTarget {
    Sqlite(PathBuf),
    Postgres(String), // full connection string, incl. any ?sslmode=…
}
```

`Shared::new` takes `Option<StorageTarget>`; `Shared::ensure` matches it to open the right
backend on first use (unchanged lazy-open behavior). The "no database configured" error when
`--db` is absent is unchanged.

## Seam extension (`storage/mod.rs`)

```rust
enum Backend { Sqlite(SqliteBackend), Postgres(PgBackend) }
enum Transaction { Sqlite(SqliteTransaction), Postgres(PgTransaction) }
```

Every dispatch method gains a `Postgres` arm. No method **signatures** change — the whole
neutral surface (`exec`/`query`/`begin`/`kv_get`/`kv_set`/`kv_delete`/`kv_add`/`kv_cas`/
`kv_incr`/`kv_update`, and `Transaction::exec`/`query`/`commit`/`rollback`) is reused as-is.
Because `db.rs`'s `run_tx` already lets a `commit()` error propagate and rolls back on a body
error, a Postgres `40001` surfaced at `COMMIT` flows out to Lua through the existing control
flow with no `db.rs` change.

## `storage/postgres.rs` — `PgBackend` / `PgTransaction`

Owns **all** Postgres specifics, mirroring `sqlite.rs`'s responsibilities: the `sqlx`
`PgPool`, SQL dialect, `$n` binding, row→Lua mapping, isolation, and the kv schema. No retry
layer (see below).

### Connection lifecycle

`PgBackend::open(conn_str)` builds `PgConnectOptions::from_str(conn_str)` (which parses
`sslmode` from the URL), opens a `PgPool` with sqlx's **default** pool size, then runs the kv
DDL. Postgres has **no** `create_if_missing` — the database is operator-provided; `lur` only
ensures its own table:

```sql
CREATE TABLE IF NOT EXISTS lur_kv (
  key   TEXT PRIMARY KEY,
  kind  SMALLINT NOT NULL,   -- 0 = opaque bytes, 1 = integer counter
  bytes BYTEA,               -- non-null when kind = 0
  num   BIGINT               -- non-null when kind = 1
);
```

No WAL / `busy_timeout` (SQLite concepts).

### TLS

Enable a **rustls-family** sqlx TLS feature (not native-tls): pure Rust, no OpenSSL system
dependency, single-binary story, aligned with the project's supply-chain posture. `sslmode`
is carried in the connection string (e.g. `…/db?sslmode=require`) and honored by
`PgConnectOptions`. The exact feature name (`tls-rustls-ring` vs `tls-rustls-aws-lc-rs` in
sqlx 0.9) and the root-certificate store are pinned during implementation, subject to the
cooldown constraint above.

### Placeholders — native, no translation

Postgres uses `$1, $2, …`; SQLite uses `?`. `lur` translates **neither**. A script targeting
Postgres writes `$n`; one targeting SQLite writes `?`. This follows the guiding principle:
the user already knows their engine's syntax, and a translation layer would both introduce
bugs and be ambiguous (`?` is a jsonb operator in Postgres).

### Row → Lua type mapping

Core types map directly; everything else falls back to Postgres's own text rendering; a
supported column **never** raises:

| Postgres type | Lua |
|---|---|
| `int2` / `int4` / `int8` | `integer` |
| `float4` / `float8` | `number` |
| `text` / `varchar` / `bpchar` / `bytea` | `string` (raw bytes) |
| anything else (`numeric`, `timestamptz`, `uuid`, `jsonb`, arrays, `bool`, …) | `string` = the value's Postgres text representation |

`bool` maps to its text form (`'t'`/`'f'`), **not** a Lua boolean, to keep read behavior
aligned with SQLite (which has no native bool and reads counters/flags back as
`integer`/text). `numeric` renders as text so arbitrary precision is never silently lost to
`i64`/`f64`. A user who wants structure decodes it themselves (`SELECT data::jsonb …` +
`lur.json.decode`).

### Parameter binding — highest-risk area

`bind_one` maps a Lua `Value` to a Postgres parameter, mirroring SQLite's intent:

| Lua value | Postgres bind |
|---|---|
| `nil` / `lur.null` | `NULL` |
| `integer` | `int8` |
| `number` (integral, in i64 range) | `int8`; else `float8` |
| `string` | `text` when valid UTF-8; else `bytea` (raw bytes) |
| `boolean` | `bool` |
| other (table, …) | error: encode with `lur.json.encode` |

Postgres is statically typed, so unlike SQLite the bound type must be compatible with the
target column. In the common case (text data → text column, integers → int/bigint) this is
transparent. For non-obvious columns (e.g. UTF-8 data destined for a `bytea` column, or a
`boolean` bound where the column is `int`), the user adds an explicit `$1::bytea` /
`$1::int` cast in their SQL — consistent with the native-dialect principle. Binding is the
subtlest part of this backend and its exact behavior is pinned by tests during
implementation.

### `exec` result

`PgQueryResult::rows_affected()` populates `ExecResult.rows_affected`. Postgres has **no**
`last_insert_rowid()` equivalent, so `ExecResult.last_insert_id` is always `0` on Postgres.
Generated keys are obtained the native way: `db.query("INSERT … RETURNING id")`. This
parity gap is documented.

### kv operations

The `kind`-discriminated schema maps the neutral value model 1:1:

- **`kv_get`**: `kind = 0` → `bytes`; `kind = 1` → `num` rendered as its decimal string
  (matching SQLite's `value_to_bytes` INTEGER→decimal-string). Absent → `nil`.
- **`kv_set` / `kv_add`**: write `kind = 0`, `bytes = $value`. `kv_add` uses
  `INSERT … ON CONFLICT(key) DO NOTHING` and reports whether a row was inserted.
- **`kv_cas`**: operates on `kind = 0 AND bytes = $expected`. A `kind = 1` counter row never
  matches a bytes comparison — exactly SQLite's behavior, where an `INTEGER` value never
  equals a `BLOB`-bound expected value.
- **`kv_incr`** (serves `incr`/`decr`): single atomic upsert with the integer guard,
  mirroring SQLite's `RETURNING` form:

  ```sql
  INSERT INTO lur_kv (key, kind, num) VALUES ($1, 1, $2)
  ON CONFLICT(key) DO UPDATE SET num = lur_kv.num + excluded.num
  WHERE lur_kv.kind = 1
  RETURNING num
  ```

  `WHERE kind = 1` is the equivalent of SQLite's `typeof(value)='integer'` guard; no row
  returned → "existing value is not an integer".

The Phase 1 commitment holds: **do not** alter the SQLite schema to harmonize; the neutral
model lives at the seam, so a future one-way migration reads `(kind, payload)` from one
backend and writes it to the other.

## Correctness model: isolation and the fallible transactional-API contract

This is the load-bearing decision of Phase 2.

### Single-statement ops — `READ COMMITTED`, atomic, never transiently fail

`db.exec` and the single-statement kv ops (`get`/`set`/`delete`/`add`/`cas`/`incr`/`decr`)
run at Postgres's default `READ COMMITTED`. A single statement is atomic at any isolation
level; the server-side guards (`ON CONFLICT`, `WHERE bytes = $expected`, `WHERE kind = 1`)
make them correct. They take no lock beyond row locks and never surface a serialization
error. No retry layer is needed or added.

### Transactional callback APIs — `SERIALIZABLE`, **fallible**

`db.tx` and `kv.update` are the only two APIs that run a user callback inside a
multi-statement transaction, and the only place a read-then-write anomaly can occur. On
Postgres both run at **`SERIALIZABLE`**:

- SERIALIZABLE (SSI) protects against anomalies from **any** concurrent writer on the
  server — including uncoordinated non-`lur` processes writing the same tables. This is the
  property that matters when connecting to an operator's shared database, and it is
  unattainable by a cooperative advisory lock (which only serializes writers that opt into
  it). This is *why* these APIs must be fallible.
- The cost is that a serialization conflict aborts the transaction with SQLSTATE `40001`,
  surfaced (typically at `COMMIT`) as a lur-voiced error. `lur` does **not** auto-retry:
  a `db.tx` body or `kv.update` transform may contain external side effects (`lur.http`,
  logging) that re-running would duplicate — the same reason SQLite's retry never re-runs a
  transaction body.

**Unified contract (parity at the contract level):** `db.tx` and `kv.update` **may raise a
transient error; the caller decides the policy** — retry, `pcall`-and-swallow, or let it
propagate. This contract is *already true on SQLite* (its retry can exhaust, and SQL/`error()`
failures propagate); Phase 2 only makes it explicit and adds one more transient reason on
Postgres. Parity is at this contract level — both backends' `db.tx`/`kv.update` "may fail,
you handle it" — not at failure *frequency*. SQLite keeps its automatic retry, so it simply
fails less often for the same contract.

Idiomatic Lua, identical across backends:

```lua
-- wrap for resilience (SQLite needs this too: its retry can exhaust)
local ok, err = pcall(function()
  return lur.db.tx(function(tx) … end)
end)
-- or a bounded retry loop, or let it propagate — the caller's choice
```

Mechanism: `PgBackend::begin` and `PgBackend::kv_update` open the transaction at
`SERIALIZABLE`; `PgTransaction::commit` returns the `40001` error, which the existing
`db.rs` `run_tx` propagates. SQLite's `begin` (`BEGIN IMMEDIATE` + retry) is unchanged.

## Testing

- **Local:** a `docker-compose.yaml` at the repo root brings up a Postgres service at
  `postgres://postgres:postgres@localhost:5432/postgres` (`POSTGRES_PASSWORD=postgres`,
  default `postgres` db, port 5432). README notes `docker compose up -d` before
  `cargo nextest run`.
- **URL discovery + skip/require:** tests read `LUR_TEST_PG_URL`, defaulting to the compose
  URL when unset. If the server is unreachable, tests **skip** locally (early return +
  `eprintln!` noting the skip) so the SQLite suite still runs everywhere. When the `CI`
  environment variable is set, an unreachable server is instead a **hard failure** — CI
  provisions the service, so unreachable means broken, not absent. No new gating flag.
- **CI:** the GitHub Actions test job adds a Postgres **service container** (image pinned by
  digest, per the pin-to-SHA convention) with a healthcheck; the test step sets
  `LUR_TEST_PG_URL` to the service and relies on `CI` being set. SQLite-only jobs are
  untouched. `cargo deny` runs as today.
- **New `tests/pg.rs`:** replays the `tests/db.rs` / kv scenarios against Postgres for parity
  (exec/query/tx, kv get/set/delete/add/cas/incr/decr/update, type-aware reads), plus
  Postgres-specific coverage:
  - `db.tx` / `kv.update` raise on a `40001` serialization conflict (two concurrent
    conflicting serializable transactions), and the error is catchable via `pcall`;
  - row type mapping (`numeric`/`timestamptz`/`jsonb` → text, core types → integer/number/
    string, `bytea` → bytes);
  - the `kind` schema: `incr` on a bytes key errors with the integer-guard message; a value
    written by `update`/`set` compares equal under `cas`;
  - `exec` `last_insert_id` is `0` and `RETURNING` yields generated keys via `query`.
- Existing SQLite tests pass **unchanged**.

## Documentation

- **README.md** — `lur.db` / `lur.kv` sections: `--db` now accepts a `postgres://` /
  `postgresql://` connection string; native placeholders per backend (`?` vs `$n`); the
  Postgres row/type mapping and the `last_insert_id`/`RETURNING` note; the
  `sslmode`-in-URL TLS note; and the **fallible `db.tx` / `kv.update` contract** with the
  `pcall`/retry example. Note the `docker compose up -d` prerequisite for PG tests.
- **ARCHITECTURE.md** — storage seam section: add the `Postgres` backend, the isolation
  model (single-statement `READ COMMITTED`; `db.tx`/`kv.update` `SERIALIZABLE` and fallible;
  retry stays SQLite-only), the operator-trusted connection (exempt from the script-facing
  net allowlist/SSRF, like the SQLite path is exempt from `lur.fs`), and the `kind` kv
  schema.
- **docs/GUIDE.md** — extend the `lur.db` / `lur.kv` cookbook with a Postgres `--db` example
  and the `pcall`-wrapped `db.tx`. Keep the `tests/guide.rs` drift guards green.

## Risks

- **Parameter binding** (static typing) is the subtlest part; anchored by explicit
  bind/round-trip tests per type in `tests/pg.rs`.
- **TLS feature + cooldown:** the rustls feature name and its transitive crates must clear
  `cargo deny` and the 7-day cooldown; resolve at implementation time, pinning older
  versions if needed.
- **CI flakiness:** the concurrent-`40001` test must deterministically force a serialization
  conflict (two overlapping serializable read-then-write transactions), not rely on timing.

## Deliverable

A `Postgres` backend behind the existing seam giving `lur.db` / `lur.kv` full contract-level
parity, with correctness (SERIALIZABLE for the transactional callback APIs) chosen over
minimizing failures — the transactional APIs are honestly documented as fallible on both
backends. `db.rs` / `kv.rs` are untouched, vindicating the Phase 1 boundary.
