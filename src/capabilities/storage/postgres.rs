//! `PostgreSQL` storage backend: owns the `sqlx` `PgPool` and all PG-specific SQL,
//! `$n` binding, row→Lua mapping (core types only; non-core must be cast to text),
//! isolation, and the `kind`-discriminated kv schema. No retry layer — under
//! `READ COMMITTED` single statements block rather than surfacing a busy error,
//! and the `SERIALIZABLE` transactional APIs are documented as fallible.

use std::str::FromStr;

use mlua::{Error, Lua, Table, Value};
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
