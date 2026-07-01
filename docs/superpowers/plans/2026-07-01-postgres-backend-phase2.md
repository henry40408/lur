# PostgreSQL Backend (Phase 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a PostgreSQL backend behind the Phase 1 storage seam, giving `lur.db` / `lur.kv` full contract-level parity with the SQLite backend.

**Architecture:** Extend `enum Backend`/`enum Transaction` in `src/capabilities/storage/mod.rs` with a `Postgres` variant, implemented in a new `src/capabilities/storage/postgres.rs` that mirrors `sqlite.rs`'s responsibilities (pool, `$n` binding, row→Lua mapping, isolation, kv schema). `db.rs`/`kv.rs` call sites are untouched. Backend is selected at first use by the `--db` value's scheme (`postgres://` → Postgres, else SQLite file). PG is built up as vertical slices: db.exec/query → db.tx → kv single-statement → kv.update → PG-specific correctness → docs.

**Tech Stack:** Rust (edition 2024), `sqlx` 0.9 (add `postgres` + a rustls TLS feature), `mlua` (Luau), `tokio`. Tests via `cargo nextest run` against a Postgres from `docker-compose.yaml`.

Spec: `docs/superpowers/specs/2026-07-01-postgres-backend-phase2-design.md`.

## Global Constraints

- **Dependency cooldown:** any newly-added crate version MUST be ≥7 days old at pin time (`cargo info <crate>` / crates.io). Pin an older version if the resolved one is too new.
- **No new crypto backend churn:** the rustls TLS feature chosen for sqlx MUST reuse the same crypto provider `reqwest` already pulls (verify with `cargo tree`), not introduce a second one.
- **`cargo deny check` stays green** with the new TLS/postgres crates (advisories, licenses in the `deny.toml` allowlist, sources).
- **`cargo fmt --all` + `cargo clippy --all-targets -- -D warnings` + `cargo nextest run` green.** Clippy `-D warnings` bans `unimplemented!()`/`todo!()`; unbuilt slices MUST return a real `Err(mlua::Error::runtime(..))`, never a stub macro.
- **SQLite path unchanged:** no edit to `src/capabilities/storage/sqlite.rs` behavior, and **no existing test file edited** (`tests/db.rs` etc. must compile and pass verbatim). The retry layer (`retry_busy`/`is_busy`/`jitter_delay`) stays SQLite-only.
- **`db.rs` / `kv.rs` unchanged:** Phase 2 touches only `storage/mod.rs`, the new `storage/postgres.rs`, `Cargo.toml`, CI, docs, and `tests/pg.rs`.
- **`RuntimeConfig.db_path: Option<PathBuf>` unchanged** (the connection string round-trips through `PathBuf`; scheme detection is internal to `storage`).

## File Structure

- **Create `src/capabilities/storage/postgres.rs`** — `PgBackend` + `PgTransaction`; all PG SQL, `$n` binding, row→Lua mapping (R1), isolation, kv `kind`-schema ops. No retry layer.
- **Modify `src/capabilities/storage/mod.rs`** — add `pub(crate) mod postgres;`, the private `enum StorageTarget`, `Backend::Postgres`/`Transaction::Postgres` variants + match arms, and scheme dispatch in `Shared::ensure`.
- **Modify `Cargo.toml`** — enable sqlx `postgres` + a rustls TLS feature.
- **Create `docker-compose.yaml`** — a Postgres service for local tests.
- **Modify `.github/workflows/ci.yml`** — a Postgres service container on the `test` job.
- **Create `tests/pg.rs`** — connect-or-skip harness + PG parity + PG-specific tests.
- **Modify `README.md`, `ARCHITECTURE.md`, `docs/GUIDE.md`** — document the PG backend and the fallible transactional-API contract.

---

### Task 1: Dependencies, docker-compose, and CI service

**Files:**
- Modify: `Cargo.toml:35`
- Create: `docker-compose.yaml`
- Modify: `.github/workflows/ci.yml` (the `test:` job, lines ~30-44)

**Interfaces:**
- Produces: the sqlx `postgres` + rustls TLS feature enabled (so `sqlx::postgres::*` and `sqlx::Postgres` are importable in later tasks); a local Postgres at `postgres://postgres:postgres@localhost:5432/postgres`; CI Postgres reachable via `LUR_TEST_PG_URL` with `CI` set.

- [ ] **Step 1: Discover reqwest's rustls crypto provider (probe before choosing the sqlx feature)**

Run: `cargo tree -e features -i rustls 2>/dev/null | head -40 ; echo '---' ; cargo tree -i aws-lc-rs 2>/dev/null | head -5 ; cargo tree -i ring 2>/dev/null | head -5`

Read which provider is already in the graph: if `aws-lc-rs` appears, the sqlx feature is `tls-rustls-aws-lc-rs`; if `ring` appears, it is `tls-rustls-ring-webpki`. Use the one that matches so no second crypto backend is added.

- [ ] **Step 2: Enable the sqlx features**

Modify `Cargo.toml` line 35. Replace:

```toml
sqlx = { version = "0.9.0", default-features = false, features = ["runtime-tokio", "sqlite"] }
```

with (pick the TLS feature from Step 1 — this example matches an `aws-lc-rs` graph):

```toml
sqlx = { version = "0.9.0", default-features = false, features = ["runtime-tokio", "sqlite", "postgres", "tls-rustls-aws-lc-rs"] }
```

- [ ] **Step 3: Verify build, cooldown, and deny**

Run: `cd /Users/henry/Develop/claude/lur && cargo build --all-targets 2>&1 | tail -20`
Expected: builds clean (no errors; the new features compile).

Run: `cd /Users/henry/Develop/claude/lur && cargo tree 2>/dev/null | grep -Ei 'rustls|ring|aws-lc|webpki|sqlx-postgres' | sort -u`
For every crate line that is **new** vs `main`, run `cargo info <crate>` and confirm the resolved version's release date is ≥7 days before today (2026-07-01). If any is too new, pin the crate to the newest version that is ≥7 days old via a `[dependencies]` entry or `cargo update -p <crate> --precise <older>`.

Run: `cd /Users/henry/Develop/claude/lur && cargo deny check 2>&1 | tail -30`
Expected: `advisories ok`, `bans ok`, `licenses ok`, `sources ok`. If a new TLS crate carries a license not in `deny.toml`'s allowlist (e.g. an `OpenSSL`-licensed `aws-lc-sys`), that is a real gate failure — switch to the `ring` rustls feature from Step 1 instead and re-run.

- [ ] **Step 4: Add docker-compose.yaml**

Create `docker-compose.yaml`:

```yaml
services:
  postgres:
    image: postgres:17-alpine
    environment:
      POSTGRES_PASSWORD: postgres
    ports:
      - "5432:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 2s
      timeout: 3s
      retries: 15
```

