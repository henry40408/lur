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

#[test]
fn kv_add_and_cas() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "assert(lur.kv.add('k', 'first') == true, 'add inserts when absent')\n\
         assert(lur.kv.add('k', 'second') == false, 'add is a no-op when present')\n\
         assert(lur.kv.get('k') == 'first', 'value kept from first add')\n\
         -- cas update-if-equal\n\
         assert(lur.kv.cas('k', 'first', 'next') == true, 'cas applies on match')\n\
         assert(lur.kv.cas('k', 'first', 'nope') == false, 'cas rejects on mismatch')\n\
         assert(lur.kv.get('k') == 'next', 'value is the cas result')\n\
         -- cas set-if-absent (expected = nil)\n\
         assert(lur.kv.cas('fresh', nil, 'v') == true, 'cas(nil,...) sets when absent')\n\
         assert(lur.kv.cas('fresh', nil, 'v2') == false, 'cas(nil,...) fails when present')\n\
         -- cas delete-if-equal (new = nil)\n\
         assert(lur.kv.cas('fresh', 'v', nil) == true, 'cas(...,nil) deletes on match')\n\
         assert(lur.kv.get('fresh') == nil, 'deleted')",
    )
    .expect("kv add + cas");
}

#[test]
fn kv_update_read_modify_write() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "-- create via update (current is nil)\n\
         local v = lur.kv.update('k', function(cur)\n\
           assert(cur == nil, 'absent starts nil')\n\
           return 'a'\n\
         end)\n\
         assert(v == 'a', 'update returns the new value')\n\
         -- transform existing\n\
         lur.kv.update('k', function(cur) return cur .. 'b' end)\n\
         assert(lur.kv.get('k') == 'ab', 'appended')\n\
         -- delete by returning nil\n\
         local d = lur.kv.update('k', function(_) return nil end)\n\
         assert(d == nil and lur.kv.get('k') == nil, 'nil deletes')\n\
         -- re-entry from inside the transform errors\n\
         local ok, err = pcall(function()\n\
           lur.kv.update('k', function(_) lur.kv.set('x', 'y'); return '1' end)\n\
         end)\n\
         assert(ok == false and tostring(err):find('re%-enter'), 'reentry blocked: ' .. tostring(err))",
    )
    .expect("kv update RMW + reentry guard");
}

#[test]
fn kv_update_no_leaked_transaction_after_transform_error() {
    // A transform that errors must not leave an open (IMMEDIATE) transaction on
    // the pooled connection.  If it did, the next update would deadlock or fail
    // because SQLite only permits one writer at a time.
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "local ok, err = pcall(function()\n\
           lur.kv.update('z', function(_) error('boom') end)\n\
         end)\n\
         assert(ok == false, 'expected error from bad transform: ' .. tostring(err))\n\
         local v = lur.kv.update('z2', function(_) return 'ok' end)\n\
         assert(v == 'ok', 'a fresh update works after a transform error (no leaked transaction): got ' .. tostring(v))",
    )
    .expect("no leaked transaction after transform error");
}

#[test]
fn kv_incr_decr_counters() {
    let dir = tempfile::tempdir().unwrap();
    let rt = db_runtime(dir.path().join("test.db"));
    rt.run(
        "assert(lur.kv.incr('hits') == 1, 'first incr creates at 1')\n\
         assert(lur.kv.incr('hits', 4) == 5, 'incr by 4')\n\
         assert(lur.kv.decr('hits', 2) == 3, 'decr by 2')\n\
         assert(lur.kv.get('hits') == '3', 'counter reads back as decimal bytes')\n\
         -- incr on a non-integer value errors and leaves it intact\n\
         lur.kv.set('blob', 'hello')\n\
         local ok, err = pcall(function() return lur.kv.incr('blob') end)\n\
         assert(ok == false, 'incr on a blob errors')\n\
         assert(tostring(err):find('not an integer'), 'clear message: ' .. tostring(err))\n\
         assert(lur.kv.get('blob') == 'hello', 'blob untouched after failed incr')\n\
         -- decr on a non-integer value errors with decr voicing (not incr)\n\
         lur.kv.set('blob2', 'x')\n\
         local ok2, err2 = pcall(function() return lur.kv.decr('blob2') end)\n\
         assert(ok2 == false, 'decr on a blob errors')\n\
         assert(tostring(err2):find('lur.kv.decr'), 'decr error is voiced as decr: ' .. tostring(err2))",
    )
    .expect("kv incr/decr");
}
