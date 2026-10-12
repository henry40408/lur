//! `lur.state` expiry, `fetch`, and the entry/byte caps.

use std::sync::Arc;

use lur::capabilities::state::{Limits, StateStore};
use lur::runtime::{Runtime, RuntimeConfig};

fn run(script: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(script)
        .unwrap_or_else(|e| panic!("script failed: {e}"));
}

fn run_err(script: &str) -> String {
    Runtime::new()
        .expect("runtime builds")
        .run(script)
        .expect_err("script should fail")
        .to_string()
}

fn limited(max_entries: usize, max_bytes: usize) -> Runtime {
    Runtime::with_config(RuntimeConfig {
        state: Arc::new(StateStore::with_limits(Limits {
            max_entries,
            max_bytes,
        })),
        ..Default::default()
    })
    .expect("runtime builds")
}

#[test]
fn ttl_expires_and_reports_remaining_time() {
    run("lur.state.set('long', 'v', { ttl_ms = 60000 })\n\
         local ms, exists = lur.state.ttl('long')\n\
         assert(exists == true and ms > 0 and ms <= 60000, 'remaining ms')\n\
         assert(lur.state.get('long') == 'v')\n\
         lur.state.set('k', 'v', { ttl_ms = 100 })\n\
         lur.async.sleep(250)\n\
         assert(lur.state.get('k') == nil, 'gone after expiry')\n\
         local ms2, exists2 = lur.state.ttl('k')\n\
         assert(ms2 == nil and exists2 == false, 'expired reads as absent')\n\
         lur.state.set('p', 1)\n\
         local ms3, exists3 = lur.state.ttl('p')\n\
         assert(ms3 == nil and exists3 == true, 'no expiry')");
}

#[test]
fn set_clears_expiry_while_cas_update_and_incr_keep_it() {
    run("lur.state.set('k', 1, { ttl_ms = 100000 })\n\
         lur.state.incr('k')\n\
         assert(lur.state.cas('k', 2, 5) == true)\n\
         lur.state.update('k', function(v) return v + 1 end)\n\
         assert(lur.state.ttl('k') ~= nil, 'still expiring')\n\
         lur.state.set('k', 9)\n\
         assert(lur.state.ttl('k') == nil, 'set cleared it')");
}

#[test]
fn incr_ttl_is_a_fixed_window_and_renew_resets_it() {
    // Windows are 600 ms with 400 ms steps: ~200 ms of scheduling slack.
    run("lur.state.incr('c', 1, { ttl_ms = 600 })\n\
         lur.async.sleep(400)\n\
         lur.state.incr('c', 1, { ttl_ms = 600 })\n\
         assert(lur.state.get('c') == 2)\n\
         lur.async.sleep(400)\n\
         assert(lur.state.get('c') == nil, 'window was not extended')\n\
         lur.state.incr('r', 1, { ttl_ms = 600, renew_ttl = true })\n\
         lur.async.sleep(400)\n\
         lur.state.incr('r', 1, { ttl_ms = 600, renew_ttl = true })\n\
         lur.async.sleep(400)\n\
         assert(lur.state.get('r') == 2, 'renewed')");
}

#[test]
fn option_errors_are_clear() {
    let e = run_err("lur.state.set('k', 1, { ttl_ms = 0 })");
    assert!(e.contains("opts.ttl_ms must be a positive integer"), "{e}");
    let e = run_err("lur.state.set('k', 1, { evictable = 'yes' })");
    assert!(e.contains("opts.evictable must be a boolean"), "{e}");
    let e = run_err("lur.state.incr('k', 1, { renew_ttl = true })");
    assert!(e.contains("renew_ttl requires opts.ttl_ms"), "{e}");
    let e = run_err("lur.state.fetch('k', {}, function() return 1 end)");
    assert!(e.contains("opts.ttl_ms is required"), "{e}");
    let e = run_err("lur.state.fetch('k', { ttl_ms = 10 }, 'nope')");
    assert!(e.contains("argument #3 must be function"), "{e}");
}

#[test]
fn fetch_loads_once_then_hits() {
    run("local calls = 0\n\
         local function load() calls = calls + 1; return 'body' end\n\
         local v, s = lur.state.fetch('k', { ttl_ms = 60000 }, load)\n\
         assert(v == 'body' and s == 'miss', s)\n\
         v, s = lur.state.fetch('k', { ttl_ms = 60000 }, load)\n\
         assert(v == 'body' and s == 'hit', s)\n\
         assert(calls == 1, 'loader ran once')\n\
         assert(lur.state.get('k') == 'body', 'get sees a fetched value')");
}

#[test]
fn fetch_reloads_after_ttl() {
    run("local n = 0\n\
         local function load() n = n + 1; return n end\n\
         assert(lur.state.fetch('k', { ttl_ms = 80 }, load) == 1)\n\
         lur.async.sleep(200)\n\
         local v, s = lur.state.fetch('k', { ttl_ms = 80 }, load)\n\
         assert(v == 2 and s == 'miss', s)");
}

#[test]
fn fetch_does_not_cache_nil() {
    run("local n = 0\n\
         local function load() n = n + 1; return nil end\n\
         local v, s = lur.state.fetch('k', { ttl_ms = 60000 }, load)\n\
         assert(v == nil and s == 'miss')\n\
         lur.state.fetch('k', { ttl_ms = 60000 }, load)\n\
         assert(n == 2, 'nil is not cached')");
}