- [ ] **Step 5: Verify compose brings up a reachable Postgres**

Run: `cd /Users/henry/Develop/claude/lur && docker compose config >/dev/null && echo COMPOSE_OK && docker compose up -d && sleep 5 && docker compose exec -T postgres pg_isready -U postgres`
Expected: `COMPOSE_OK` then `... accepting connections`.

- [ ] **Step 6: Add the Postgres service to the CI `test` job**

Modify `.github/workflows/ci.yml`. Pin the image digest first:
Run: `docker pull postgres:17-alpine >/dev/null && docker inspect --format='{{index .RepoDigests 0}}' postgres:17-alpine`
Use the printed `postgres@sha256:...` as `image:` below. Replace the `test:` job with:

```yaml
  test:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres@sha256:PASTE_DIGEST_FROM_COMMAND_ABOVE
        env:
          POSTGRES_PASSWORD: postgres
        ports:
          - 5432:5432
        options: >-
          --health-cmd "pg_isready -U postgres"
          --health-interval 2s
          --health-timeout 3s
          --health-retries 15
    env:
      LUR_TEST_PG_URL: postgres://postgres:postgres@localhost:5432/postgres
      CI: "1"
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7.0.0
      - uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8 # stable, 2026-03-27
      - uses: Swatinem/rust-cache@c19371144df3bb44fab255c43d04cbc2ab54d1c4 # v2.9.1
      - uses: taiki-e/install-action@8b3c737da4b541bf0fb5a3e0488ff20535badac9 # v2.82.1
        with:
          tool: nextest
      - name: nextest
        run: cargo nextest run
```

- [ ] **Step 7: Verify YAML + commit**

Run: `cd /Users/henry/Develop/claude/lur && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); yaml.safe_load(open('docker-compose.yaml')); print('YAML_OK')"`
Expected: `YAML_OK`.

```bash
git add Cargo.toml Cargo.lock docker-compose.yaml .github/workflows/ci.yml
git commit -m "build(storage): enable sqlx postgres + rustls, add PG compose & CI service"
```

---

### Task 2: PgBackend open + db.exec / db.query (backend selection, binding, R1 row mapping)

**Files:**
- Create: `src/capabilities/storage/postgres.rs`
- Modify: `src/capabilities/storage/mod.rs`
- Create: `tests/pg.rs`

**Interfaces:**
- Consumes: `super::ExecResult { rows_affected: u64, last_insert_id: i64 }`; `crate::capabilities::null`.
- Produces: `PgBackend { pool: PgPool }` with `open(url: &str) -> mlua::Result<Self>`, `exec(&Lua, String, Vec<Value>) -> mlua::Result<super::ExecResult>`, `query(&Lua, String, Vec<Value>) -> mlua::Result<Table>`; `Backend::Postgres(PgBackend)` variant; `enum StorageTarget`; `Shared::ensure` scheme dispatch. `begin`/`kv_*`/`kv_update` Postgres arms return a temporary runtime error (implemented in Tasks 3–5).

- [ ] **Step 1: Write the failing test (db.exec + db.query parity, and R1 non-core error)**

Create `tests/pg.rs`:

```rust
use lur::runtime::{Runtime, RuntimeConfig};

/// The Postgres URL for tests: `LUR_TEST_PG_URL` or the docker-compose default.
fn pg_test_url() -> String {
    std::env::var("LUR_TEST_PG_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".to_string())
}

/// A runtime pointed at Postgres, or `None` when the server is unreachable.
/// Locally an unreachable server SKIPS the test (returns None); under CI it is a
/// hard failure (panics), because CI provisions the service.
fn pg_runtime() -> Option<Runtime> {
    let url = pg_test_url();
    // Cheap reachability probe: open a TCP connection to host:port.
    let reachable = reachable(&url);
    if !reachable {
        if std::env::var("CI").is_ok() {
            panic!("CI: Postgres at {url} is unreachable but CI must provision it");
        }
        eprintln!("skipping PG test: {url} unreachable (start it: docker compose up -d)");
        return None;
    }
    Some(
        Runtime::with_config(RuntimeConfig {
            db_path: Some(std::path::PathBuf::from(url)),
            ..Default::default()
        })
        .expect("runtime builds"),
    )
}

/// Parse host:port out of a postgres URL and try a TCP connect with a short timeout.
fn reachable(url: &str) -> bool {
    use std::net::ToSocketAddrs;
    use std::time::Duration;
    // postgres://user:pass@host:port/db  ->  host:port
    let after_scheme = url.split("://").nth(1).unwrap_or("");
    let authority = after_scheme.split('/').next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().unwrap_or(5432)),
        None => (hostport, 5432),
    };
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok())
}

/// A fresh, uniquely-named table per test so parallel tests don't collide on the
/// shared database. Caller drops it via `DROP TABLE IF EXISTS`.
fn unique(prefix: &str) -> String {
    // Vary by a monotonically increasing process-local counter (no Date/random).
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("{prefix}_{}", N.fetch_add(1, Ordering::Relaxed))
}

#[test]
fn pg_exec_and_query_round_trip_with_type_mapping() {
    let Some(rt) = pg_runtime() else { return };
    let t = unique("pg_rt");
    rt.run(&format!(
        "lur.db.exec('DROP TABLE IF EXISTS {t}')\n\
         lur.db.exec('CREATE TABLE {t} (id BIGINT, r DOUBLE PRECISION, s TEXT, n TEXT)')\n\
         local w = lur.db.exec('INSERT INTO {t} VALUES ($1,$2,$3,$4)', 42, 3.5, 'hi', lur.null)\n\
         assert(w.rows_affected == 1, 'rows_affected')\n\
         assert(w.last_insert_id == 0, 'pg has no last_insert_id')\n\
         local rows = lur.db.query('SELECT id, r, s, n FROM {t} ORDER BY id')\n\
         assert(#rows == 1, 'one row')\n\
         assert(rows[1].id == 42, 'int8->integer')\n\
         assert(rows[1].r == 3.5, 'float8->number')\n\
         assert(rows[1].s == 'hi', 'text->string')\n\
         assert(rows[1].n == lur.null, 'null->lur.null')\n\
         lur.db.exec('DROP TABLE {t}')"
    ))
    .expect("pg exec/query round-trip");
}

#[test]
fn pg_noncore_column_errors_until_cast_to_text() {
    let Some(rt) = pg_runtime() else { return };
    let t = unique("pg_noncore");
    rt.run(&format!("lur.db.exec('DROP TABLE IF EXISTS {t}')\n\
        lur.db.exec('CREATE TABLE {t} (j JSONB)')\n\
        lur.db.exec($$INSERT INTO {t} VALUES ('{{\"a\":1}}')$$)"))
        .expect("setup jsonb table");
    // Reading jsonb directly errors with the cast-to-text guidance.
    let err = rt
        .run(&format!("lur.db.query('SELECT j FROM {t}')"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("unsupported column type") && err.contains("::text"), "got: {err}");
    // Casting to text succeeds.
    rt.run(&format!(
        "local rows = lur.db.query('SELECT j::text AS j FROM {t}')\n\
         assert(rows[1].j:find('\"a\"'), 'jsonb text form')\n\
         lur.db.exec('DROP TABLE {t}')"
    ))
    .expect("cast-to-text read works");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/henry/Develop/claude/lur && docker compose up -d >/dev/null 2>&1; cargo nextest run --test pg 2>&1 | tail -20`
