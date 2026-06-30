use std::path::PathBuf;

use lur::runtime::{Runtime, RuntimeConfig};

fn db_runtime(path: PathBuf) -> Runtime {
    Runtime::with_config(RuntimeConfig {
        db_path: Some(path),
        ..Default::default()
    })
    .expect("runtime builds")
}

#[test]
fn db_exec_creates_table_and_inserts() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)')\n\
         local r = lur.db.exec('INSERT INTO t(name) VALUES (?)', 'alice')\n\
         assert(r.rows_affected == 1, 'rows_affected')\n\
         assert(r.last_insert_id == 1, 'last_insert_id')",
    )
    .expect("exec works");
}

#[test]
fn db_query_returns_rows_with_type_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec('CREATE TABLE t (i INTEGER, r REAL, s TEXT, n TEXT)')\n\
         lur.db.exec('INSERT INTO t VALUES (?, ?, ?, ?)', 42, 3.5, 'hi', lur.null)\n\
         local rows = lur.db.query('SELECT i, r, s, n FROM t')\n\
         assert(#rows == 1, 'one row')\n\
         local row = rows[1]\n\
         assert(row.i == 42, 'integer')\n\
         assert(row.r == 3.5, 'real')\n\
         assert(row.s == 'hi', 'text')\n\
         assert(row.n == lur.null, 'null maps to lur.null')",
    )
    .expect("query + type mapping");
}

#[test]
fn db_boolean_and_binary_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec('CREATE TABLE b (flag INTEGER, data BLOB)')\n\
         lur.db.exec('INSERT INTO b VALUES (?, ?)', true, '\\0\\255bin')\n\
         local row = lur.db.query('SELECT flag, data FROM b')[1]\n\
         assert(row.flag == 1, 'boolean stored as 1')\n\
         assert(row.data == '\\0\\255bin', 'binary blob round-trips')",
    )
    .expect("boolean + binary round-trip");
}

#[test]
fn kv_set_get_delete_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "assert(lur.kv.get('missing') == nil, 'miss is nil')\n\
         lur.kv.set('k', 'value-bytes')\n\
         assert(lur.kv.get('k') == 'value-bytes', 'get after set')\n\
         lur.kv.set('k', 'updated')\n\
         assert(lur.kv.get('k') == 'updated', 'overwrite')\n\
         lur.kv.delete('k')\n\
         assert(lur.kv.get('k') == nil, 'gone after delete')",
    )
    .expect("kv round-trip");
}

#[test]
fn tx_commits_on_normal_return() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec('CREATE TABLE acct (id INTEGER PRIMARY KEY, bal INTEGER)')\n\
         lur.db.exec('INSERT INTO acct VALUES (1, 100)')\n\
         lur.db.exec('INSERT INTO acct VALUES (2, 0)')\n\
         lur.db.tx(function(tx)\n\
           tx.exec('UPDATE acct SET bal = bal - 50 WHERE id = 1')\n\
           tx.exec('UPDATE acct SET bal = bal + 50 WHERE id = 2')\n\
         end)\n\
         local a = lur.db.query('SELECT bal FROM acct WHERE id = 1')[1].bal\n\
         local b = lur.db.query('SELECT bal FROM acct WHERE id = 2')[1].bal\n\
         assert(a == 50, 'id1 = ' .. a)\n\
         assert(b == 50, 'id2 = ' .. b)",
    )
    .expect("tx commits on return");
}

#[test]
fn tx_rolls_back_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec('CREATE TABLE acct (id INTEGER PRIMARY KEY, bal INTEGER)')\n\
         lur.db.exec('INSERT INTO acct VALUES (1, 100)')\n\
         local ok = pcall(function()\n\
           lur.db.tx(function(tx)\n\
             tx.exec('UPDATE acct SET bal = 999 WHERE id = 1')\n\
             error('boom')\n\
           end)\n\
         end)\n\
         assert(ok == false, 'tx must propagate the error')\n\
         local a = lur.db.query('SELECT bal FROM acct WHERE id = 1')[1].bal\n\
         assert(a == 100, 'must be rolled back, got ' .. a)",
    )
    .expect("tx rolls back on error");
}

#[test]
fn db_without_a_path_errors() {
    let rt = Runtime::new().expect("runtime builds"); // db_path is None
    assert!(
        rt.run("lur.db.exec('SELECT 1')").is_err(),
        "using lur.db without --db must error"
    );
}

#[test]
fn tx_uses_a_write_lock_and_still_commits_and_rolls_back() {
    // Smoke test that the BEGIN IMMEDIATE rewrite preserves tx semantics:
    // a committing tx persists, an erroring tx rolls back, on the same db.
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)')\n\
         lur.db.exec('INSERT INTO t VALUES (1, 0)')\n\
         lur.db.tx(function(tx) tx.exec('UPDATE t SET n = 5 WHERE id = 1') end)\n\
         assert(lur.db.query('SELECT n FROM t WHERE id=1')[1].n == 5, 'committed')\n\
         pcall(function()\n\
           lur.db.tx(function(tx)\n\
             tx.exec('UPDATE t SET n = 99 WHERE id = 1')\n\
             error('boom')\n\
           end)\n\
         end)\n\
         assert(lur.db.query('SELECT n FROM t WHERE id=1')[1].n == 5, 'rolled back')",
    )
    .expect("tx commit + rollback under IMMEDIATE");
}

#[test]
fn kv_get_reads_an_integer_cell_as_decimal_bytes() {
    // A counter (INTEGER affinity) written into lur_kv must read back through
    // kv.get as its decimal-string bytes, not crash on a Vec<u8> type mismatch.
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "lur.db.exec(\"INSERT INTO lur_kv(key,value) VALUES('c', 42)\")\n\
         assert(lur.kv.get('c') == '42', 'integer cell reads as \"42\"')\n\
         lur.kv.set('b', 'raw')\n\
         assert(lur.kv.get('b') == 'raw', 'blob cell still reads raw bytes')\n\
         assert(lur.kv.get('missing') == nil, 'absent is nil')",
    )
    .expect("type-aware kv.get");
}