#[test]
fn fetch_serves_stale_when_the_loader_fails() {
    run(
        "lur.state.fetch('k', { ttl_ms = 60, stale_ms = 60000 }, function() return 'old' end)\n\
         lur.async.sleep(150)\n\
         local v, s = lur.state.fetch('k', { ttl_ms = 60, stale_ms = 60000 }, function()\n\
           error('upstream down')\n\
         end)\n\
         assert(v == 'old' and s == 'stale', tostring(v) .. tostring(s))\n\
         assert(lur.state.get('k') == nil, 'get never returns a stale value')\n\
         assert(lur.state.add('k', 'fresh') == true, 'a stale key counts as absent')",
    );
}

#[test]
fn fetch_raises_when_there_is_no_stale_value_or_it_aged_out() {
    let e =
        run_err("lur.state.fetch('k', { ttl_ms = 60000 }, function() error('upstream down') end)");
    assert!(e.contains("upstream down"), "{e}");
    let e = run_err(
        "lur.state.fetch('k', { ttl_ms = 40, stale_ms = 40 }, function() return 'old' end)\n\
         lur.async.sleep(250)\n\
         lur.state.fetch('k', { ttl_ms = 40, stale_ms = 40 }, function() error('still down') end)",
    );
    assert!(e.contains("still down"), "{e}");
}

#[test]
fn fetch_loader_may_await_and_use_state() {
    run(
        "local v = lur.state.fetch('k', { ttl_ms = 60000 }, function()\n\
           lur.async.sleep(20)\n\
           lur.state.set('side', 'effect')\n\
           return 'loaded'\n\
         end)\n\
         assert(v == 'loaded')\n\
         assert(lur.state.get('side') == 'effect')",
    );
}

#[test]
fn fetch_does_not_overwrite_a_write_made_while_the_loader_ran() {
    run(
        "local v, s = lur.state.fetch('c', { ttl_ms = 50 }, function()\n\
           lur.state.set('c', 7)\n\
           return 1\n\
         end)\n\
         assert(v == 1 and s == 'miss', 'caller still gets the loaded value')\n\
         lur.async.sleep(150)\n\
         assert(lur.state.get('c') == 7, 'the concurrent set survives, unexpired')\n\
         assert(lur.state.ttl('c') == nil, 'and stays a plain, non-expiring key')",
    );
}

#[test]
fn update_keeps_the_stale_window_of_a_fetched_value() {
    run(
        "lur.state.fetch('f', { ttl_ms = 50, stale_ms = 60000 }, function() return 'old' end)\n\
         lur.state.update('f', function(v) return v .. '!' end)\n\
         lur.async.sleep(150)\n\
         local v, s = lur.state.fetch('f', { ttl_ms = 50 }, function() error('down') end)\n\
         assert(v == 'old!' and s == 'stale', tostring(v) .. tostring(s))",
    );
}

#[test]
fn fetch_cannot_run_inside_update() {
    let e = run_err(
        "lur.state.update('k', function(v)\n\
           lur.state.fetch('x', { ttl_ms = 10 }, function() return 1 end)\n\
           return 1\n\
         end)",
    );
    assert!(e.contains("cannot be re-entered"), "{e}");
}

#[test]
fn a_full_store_keeps_counters_and_evicts_cache_entries() {
    limited(3, 0)
        .run(
            "lur.state.set('pin', 1)\n\
             lur.state.fetch('a', { ttl_ms = 60000 }, function() return 'A' end)\n\
             lur.state.fetch('b', { ttl_ms = 60000 }, function() return 'B' end)\n\
             lur.state.fetch('c', { ttl_ms = 60000 }, function() return 'C' end)\n\
             assert(lur.state.get('pin') == 1, 'unevictable survives')\n\
             assert(lur.state.get('a') == nil, 'oldest cache entry evicted')\n\
             assert(lur.state.get('c') == 'C')",
        )
        .expect("evicts");
}

#[test]
fn a_store_full_of_unevictable_entries_raises_and_loses_nothing() {
    limited(2, 0)
        .run(
            "lur.state.set('x', 1)\n\
             lur.state.set('y', 2)\n\
             local ok, e = pcall(lur.state.set, 'z', 3)\n\
             assert(not ok and tostring(e):find('state is full'), tostring(e))\n\
             assert(lur.state.get('x') == 1 and lur.state.get('y') == 2)\n\
             lur.state.set('x', 10) -- overwriting is not growth\n\
             -- a fetch that cannot be stored still returns its value\n\
             local v, s = lur.state.fetch('w', { ttl_ms = 1000 }, function() return 'v' end)\n\
             assert(v == 'v' and s == 'miss')\n\
             assert(lur.state.get('w') == nil)",
        )
        .expect("full store");
}

#[test]
fn state_limits_apply_by_bytes() {
    limited(0, 600)
        .run(
            "local big = string.rep('x', 200)\n\
             for i = 1, 10 do\n\
               lur.state.fetch('k' .. i, { ttl_ms = 60000 }, function() return big end)\n\
             end\n\
             assert(lur.state.get('k10') == big)\n\
             assert(lur.state.get('k1') == nil)",
        )
        .expect("byte cap evicts");
}