Expected: FAIL — either a compile error (no PG backend yet) or a runtime error that `postgres://…` is unsupported.

- [ ] **Step 3: Create `src/capabilities/storage/postgres.rs` (open + binding + R1 mapping + exec/query)**

```rust
//! PostgreSQL storage backend: owns the `sqlx` `PgPool` and all PG-specific SQL,
//! `$n` binding, row→Lua mapping (core types only; non-core must be cast to text),
//! isolation, and the `kind`-discriminated kv schema. No retry layer — under
//! `READ COMMITTED` single statements block rather than surfacing a busy error,
//! and the `SERIALIZABLE` transactional APIs are documented as fallible.

use std::str::FromStr;

use mlua::{Error, Function, Lua, Table, Value};
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Column, Postgres, Row, TypeInfo, ValueRef};

use crate::capabilities::null;

/// A dynamically-bound Postgres query.
pub(crate) type PgQuery<'q> = sqlx::query::Query<'q, Postgres, PgArguments>;

/// Bind each Lua value as a positional (`$n`) parameter.
pub(crate) fn bind_all<'q>(mut q: PgQuery<'q>, params: &[Value]) -> mlua::Result<PgQuery<'q>> {
    for v in params {
        q = bind_one(q, v)?;
    }
    Ok(q)
}

fn bind_one<'q>(q: PgQuery<'q>, v: &Value) -> mlua::Result<PgQuery<'q>> {
    Ok(match v {
        // A NULL is bound as a text NULL; inserting it into a strictly-typed
        // non-text column may need an explicit `$1::int` cast (native-dialect
        // principle), same as reading a non-core type back.
        Value::Nil => q.bind(None::<String>),
        Value::UserData(_) if null::is_null(v) => q.bind(None::<String>),
        Value::Boolean(b) => q.bind(*b),
        Value::Integer(i) => q.bind(*i),
        Value::Number(n) => {
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                q.bind(*n as i64)
            } else {
                q.bind(*n)
            }
        }
        Value::String(s) => {
            let bytes = s.as_bytes();
            match std::str::from_utf8(&bytes) {
                Ok(text) => q.bind(text.to_owned()),
                Err(_) => q.bind(bytes.to_vec()),
            }
        }
        other => {
            return Err(Error::runtime(format!(
                "lur.db: cannot bind a {} value (encode tables with lur.json.encode)",
                other.type_name()
            )));
        }
    })
}

/// Convert a result row to a Lua table keyed by column name. Only core scalar
/// types map; a non-core column raises a clear cast-to-text error (R1) — `sqlx`
/// returns Postgres values in the binary wire format and cannot render an
/// arbitrary type as text, so `lur` never guesses a representation.
pub(crate) fn read_row(lua: &Lua, row: &PgRow) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for col in row.columns() {
        let i = col.ordinal();
        let raw = row
            .try_get_raw(i)
            .map_err(|e| Error::runtime(format!("lur.db: {e}")))?;
        let value = if raw.is_null() {
            null::value(lua)?
        } else {
            match raw.type_info().name() {
                "INT2" => Value::Integer(i64::from(get::<i16>(row, i)?)),
                "INT4" => Value::Integer(i64::from(get::<i32>(row, i)?)),
                "INT8" => Value::Integer(get::<i64>(row, i)?),
                "FLOAT4" => Value::Number(f64::from(get::<f32>(row, i)?)),
                "FLOAT8" => Value::Number(get::<f64>(row, i)?),
                "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => {
                    Value::String(lua.create_string(get::<String>(row, i)?)?)
                }
                "BYTEA" => Value::String(lua.create_string(get::<Vec<u8>>(row, i)?)?),
                other => {
                    let name = col.name();
                    return Err(Error::runtime(format!(
                        "lur.db: unsupported column type '{other}' in column '{name}'; \
                         CAST it to text (e.g. {name}::text)"
                    )));
                }
            }
        };
        t.set(col.name(), value)?;
    }
    Ok(t)
}

fn get<'r, T>(row: &'r PgRow, i: usize) -> mlua::Result<T>
where
    T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get::<T, usize>(i)
        .map_err(|e| Error::runtime(format!("lur.db: decoding column {i}: {e}")))
}

/// Postgres backend: owns the pool. Cloning is a cheap `sqlx` pool handle clone.
#[derive(Clone)]
pub(crate) struct PgBackend {
    pool: PgPool,
}

impl PgBackend {
    /// Connect to an operator-provided database (which must already exist) and
    /// ensure the internal `lur_kv` table. `sslmode` in the URL is honored.
    pub(crate) async fn open(url: &str) -> mlua::Result<Self> {
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| Error::runtime(format!("lur.db: invalid postgres url: {e}")))?;
        let pool = PgPoolOptions::new()
            .connect_with(opts)
            .await
            .map_err(|e| Error::runtime(format!("lur.db: connecting to postgres: {e}")))?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS lur_kv (\
             key TEXT PRIMARY KEY, kind SMALLINT NOT NULL, bytes BYTEA, num BIGINT)",
        )
        .execute(&pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.db: ensuring lur_kv: {e}")))?;
        Ok(Self { pool })
    }

    pub(crate) async fn exec(
        &self,
        _lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<super::ExecResult> {
        let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .execute(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.exec: {e}")))?;
        // Postgres has no last_insert_rowid(); generated keys come via RETURNING.
        Ok(super::ExecResult {
            rows_affected: res.rows_affected(),
            last_insert_id: 0,
        })
    }

    pub(crate) async fn query(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<Table> {
        let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.query: {e}")))?;
        let out = lua.create_table()?;
        for (i, row) in rows.iter().enumerate() {
            out.raw_set(i as i64 + 1, read_row(lua, row)?)?;
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: Wire the seam in `src/capabilities/storage/mod.rs`**

Add near the top (after `pub(crate) mod sqlite;`):

```rust
pub(crate) mod postgres;

