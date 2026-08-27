use lur::runtime::{Runtime, RuntimeConfig};

/// The Postgres URL for tests: `LUR_TEST_PG_URL` or the docker-compose default.
fn pg_test_url() -> String {
    std::env::var("LUR_TEST_PG_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".to_string())
}

/// A `RuntimeConfig` pointed at Postgres, or `None` when the server is
/// unreachable. Locally an unreachable server SKIPS the test (returns None);
/// under CI it is a hard failure (panics), because CI provisions the service.
/// Building the config once and `.clone()`-ing it (rather than calling this
/// per runtime) is required whenever two runtimes must share `lur.state`: the
/// `Arc<StateStore>` behind it lives on the config, and `RuntimeConfig::default`
/// mints a fresh store on every call.
fn pg_config() -> Option<RuntimeConfig> {
    let url = pg_test_url();
    let reachable = reachable(&url);
    if !reachable {
        assert!(
            std::env::var("CI").is_err(),
            "CI: Postgres at {url} is unreachable but CI must provision it"
        );
        eprintln!("skipping PG test: {url} unreachable (start it: docker compose up -d)");
        return None;
    }
    Some(RuntimeConfig {
        db_path: Some(std::path::PathBuf::from(url)),
        ..Default::default()
    })
}

/// A runtime pointed at Postgres, or `None` when the server is unreachable.
fn pg_runtime() -> Option<Runtime> {
    let config = pg_config()?;
    Some(Runtime::with_config(config).expect("runtime builds"))
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
    addrs
        .any(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok())
}

/// The locale-independent tail `postgres.rs::map_pg_error` appends for a
/// SQLSTATE 40001 serialization failure. Identical at every call site
/// (in-transaction statement or COMMIT), so a retry loop — and these tests —
/// can match on it regardless of which site the abort lands at.
const SERIALIZATION_FAILURE_TAIL: &str = "serialization failure (SQLSTATE 40001): concurrent transaction conflict, retry the transaction";

