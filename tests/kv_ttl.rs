//! `lur.kv` expiry: `ttl_ms`, `renew_ttl`, `expire`, `ttl`, and the schema upgrade.

use std::path::PathBuf;

use lur::runtime::{Runtime, RuntimeConfig};

fn runtime(path: PathBuf) -> Runtime {
    Runtime::with_config(RuntimeConfig {
        db_path: Some(path),
        ..Default::default()
    })
    .expect("runtime builds")
}

fn run(script: &str) {
    let dir = tempfile::tempdir().unwrap();
    runtime(dir.path().join("ttl.db"))
        .run(script)
        .unwrap_or_else(|e| panic!("script failed: {e}"));
}

fn run_err(script: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    runtime(dir.path().join("ttl.db"))
        .run(script)
        .expect_err("script should fail")
        .to_string()
}

#[test]
fn set_with_ttl_expires() {
    run("lur.kv.set('k', 'v', { ttl_ms = 100 })\n\
         assert(lur.kv.get('k') == 'v', 'live before expiry')\n\
         lur.async.sleep(250)\n\
         assert(lur.kv.get('k') == nil, 'gone after expiry')\n\
         local ms, exists = lur.kv.ttl('k')\n\
         assert(ms == nil and exists == false, 'expired reads as absent')");
}

#[test]
fn set_without_ttl_clears_the_expiry() {
    run("lur.kv.set('k', 'v', { ttl_ms = 100 })\n\
         lur.kv.set('k', 'w')\n\
         lur.async.sleep(250)\n\
         assert(lur.kv.get('k') == 'w', 'plain set makes the key permanent')\n\
         local ms, exists = lur.kv.ttl('k')\n\
         assert(ms == nil and exists == true, 'no expiry')");
}

#[test]
fn ttl_reports_remaining_milliseconds() {
    run("local ms, exists = lur.kv.ttl('missing')\n\
         assert(ms == nil and exists == false, 'absent')\n\
         lur.kv.set('p', 'v')\n\
         ms, exists = lur.kv.ttl('p')\n\
         assert(ms == nil and exists == true, 'permanent')\n\
         lur.kv.set('t', 'v', { ttl_ms = 5000 })\n\
         ms, exists = lur.kv.ttl('t')\n\
         assert(exists == true and ms > 0 and ms <= 5000, 'remaining ' .. tostring(ms))");
}

#[test]
fn add_succeeds_over_an_expired_key_only() {
    run("assert(lur.kv.add('k', 'a', { ttl_ms = 100 }) == true)\n\
         assert(lur.kv.add('k', 'b') == false, 'live key blocks add')\n\
         lur.async.sleep(250)\n\
         assert(lur.kv.add('k', 'c') == true, 'expired key is free')\n\
         assert(lur.kv.get('k') == 'c')");
}

#[test]
fn incr_ttl_is_a_fixed_window() {
    // The window opens at the first hit and later hits don't extend it.
    run("assert(lur.kv.incr('c', 1, { ttl_ms = 1500 }) == 1)\n\
         lur.async.sleep(600)\n\
         assert(lur.kv.incr('c', 1, { ttl_ms = 1500 }) == 2)\n\
         lur.async.sleep(1100)\n\
         assert(lur.kv.incr('c', 1, { ttl_ms = 1500 }) == 1, 'window elapsed, restarts')");
}

#[test]
fn incr_renew_ttl_slides_the_window() {
    run(
        "assert(lur.kv.incr('c', 1, { ttl_ms = 1000, renew_ttl = true }) == 1)\n\
         lur.async.sleep(500)\n\
         assert(lur.kv.incr('c', 1, { ttl_ms = 1000, renew_ttl = true }) == 2)\n\
         lur.async.sleep(500)\n\
         assert(lur.kv.incr('c', 1, { ttl_ms = 1000, renew_ttl = true }) == 3, 'renewed')",
    );
}

#[test]
fn decr_takes_the_same_options() {
    run("assert(lur.kv.decr('c', 2, { ttl_ms = 5000 }) == -2)\n\
         local ms = lur.kv.ttl('c')\n\
         assert(ms ~= nil and ms > 0)");
}

#[test]
fn incr_adds_an_expiry_to_a_counter_that_has_none() {
    run("assert(lur.kv.incr('c') == 1)\n\
         local _, exists = lur.kv.ttl('c')\n\
         assert(exists == true)\n\
         assert(lur.kv.incr('c', 1, { ttl_ms = 5000 }) == 2, 'value carries over')\n\
         local ms = lur.kv.ttl('c')\n\
         assert(ms ~= nil and ms > 0, 'expiry healed in')");
}

#[test]
fn incr_without_options_leaves_the_expiry_alone() {
    run("lur.kv.incr('c', 1, { ttl_ms = 5000 })\n\
         lur.kv.incr('c')\n\
         local ms = lur.kv.ttl('c')\n\
         assert(ms ~= nil and ms > 0 and ms <= 5000)");
}