use postgres::PgBackend;
use sqlite::{SqliteBackend, SqliteTransaction};
```

(Adjust the existing `use sqlite::...` line so it is not duplicated.)

Add above `Shared`:

```rust
/// Which concrete backend a `--db` value selects, resolved by URL scheme.
enum StorageTarget {
    Sqlite(std::path::PathBuf),
    Postgres(String),
}

impl StorageTarget {
    fn resolve(path: &std::path::Path) -> Self {
        match path.to_str() {
            Some(s) if s.starts_with("postgres://") || s.starts_with("postgresql://") => {
                StorageTarget::Postgres(s.to_owned())
            }
            _ => StorageTarget::Sqlite(path.to_path_buf()),
        }
    }
}
```

Change the `Backend` enum and add `Postgres` arms. Replace the enum and its `impl` methods so each dispatch matches both variants. The `Postgres` arms for `exec`/`query` call the real methods; `begin` and every `kv_*`/`kv_update` arm return a temporary error until Tasks 3–5:

```rust
#[derive(Clone)]
pub(crate) enum Backend {
    Sqlite(SqliteBackend),
    Postgres(PgBackend),
}
```

For `exec`:
```rust
match self {
    Backend::Sqlite(b) => b.exec(lua, sql, params).await,
    Backend::Postgres(b) => b.exec(lua, sql, params).await,
}
```
For `query`:
```rust
match self {
    Backend::Sqlite(b) => b.query(lua, sql, params).await,
    Backend::Postgres(b) => b.query(lua, sql, params).await,
}
```
For `begin` (temporary — real in Task 3):
```rust
match self {
    Backend::Sqlite(b) => Ok(Transaction::Sqlite(b.begin().await?)),
    Backend::Postgres(_) => Err(Error::runtime("lur.db.tx: postgres backend not yet implemented")),
}
```
For each of `kv_get`, `kv_set`, `kv_delete`, `kv_add`, `kv_cas`, `kv_incr`, `kv_update`, add a `Postgres` arm returning a matching temporary error, e.g. for `kv_get`:
```rust
match self {
    Backend::Sqlite(b) => b.kv_get(lua, key).await,
    Backend::Postgres(_) => Err(Error::runtime("lur.kv: postgres backend not yet implemented")),
}
```
(Use the same message pattern per method; these are replaced in Tasks 4–5. `begin` returns `mlua::Result<Transaction>`, so its temporary arm returns `Err(..)` directly.)

Change `Shared::ensure` to dispatch on scheme:

```rust
pub(crate) async fn ensure(&self) -> mlua::Result<Backend> {
    if let Some(b) = self.cell.get() {
        return Ok(b.clone());
    }
    let path = self
        .path
        .as_ref()
        .as_ref()
        .ok_or_else(|| Error::runtime("lur.db: no database configured; pass --db <path>"))?;
    let backend = match StorageTarget::resolve(path) {
        StorageTarget::Sqlite(p) => Backend::Sqlite(SqliteBackend::open(&p).await?),
        StorageTarget::Postgres(url) => Backend::Postgres(PgBackend::open(&url).await?),
    };
    let _ = self.cell.set(backend);
    Ok(self.cell.get().expect("backend just set").clone())
}
```

- [ ] **Step 5: Run tests + fmt + clippy**

Run: `cd /Users/henry/Develop/claude/lur && cargo fmt --all && cargo clippy --all-targets -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg 2>&1 | tail -20`
Expected: PASS (`pg_exec_and_query_round_trip_with_type_mapping`, `pg_noncore_column_errors_until_cast_to_text`).

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test db 2>&1 | tail -8`
Expected: PASS (SQLite path unchanged).

- [ ] **Step 6: Commit**

```bash
git add src/capabilities/storage/postgres.rs src/capabilities/storage/mod.rs tests/pg.rs
git commit -m "feat(storage): PostgreSQL backend db.exec/db.query behind the seam"
```

---

### Task 3: db.tx over a SERIALIZABLE Postgres transaction

**Files:**
- Modify: `src/capabilities/storage/postgres.rs`
- Modify: `src/capabilities/storage/mod.rs`
- Modify: `tests/pg.rs`

**Interfaces:**
- Consumes: `PgBackend`, `super::ExecResult`, `bind_all`, `read_row`.
- Produces: `PgBackend::begin() -> mlua::Result<PgTransaction>`; `PgTransaction` with async `exec`/`query`/`commit`/`rollback`; `Transaction::Postgres(PgTransaction)` variant + arms.

- [ ] **Step 1: Write the failing test (commit + rollback parity)**

Add to `tests/pg.rs`:

```rust
#[test]
fn pg_tx_commits_and_rolls_back() {
    let Some(rt) = pg_runtime() else { return };
    let t = unique("pg_tx");
    rt.run(&format!(
        "lur.db.exec('DROP TABLE IF EXISTS {t}')\n\
         lur.db.exec('CREATE TABLE {t} (id BIGINT PRIMARY KEY, bal BIGINT)')\n\
         lur.db.exec('INSERT INTO {t} VALUES (1,100),(2,0)')\n\
         lur.db.tx(function(tx)\n\
           tx.exec('UPDATE {t} SET bal = bal - 50 WHERE id = 1')\n\
           tx.exec('UPDATE {t} SET bal = bal + 50 WHERE id = 2')\n\
         end)\n\
         assert(lur.db.query('SELECT bal FROM {t} WHERE id=1')[1].bal == 50, 'committed 1')\n\
         assert(lur.db.query('SELECT bal FROM {t} WHERE id=2')[1].bal == 50, 'committed 2')\n\
         local ok = pcall(function()\n\
           lur.db.tx(function(tx)\n\
             tx.exec('UPDATE {t} SET bal = 999 WHERE id = 1')\n\
             error('boom')\n\
           end)\n\
         end)\n\
         assert(not ok, 'tx raised')\n\
         assert(lur.db.query('SELECT bal FROM {t} WHERE id=1')[1].bal == 50, 'rolled back')\n\
         lur.db.exec('DROP TABLE {t}')"
    ))
    .expect("pg tx commit + rollback");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg -E 'test(pg_tx_commits_and_rolls_back)' 2>&1 | tail -12`
Expected: FAIL with "postgres backend not yet implemented".

- [ ] **Step 3: Add `PgBackend::begin` + `PgTransaction` to `postgres.rs`**

Add these imports to the top of `postgres.rs`:
```rust
use sqlx::pool::PoolConnection;
```

Add to `impl PgBackend`:
```rust
    /// Open a `SERIALIZABLE` write transaction on a pinned connection. Serializable
    /// (SSI) protects `db.tx` read-then-write logic against any concurrent writer,
    /// at the cost that a conflict aborts with SQLSTATE 40001 — surfaced (usually
    /// at COMMIT) as a lur-voiced error the caller handles. No retry (a body may
    /// have external side effects).
    pub(crate) async fn begin(&self) -> mlua::Result<PgTransaction> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        sqlx::query("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut *conn)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        Ok(PgTransaction {
            conn: tokio::sync::Mutex::new(Some(conn)),
        })
    }
```