/// A fresh, uniquely-named table per test so parallel tests don't collide on the
/// shared database. Caller drops it via `DROP TABLE IF EXISTS`.
fn unique(prefix: &str) -> String {
    // A process-local counter, so neither the clock nor an RNG is involved.
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
    rt.run(&format!(
        "lur.db.exec('DROP TABLE IF EXISTS {t}')\n\
        lur.db.exec('CREATE TABLE {t} (j JSONB)')\n\
        lur.db.exec([[INSERT INTO {t} VALUES ('{{\"a\":1}}')]])"
    ))
    .expect("setup jsonb table");
    // Reading jsonb directly errors with the cast-to-text guidance.
    let err = rt
        .run(&format!("lur.db.query('SELECT j FROM {t}')"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unsupported column type") && err.contains("::text"),
        "got: {err}"
    );
    // Casting to text succeeds.
    rt.run(&format!(
        "local rows = lur.db.query('SELECT j::text AS j FROM {t}')\n\
         assert(rows[1].j:find('\"a\"'), 'jsonb text form')\n\
         lur.db.exec('DROP TABLE {t}')"
    ))
    .expect("cast-to-text read works");
}

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

/// Deterministically forces an SSI write-skew conflict (classic crosswise
/// read-then-write) and asserts the abort actually fires, is voiced with the
/// stable 40001 tail, and leaves the table intact. Complementary to
/// `pg_serializable_tx_conflict_is_fallible_and_catchable` above, which proves
/// no-fatal-under-stress + data integrity across many rounds but never proves
/// a conflict actually happened. Here a `lur.state` barrier (process-local,
/// synchronous, outside the SERIALIZABLE read/write set) rendezvouses both
/// transactions between their SELECT and UPDATE so SSI is guaranteed to see
/// the rw-antidependency cycle both ways.
///
/// The abort site splits roughly 50/50 between COMMIT and the in-transaction
/// UPDATE (measured over repeated runs), so the assertion below matches the
/// stable 40001 tail rather than the context prefix or Postgres's own prose.
#[test]
fn pg_ssi_write_skew_aborts_one_side_with_stable_40001_message() {
    let Some(config) = pg_config() else { return };
    let seed = Runtime::with_config(config.clone()).expect("runtime builds");

    let t = unique("pgwriteskew");
    let barrier = unique("pgwriteskew_arrived");
    let out1 = unique("pgwriteskew_out1");
    let out2 = unique("pgwriteskew_out2");

    seed.run(&format!(
        "lur.db.exec('DROP TABLE IF EXISTS {t}')\n\
         lur.db.exec('CREATE TABLE {t} (id INT PRIMARY KEY, v INT)')\n\
         lur.db.exec('INSERT INTO {t} VALUES (1,0),(2,0)')"
    ))
    .expect("seed");

    // Each thread reads the OTHER row and writes its OWN row, rendezvousing
    // between the two so both reads are guaranteed to precede either write —
    // exactly the write-skew shape SSI must detect and abort one side of.
    let spawn = |read_id: i64, write_id: i64, table: String, barrier: String, out_key: String| {
        let cfg = config.clone();
        std::thread::spawn(move || {
            let rt = Runtime::with_config(cfg).expect("runtime builds");
            rt.run(&format!(
                "local ok, err = pcall(function()\n\
                   lur.db.tx(function(tx)\n\
                     local cur = tx.query('SELECT v FROM {table} WHERE id = {read_id}')[1].v\n\
                     lur.state.incr('{barrier}')\n\
                     local deadline = lur.time.monotonic_ms() + 10000\n\
                     while tonumber(lur.state.get('{barrier}')) < 2 do\n\
                       if lur.time.monotonic_ms() > deadline then error('barrier timeout') end\n\
                       lur.async.sleep(2)\n\
                     end\n\
                     tx.exec('UPDATE {table} SET v = ' .. (cur + 1) .. ' WHERE id = {write_id}')\n\
                   end)\n\
                 end)\n\
                 lur.kv.set('{out_key}', ok and 'ok' or tostring(err))"
            ))
            .expect("script never propagates: outcome is stashed via lur.kv, not pcall's return");
        })
    };
    let h1 = spawn(2, 1, t.clone(), barrier.clone(), out1.clone());
    let h2 = spawn(1, 2, t.clone(), barrier.clone(), out2.clone());
    h1.join().unwrap();
    h2.join().unwrap();

    seed.run(&format!(
        "local o1 = lur.kv.get('{out1}')\n\
         local o2 = lur.kv.get('{out2}')\n\
         local ok_count = (o1 == 'ok' and 1 or 0) + (o2 == 'ok' and 1 or 0)\n\
         assert(ok_count == 1, 'exactly one side must succeed, got o1=' .. tostring(o1) .. ' o2=' .. tostring(o2))\n\
         local failure = (o1 == 'ok') and o2 or o1\n\
         assert(failure ~= 'ok' and failure ~= nil, 'the other side must have failed')\n\
         assert(\n\
           failure:find('{SERIALIZATION_FAILURE_TAIL}', 1, true),\n\
           'failure must carry the stable 40001 tail, got: ' .. failure\n\
         )\n\
         assert(#lur.db.query('SELECT id FROM {t}') == 2, 'table intact after the aborted side')\n\
         lur.kv.delete('{out1}')\n\
         lur.kv.delete('{out2}')\n\
         lur.db.exec('DROP TABLE {t}')"
    ))
    .expect("exactly one write-skew side aborts with the stable 40001 message");
}

/// Guards against an over-broad refactor mapping every Postgres error to the
/// serialization-failure message: a primary-key violation inside `db.tx`
/// (SQLSTATE 23505, not 40001) must NOT be voiced as a serialization failure
/// and must still carry the driver's own text.
#[test]
fn pg_tx_non_serialization_error_keeps_driver_text() {
    let Some(rt) = pg_runtime() else { return };
    let t = unique("pg_negerr");
    rt.run(&format!(
        "lur.db.exec('DROP TABLE IF EXISTS {t}')\n\
         lur.db.exec('CREATE TABLE {t} (id INT PRIMARY KEY)')\n\
         lur.db.exec('INSERT INTO {t} VALUES (1)')"
    ))
    .expect("seed");

    let err = rt
        .run(&format!(
            "lur.db.tx(function(tx)\n\
               tx.exec('INSERT INTO {t} VALUES (1)')\n\
             end)"
        ))
        .unwrap_err()
        .to_string();
    assert!(
        !err.contains(SERIALIZATION_FAILURE_TAIL),
        "a non-40001 error must not be voiced as a serialization failure: {err}"
    );
    // Assert the shape, not the driver's wording: the wording is itself
    // localized by `lc_messages` — depending on it here would reintroduce
    // exactly the fragility this voicing removes.
    let (_, driver_text) = err
        .split_once("lur.db.tx exec: ")
        .unwrap_or_else(|| panic!("must keep the site's context prefix: {err}"));
    assert!(
        !driver_text.trim().is_empty(),
        "must pass the driver's own text through after the prefix: {err}"
    );

    rt.run(&format!("lur.db.exec('DROP TABLE {t}')"))
        .expect("cleanup");
}