#[test]
fn cas_and_update_keep_the_expiry_unless_told() {
    run("lur.kv.set('k', 'a', { ttl_ms = 5000 })\n\
         assert(lur.kv.cas('k', 'a', 'b') == true)\n\
         local ms = lur.kv.ttl('k')\n\
         assert(ms ~= nil and ms <= 5000, 'cas keeps it')\n\
         lur.kv.update('k', function(cur) return cur .. 'c' end)\n\
         ms = lur.kv.ttl('k')\n\
         assert(lur.kv.get('k') == 'bc' and ms ~= nil and ms <= 5000, 'update keeps it')\n\
         lur.kv.update('k', function(cur) return cur end, { ttl_ms = 60000 })\n\
         ms = lur.kv.ttl('k')\n\
         assert(ms > 5000, 'update with ttl_ms replaces it')\n\
         assert(lur.kv.cas('k', 'bc', 'd', { ttl_ms = 90000 }) == true)\n\
         ms = lur.kv.ttl('k')\n\
         assert(ms > 60000, 'cas with ttl_ms replaces it')");
}

#[test]
fn expired_keys_read_as_absent_in_cas_and_update() {
    run("lur.kv.set('k', 'old', { ttl_ms = 100 })\n\
         lur.async.sleep(250)\n\
         assert(lur.kv.cas('k', 'old', 'x') == false, 'expired value cannot match')\n\
         assert(lur.kv.cas('k', nil, 'fresh') == true, 'nil matches an expired key')\n\
         assert(lur.kv.get('k') == 'fresh')\n\
         lur.kv.set('u', 'old', { ttl_ms = 100 })\n\
         lur.async.sleep(250)\n\
         local seen = 'unset'\n\
         lur.kv.update('u', function(cur) seen = cur; return 'new' end)\n\
         assert(seen == nil, 'transform sees nil')\n\
         assert(lur.kv.get('u') == 'new')");
}

#[test]
fn incr_restarts_an_expired_counter_even_if_it_held_text() {
    run("lur.kv.set('c', 'text', { ttl_ms = 100 })\n\
         lur.async.sleep(250)\n\
         assert(lur.kv.incr('c', 5, { ttl_ms = 5000 }) == 5)");
}

#[test]
fn expire_sets_an_expiry_on_a_live_key() {
    run("assert(lur.kv.expire('missing', 1000) == false)\n\
         lur.kv.set('k', 'v')\n\
         assert(lur.kv.expire('k', 100) == true)\n\
         local ms = lur.kv.ttl('k')\n\
         assert(ms ~= nil and ms <= 100)\n\
         lur.async.sleep(250)\n\
         assert(lur.kv.get('k') == nil)\n\
         assert(lur.kv.expire('k', 1000) == false, 'expired key is gone')");
}

#[test]
fn invalid_ttl_options_raise() {
    for opts in [
        "{ ttl_ms = 0 }",
        "{ ttl_ms = -5 }",
        "{ ttl_ms = 'x' }",
        "{ ttl_ms = 1.5 }",
    ] {
        let err = run_err(&format!("lur.kv.set('k', 'v', {opts})"));
        assert!(
            err.contains("ttl_ms must be a positive integer"),
            "{opts}: {err}"
        );
    }
    let err = run_err("lur.kv.incr('k', 1, { renew_ttl = true })");
    assert!(err.contains("renew_ttl requires"), "{err}");
    let err = run_err("lur.kv.expire('k', 0)");
    assert!(err.contains("positive integer"), "{err}");
}

#[test]
fn a_database_from_before_ttls_is_upgraded_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    // Recreate the pre-TTL table shape, with a row in it.
    runtime(path.clone())
        .run(
            "lur.db.exec('DROP TABLE lur_kv')\n\
             lur.db.exec('CREATE TABLE lur_kv (key TEXT PRIMARY KEY, value BLOB)')\n\
             lur.db.exec(\"INSERT INTO lur_kv (key, value) VALUES ('old', 'kept')\")",
        )
        .expect("old schema built");
    runtime(path)
        .run(
            "assert(lur.kv.get('old') == 'kept', 'existing rows survive')\n\
             local _, exists = lur.kv.ttl('old')\n\
             assert(exists == true, 'and have no expiry')\n\
             lur.kv.set('new', 'v', { ttl_ms = 5000 })\n\
             assert(lur.kv.ttl('new') ~= nil)",
        )
        .expect("upgraded");
}

#[test]
fn expired_rows_are_swept_when_the_database_opens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sweep.db");
    runtime(path.clone())
        .run("lur.kv.set('k', 'v', { ttl_ms = 100 })\nlur.async.sleep(250)")
        .expect("seeded");
    runtime(path)
        .run(
            "lur.kv.get('anything')\n\
             local rows = lur.db.query('SELECT COUNT(*) AS n FROM lur_kv')\n\
             assert(rows[1].n == 0, 'expired row swept on open')",
        )
        .expect("swept");
}