Add at the end of `postgres.rs`:
```rust
/// A pinned-connection Postgres write transaction. `exec`/`query` run on the
/// pinned connection; `commit`/`rollback` take it. A call after finish errors.
/// `commit` can surface SQLSTATE 40001 (serialization failure).
pub(crate) struct PgTransaction {
    conn: tokio::sync::Mutex<Option<PoolConnection<Postgres>>>,
}

impl PgTransaction {
    pub(crate) async fn exec(
        &self,
        _lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<super::ExecResult> {
        let mut guard = self.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
        let res = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .execute(&mut **conn)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx exec: {e}")))?;
        Ok(super::ExecResult {
            rows_affected: res.rows_affected(),
            last_insert_id: 0,
        })
    }

    pub(crate) async fn query(
        &self,
        lua: &Lua,
        sql: String,
        params: Vec<Value>,
    ) -> mlua::Result<Table> {
        let mut guard = self.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or_else(|| Error::runtime("lur.db.tx: transaction already finished"))?;
        let rows = bind_all(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())), &params)?
            .fetch_all(&mut **conn)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx query: {e}")))?;
        let out = lua.create_table()?;
        for (i, row) in rows.iter().enumerate() {
            out.raw_set(i as i64 + 1, read_row(lua, row)?)?;
        }
        Ok(out)
    }

    pub(crate) async fn commit(&self) -> mlua::Result<()> {
        let mut guard = self.conn.lock().await;
        if let Some(mut conn) = guard.take()
            && let Err(e) = sqlx::query("COMMIT").execute(&mut *conn).await
        {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            return Err(Error::runtime(format!("lur.db.tx: commit: {e}")));
        }
        Ok(())
    }

    pub(crate) async fn rollback(&self) {
        let mut guard = self.conn.lock().await;
        if let Some(mut conn) = guard.take() {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
        }
    }
}
```

- [ ] **Step 4: Wire the `Transaction::Postgres` variant in `mod.rs`**

Update the import:
```rust
use postgres::{PgBackend, PgTransaction};
```

Change the enum:
```rust
pub(crate) enum Transaction {
    Sqlite(SqliteTransaction),
    Postgres(PgTransaction),
}
```

Add a `Postgres` arm to each `Transaction` method (`exec`, `query`, `commit`, `rollback`), e.g. `exec`:
```rust
match self {
    Transaction::Sqlite(t) => t.exec(lua, sql, params).await,
    Transaction::Postgres(t) => t.exec(lua, sql, params).await,
}
```
and `commit`:
```rust
match self {
    Transaction::Sqlite(t) => t.commit().await,
    Transaction::Postgres(t) => t.commit().await,
}
```
(Same shape for `query` and `rollback`.)

Replace the temporary `Backend::begin` Postgres arm with the real one:
```rust
match self {
    Backend::Sqlite(b) => Ok(Transaction::Sqlite(b.begin().await?)),
    Backend::Postgres(b) => Ok(Transaction::Postgres(b.begin().await?)),
}
```

- [ ] **Step 5: Run tests + fmt + clippy**

Run: `cd /Users/henry/Develop/claude/lur && cargo fmt --all && cargo clippy --all-targets -- -D warnings 2>&1 | tail -8`
Expected: clean.

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg 2>&1 | tail -12`
Expected: PASS including `pg_tx_commits_and_rolls_back`.

- [ ] **Step 6: Commit**

```bash
git add src/capabilities/storage/postgres.rs src/capabilities/storage/mod.rs tests/pg.rs
git commit -m "feat(storage): db.tx over a SERIALIZABLE Postgres transaction"
```

---

### Task 4: Postgres kv single-statement ops (get/set/delete/add/cas/incr)

**Files:**
- Modify: `src/capabilities/storage/postgres.rs`
- Modify: `src/capabilities/storage/mod.rs`
- Modify: `tests/pg.rs`

**Interfaces:**
- Produces on `PgBackend`: `kv_get(&Lua, String) -> mlua::Result<Value>`, `kv_set(String, Vec<u8>) -> mlua::Result<()>`, `kv_delete(String) -> mlua::Result<()>`, `kv_add(String, Vec<u8>) -> mlua::Result<bool>`, `kv_cas(String, Option<Vec<u8>>, Option<Vec<u8>>) -> mlua::Result<bool>`, `kv_incr(&'static str, String, i64) -> mlua::Result<i64>`.

- [ ] **Step 1: Write the failing tests (kv parity)**

Add to `tests/pg.rs`:

```rust
#[test]
fn pg_kv_set_get_delete_add_cas() {
    let Some(rt) = pg_runtime() else { return };
    let k = unique("pgkv");
    rt.run(&format!(
        "assert(lur.kv.get('{k}') == nil, 'miss is nil')\n\
         lur.kv.set('{k}', 'v1')\n\
         assert(lur.kv.get('{k}') == 'v1', 'get after set')\n\
         lur.kv.set('{k}', 'v2')\n\
         assert(lur.kv.get('{k}') == 'v2', 'overwrite')\n\
         assert(lur.kv.add('{k}', 'nope') == false, 'add on existing = false')\n\
         assert(lur.kv.cas('{k}', 'wrong', 'v3') == false, 'cas mismatch = false')\n\
         assert(lur.kv.cas('{k}', 'v2', 'v3') == true, 'cas match = true')\n\
         assert(lur.kv.get('{k}') == 'v3', 'cas applied')\n\
         lur.kv.delete('{k}')\n\
         assert(lur.kv.get('{k}') == nil, 'gone after delete')\n\
         assert(lur.kv.add('{k}', 'fresh') == true, 'add on absent = true')\n\
         lur.kv.delete('{k}')"
    ))
    .expect("pg kv set/get/delete/add/cas");
}

#[test]
fn pg_kv_incr_decr_and_integer_guard() {
    let Some(rt) = pg_runtime() else { return };
    let c = unique("pgctr");
    let s = unique("pgstr");
    rt.run(&format!(
        "assert(lur.kv.incr('{c}') == 1, 'first incr = 1')\n\
         assert(lur.kv.incr('{c}', 5) == 6, 'incr by 5')\n\
         assert(lur.kv.decr('{c}', 2) == 4, 'decr by 2')\n\
         assert(lur.kv.get('{c}') == '4', 'counter reads as decimal string')\n\
         lur.kv.set('{s}', 'not-a-number')\n\
         local ok, err = pcall(function() lur.kv.incr('{s}') end)\n\
         assert(not ok and tostring(err):find('not an integer'), 'incr on non-int errors')\n\
         lur.kv.delete('{c}'); lur.kv.delete('{s}')"
    ))
    .expect("pg kv incr/decr + integer guard");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg -E 'test(pg_kv)' 2>&1 | tail -12`
Expected: FAIL with "postgres backend not yet implemented".

- [ ] **Step 3: Add the kv methods to `impl PgBackend` in `postgres.rs`**

```rust
    pub(crate) async fn kv_get(&self, lua: &Lua, key: String) -> mlua::Result<Value> {
        let row = sqlx::query("SELECT kind, bytes, num FROM lur_kv WHERE key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.get: {e}")))?;
        match row {
            None => Ok(Value::Nil),
            Some(r) => Ok(Value::String(lua.create_string(kv_row_to_bytes(&r)?)?)),
        }
    }

    pub(crate) async fn kv_set(&self, key: String, value: Vec<u8>) -> mlua::Result<()> {
        sqlx::query(
            "INSERT INTO lur_kv (key, kind, bytes, num) VALUES ($1, 0, $2, NULL) \
             ON CONFLICT (key) DO UPDATE SET kind = 0, bytes = excluded.bytes, num = NULL",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.set: {e}")))?;
        Ok(())
    }

    pub(crate) async fn kv_delete(&self, key: String) -> mlua::Result<()> {
        sqlx::query("DELETE FROM lur_kv WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::runtime(format!("lur.kv.delete: {e}")))?;
        Ok(())
    }

    pub(crate) async fn kv_add(&self, key: String, value: Vec<u8>) -> mlua::Result<bool> {
        let res = sqlx::query(
            "INSERT INTO lur_kv (key, kind, bytes) VALUES ($1, 0, $2) \
             ON CONFLICT (key) DO NOTHING",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("lur.kv.add: {e}")))?;
        Ok(res.rows_affected() == 1)
    }

    pub(crate) async fn kv_cas(
        &self,
        key: String,
        expected: Option<Vec<u8>>,
        new: Option<Vec<u8>>,
    ) -> mlua::Result<bool> {
        let applied = match (expected, new) {
            // expect absent, set new: insert iff absent
            (None, Some(v)) => {
                sqlx::query(
                    "INSERT INTO lur_kv (key, kind, bytes) VALUES ($1, 0, $2) \
                     ON CONFLICT (key) DO NOTHING",
                )
                .bind(key)
                .bind(v)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
            // expect absent, want absent: succeeds iff already absent
            (None, None) => {
                let r = sqlx::query("SELECT 1 FROM lur_kv WHERE key = $1")
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?;
                r.is_none()
            }
            // expect bytes value, set new
            (Some(e), Some(v)) => {
                sqlx::query(
                    "UPDATE lur_kv SET kind = 0, bytes = $1, num = NULL \
                     WHERE key = $2 AND kind = 0 AND bytes = $3",
                )
                .bind(v)
                .bind(key)
                .bind(e)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                .rows_affected()
                    == 1
            }
            // expect bytes value, delete
            (Some(e), None) => {
                sqlx::query("DELETE FROM lur_kv WHERE key = $1 AND kind = 0 AND bytes = $2")
                    .bind(key)
                    .bind(e)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.cas: {e}")))?
                    .rows_affected()
                    == 1
            }
        };
        Ok(applied)
    }

    /// Atomically add `delta` to an integer counter, creating it at `delta` when
    /// absent. The `WHERE kind = 1` guard on the conflict update returns no row
    /// when the key holds opaque bytes — the "not an integer" case.
    pub(crate) async fn kv_incr(
        &self,
        voice: &'static str,
        key: String,
        delta: i64,
    ) -> mlua::Result<i64> {
        let row = sqlx::query(
            "INSERT INTO lur_kv (key, kind, num) VALUES ($1, 1, $2) \
             ON CONFLICT (key) DO UPDATE SET num = lur_kv.num + excluded.num \
             WHERE lur_kv.kind = 1 \
             RETURNING num",
        )
        .bind(key)
        .bind(delta)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::runtime(format!("{voice}: {e}")))?;
        match row {
            Some(r) => r
                .try_get::<i64, usize>(0)
                .map_err(|e| Error::runtime(format!("{voice}: {e}"))),
            None => Err(Error::runtime(format!(
                "{voice}: existing value is not an integer"
            ))),
        }
    }
```

Add this free helper near `read_row` in `postgres.rs`:
```rust
/// Decode a `SELECT kind, bytes, num` row into the neutral value bytes: a `kind=1`
/// counter renders as its decimal string; `kind=0` yields the raw bytes (matching
/// SQLite's `value_to_bytes`).
fn kv_row_to_bytes(row: &PgRow) -> mlua::Result<Vec<u8>> {
    let kind: i16 = row
        .try_get::<i16, usize>(0)
        .map_err(|e| Error::runtime(format!("lur.kv: decoding kind: {e}")))?;
    if kind == 1 {
        let n: i64 = row
            .try_get::<i64, usize>(2)
            .map_err(|e| Error::runtime(format!("lur.kv: decoding counter: {e}")))?;
        Ok(n.to_string().into_bytes())
    } else {
        let b: Option<Vec<u8>> = row
            .try_get::<Option<Vec<u8>>, usize>(1)
            .map_err(|e| Error::runtime(format!("lur.kv: decoding value: {e}")))?;
        Ok(b.unwrap_or_default())
    }
}
```

- [ ] **Step 4: Replace the temporary kv arms in `mod.rs`**

For each of `kv_get`, `kv_set`, `kv_delete`, `kv_add`, `kv_cas`, `kv_incr`, replace the temporary `Backend::Postgres(_) => Err(..)` arm with a real delegation, matching the SQLite arm's call. Examples:
```rust
// kv_get
Backend::Postgres(b) => b.kv_get(lua, key).await,
// kv_set
Backend::Postgres(b) => b.kv_set(key, value).await,
// kv_delete
Backend::Postgres(b) => b.kv_delete(key).await,
// kv_add
Backend::Postgres(b) => b.kv_add(key, value).await,
// kv_cas
Backend::Postgres(b) => b.kv_cas(key, expected, new).await,
// kv_incr
Backend::Postgres(b) => b.kv_incr(voice, key, delta).await,
```
Leave the `kv_update` Postgres arm as the temporary error (Task 5).

- [ ] **Step 5: Run tests + fmt + clippy**

Run: `cd /Users/henry/Develop/claude/lur && cargo fmt --all && cargo clippy --all-targets -- -D warnings 2>&1 | tail -8`
Expected: clean.

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg 2>&1 | tail -14`
Expected: PASS including the two new kv tests.

- [ ] **Step 6: Commit**

```bash
git add src/capabilities/storage/postgres.rs src/capabilities/storage/mod.rs tests/pg.rs
git commit -m "feat(storage): Postgres kv get/set/delete/add/cas/incr"
```

---

### Task 5: Postgres kv.update (SERIALIZABLE read-modify-write)

**Files:**
- Modify: `src/capabilities/storage/postgres.rs`
- Modify: `src/capabilities/storage/mod.rs`
- Modify: `tests/pg.rs`

**Interfaces:**
- Produces on `PgBackend`: `kv_update(&Lua, String, Function) -> mlua::Result<Value>`.
- Consumes: `kv_row_to_bytes` (Task 4). The `kv.rs` `IN_KV_UPDATE` guard wrapper is unchanged — `kv_update` receives the already-wrapped `func`.

- [ ] **Step 1: Write the failing tests (RMW + cas-compatible bytes)**

Add to `tests/pg.rs`:

```rust
#[test]
fn pg_kv_update_read_modify_write() {
    let Some(rt) = pg_runtime() else { return };
    let k = unique("pgupd");
    rt.run(&format!(
        "lur.kv.set('{k}', 'a')\n\
         local out = lur.kv.update('{k}', function(cur)\n\
           assert(cur == 'a', 'sees current')\n\
           return cur .. 'b'\n\
         end)\n\
         assert(out == 'ab', 'returns new')\n\
         assert(lur.kv.get('{k}') == 'ab', 'persisted')\n\
         lur.kv.update('{k}', function(_) return nil end)\n\
         assert(lur.kv.get('{k}') == nil, 'nil deletes')\n\
         local seen\n\
         lur.kv.update('{k}', function(cur) seen = cur; return 'fresh' end)\n\
         assert(seen == nil, 'absent key sees nil')\n\
         assert(lur.kv.get('{k}') == 'fresh', 'created')\n\
         lur.kv.delete('{k}')"
    ))
    .expect("pg kv.update RMW");
}

#[test]
fn pg_kv_update_writes_bytes_that_cas_can_match() {
    let Some(rt) = pg_runtime() else { return };
    let k = unique("pgupdcas");
    rt.run(&format!(
        "lur.kv.set('{k}', 'x')\n\
         lur.kv.update('{k}', function(_) return 'y' end)\n\
         assert(lur.kv.cas('{k}', 'y', 'z') == true, 'update value matches cas')\n\
         lur.kv.delete('{k}')"
    ))
    .expect("pg kv.update writes cas-comparable bytes");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg -E 'test(pg_kv_update)' 2>&1 | tail -12`
Expected: FAIL with "postgres backend not yet implemented".

- [ ] **Step 3: Add `kv_update` to `impl PgBackend` in `postgres.rs`**

```rust
    /// Read-modify-write for `lur.kv.update`: a `SERIALIZABLE` transaction on a
    /// pinned connection — read (type-aware, matching `kv_get`), call `func`, then
    /// write/delete and commit; roll back and re-raise on any error. A conflicting
    /// concurrent writer aborts this with SQLSTATE 40001 (surfaced, not retried).
    /// The write stores the returned string as `kind=0` bytes so a value written by
    /// `update` compares equal under `kv_cas` (which matches on `bytes`).
    pub(crate) async fn kv_update(
        &self,
        lua: &Lua,
        key: String,
        func: Function,
    ) -> mlua::Result<Value> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;
        sqlx::query("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut *conn)
            .await
            .map_err(|e| Error::runtime(format!("lur.db.tx: begin: {e}")))?;

        let result: mlua::Result<Value> = async {
            let cur: Value = match sqlx::query("SELECT kind, bytes, num FROM lur_kv WHERE key = $1")
                .bind(&key)
                .fetch_optional(&mut *conn)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?
            {
                None => Value::Nil,
                Some(r) => Value::String(lua.create_string(kv_row_to_bytes(&r)?)?),
            };

            let new = func.call_async::<Value>(cur).await?;

            match &new {
                Value::Nil => {
                    sqlx::query("DELETE FROM lur_kv WHERE key = $1")
                        .bind(&key)
                        .execute(&mut *conn)
                        .await
                        .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                Value::String(s) => {
                    sqlx::query(
                        "INSERT INTO lur_kv (key, kind, bytes, num) VALUES ($1, 0, $2, NULL) \
                         ON CONFLICT (key) DO UPDATE SET kind = 0, bytes = excluded.bytes, num = NULL",
                    )
                    .bind(&key)
                    .bind(s.as_bytes().to_vec())
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| Error::runtime(format!("lur.kv.update: {e}")))?;
                }
                other => {
                    return Err(Error::runtime(format!(
                        "lur.kv.update: transform must return a string or nil, got {}",
                        other.type_name()
                    )));
                }
            }
            sqlx::query("COMMIT")
                .execute(&mut *conn)
                .await
                .map_err(|e| Error::runtime(format!("lur.kv.update: commit: {e}")))?;
            Ok(new)
        }
        .await;

        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                Err(e)
            }
        }
    }
```

- [ ] **Step 4: Replace the temporary `kv_update` arm in `mod.rs`**

```rust
match self {
    Backend::Sqlite(b) => b.kv_update(lua, key, func).await,
    Backend::Postgres(b) => b.kv_update(lua, key, func).await,
}
```

- [ ] **Step 5: Run tests + fmt + clippy**

Run: `cd /Users/henry/Develop/claude/lur && cargo fmt --all && cargo clippy --all-targets -- -D warnings 2>&1 | tail -8`
Expected: clean (no temporary-error arms remain).

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg 2>&1 | tail -16`
Expected: PASS including both `pg_kv_update_*` tests.

- [ ] **Step 6: Commit**

```bash
git add src/capabilities/storage/postgres.rs src/capabilities/storage/mod.rs tests/pg.rs
git commit -m "feat(storage): Postgres kv.update (SERIALIZABLE read-modify-write)"
```

---

### Task 6: PostgreSQL-specific correctness (SERIALIZABLE 40001 is fallible & catchable)

**Files:**
- Modify: `tests/pg.rs`

**Interfaces:**
- Consumes: all `PgBackend` behavior from Tasks 2–5. No `src` change expected; this task proves the fallible-transactional-API contract deterministically.

- [ ] **Step 1: Write the test (concurrent SERIALIZABLE workload stays catchable and consistent)**

Two threads run crosswise read-then-write SERIALIZABLE transactions in a loop. Each wraps `db.tx` in `pcall`, so any `40001` abort is caught inside the script and never escapes as a fatal error. The firm, non-flaky assertions are: (1) `pcall` is always a valid guard — `rt.run` on a `pcall`-wrapped `db.tx` never propagates; (2) after the conflict storm the table is intact (atomicity held through any aborts). This proves the fallible-contract without a timing-dependent "a conflict must fire this run" assertion. Add to `tests/pg.rs`:

```rust
#[test]
fn pg_serializable_tx_conflict_is_fallible_and_catchable() {
    let Some(seed) = pg_runtime() else { return };
    let t = unique("pgssi");
    seed.run(&format!(
        "lur.db.exec('DROP TABLE IF EXISTS {t}')\n\
         lur.db.exec('CREATE TABLE {t} (id INT PRIMARY KEY, v INT)')\n\
         lur.db.exec('INSERT INTO {t} VALUES (1,0),(2,0)')"
    ))
    .expect("seed");

    // Each thread: crosswise read-then-write serializable tx, pcall-guarded.
    let spawn = |read_id: i64, write_id: i64, table: String| {
        std::thread::spawn(move || {
            let rt = pg_runtime().expect("worker runtime");
            for _ in 0..40 {
                // `return pcall(...)`: a 40001 abort is caught inside the script,
                // so rt.run itself must always succeed — proving catchability.
                rt.run(&format!(
                    "return pcall(function()\n\
                       lur.db.tx(function(tx)\n\
                         local o = tx.query('SELECT v FROM {table} WHERE id = {read_id}')[1].v\n\
                         tx.exec('UPDATE {table} SET v = ' .. (o + 1) .. ' WHERE id = {write_id}')\n\
                       end)\n\
                     end)"
                ))
                .expect("script with its own pcall never propagates a fatal error");
            }
        })
    };
    let h1 = spawn(2, 1, t.clone());
    let h2 = spawn(1, 2, t.clone());
    h1.join().unwrap();
    h2.join().unwrap();

    // Runtime + data intact after the conflict storm (atomicity held through aborts).
    seed.run(&format!(
        "assert(#lur.db.query('SELECT id FROM {t}') == 2, 'table intact')\n\
         lur.db.exec('DROP TABLE {t}')"
    ))
    .expect("healthy after concurrent serializable conflicts");
}
```

> Implementer note: SSI conflict *timing* on shared CI runners is not deterministic, so this test does not assert "an abort fired this run" (that would flake). The binding assertions are catchability (`pcall` always guards `db.tx`) and post-storm consistency. If you can force a conflict deterministically (e.g. two Rust-driven pinned connections with an explicit read-barrier-write interleave), you may additionally assert an abort was observed — but never assert both transactions commit.

- [ ] **Step 2: Run to verify it fails / passes**

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test pg -E 'test(pg_tx_serialization_conflict_is_catchable)' 2>&1 | tail -12`
Expected: PASS (the catchability contract holds). If it fails, the failure is a real signal that `db.tx` does not propagate/catch cleanly — fix the backend, not the test.

- [ ] **Step 3: Full PG suite + fmt + clippy**

Run: `cd /Users/henry/Develop/claude/lur && cargo fmt --all && cargo clippy --all-targets -- -D warnings 2>&1 | tail -6 && cargo nextest run --test pg 2>&1 | tail -18`
Expected: all PG tests pass.

- [ ] **Step 4: Commit**

```bash
git add tests/pg.rs
git commit -m "test(storage): Postgres SERIALIZABLE db.tx is fallible and pcall-catchable"
```

---

### Task 7: Documentation

**Files:**
- Modify: `README.md`
- Modify: `ARCHITECTURE.md`
- Modify: `docs/GUIDE.md`

**Interfaces:**
- Consumes: the shipped PG behavior. `tests/guide.rs` drift guards must stay green (every `lur.*` function exercised in `docs/GUIDE.md` still is).

- [ ] **Step 1: Locate the storage docs to extend**

Run: `cd /Users/henry/Develop/claude/lur && grep -n "lur.db\|lur.kv\|--db\|busy_timeout\|BEGIN IMMEDIATE\|SQLite" README.md ARCHITECTURE.md docs/GUIDE.md | head -50`
Read the surrounding sections so the additions match the existing voice and structure.

- [ ] **Step 2: Update `README.md`**

In the `lur.db` / `lur.kv` / `--db` sections, add (matching the existing prose style):
- `--db` accepts either a SQLite file path **or** a `postgres://` / `postgresql://` connection string; the scheme selects the backend at first use.
- Native placeholders per backend: `?` for SQLite, `$1, $2, …` for Postgres (no translation).
- Postgres row types: core types (int/float/text/varchar/bytea) map to Lua integer/number/string; **a non-core column raises an error asking you to cast it (`col::text`)**.
- `db.exec(...).last_insert_id` is SQLite-only (`0` on Postgres); use `db.query("INSERT … RETURNING id")` on Postgres.
- TLS via `sslmode` in the URL (e.g. `?sslmode=require`).
- The **fallible transactional-API contract**: `db.tx` and `kv.update` may raise a transient error (on Postgres, a SERIALIZABLE `40001` conflict); wrap them in `pcall` or your own retry — the example:

```lua
local ok, err = pcall(function()
  return lur.db.tx(function(tx) --[[ … ]] end)
end)
```

- Running the Postgres tests locally needs `docker compose up -d` first.

- [ ] **Step 3: Update `ARCHITECTURE.md`**

In the storage-seam section, add:
- the `Postgres` backend behind `enum Backend`/`enum Transaction` (`storage/postgres.rs`), selected by `--db` scheme;
- the isolation model: single-statement ops at `READ COMMITTED` (atomic, no retry); `db.tx` / `kv.update` at `SERIALIZABLE` and **documented as fallible** (40001 surfaces, no auto-retry because bodies may have side effects); the `retry_busy` layer stays SQLite-only;
- the operator-trusted connection: exempt from the script-facing net allowlist / SSRF guard, the way the SQLite file path is exempt from `lur.fs`;
- the `kind`-discriminated PG kv schema (`kind` 0 = bytes, 1 = counter) mapping the neutral value model 1:1.

- [ ] **Step 4: Update `docs/GUIDE.md`**

Extend the `lur.db` / `lur.kv` cookbook with a short Postgres example: a `--db postgres://…` invocation, a `$1`-placeholder `db.exec`/`query`, and a `pcall`-wrapped `db.tx`. Do not remove any existing example (the drift guard reflects the live `lur` table).

- [ ] **Step 5: Verify the guide still runs and drift guards pass**

Run: `cd /Users/henry/Develop/claude/lur && cargo nextest run --test guide 2>&1 | tail -12`
Expected: PASS (guide examples execute; capability- and function-level drift guards green).

- [ ] **Step 6: Commit**

```bash
git add README.md ARCHITECTURE.md docs/GUIDE.md
git commit -m "docs(storage): document the PostgreSQL backend and fallible tx contract"
```

---

## Final verification (after all tasks)

Run the whole gate exactly as CI does:

```bash
cd /Users/henry/Develop/claude/lur
docker compose up -d
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo nextest run
cargo deny check
```

Expected: all green, with `tests/pg.rs` exercised against the compose Postgres and every existing SQLite test passing unchanged.
