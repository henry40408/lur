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

// ---- `fetch` single-flight ------------------------------------------------
//
// Cross-VM tests run one `Runtime` per thread over a shared `StateStore`, so
// each caller has its own timeout, as pooled requests do. Counters live in
// `lur.state` itself (process-wide) and are checked by a final script.

mod single_flight {
    use std::sync::{Arc, Barrier};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use lur::capabilities::state::{Limits, StateStore};
    use lur::runtime::{RunError, Runtime, RuntimeConfig};
    use lur::serve::Server;

    const O: &str = "{ ttl_ms = 60000 }";

    fn vm(state: &Arc<StateStore>) -> Runtime {
        Runtime::with_config(RuntimeConfig {
            state: state.clone(),
            ..Default::default()
        })
        .expect("runtime builds")
    }

    /// Run `script` on its own VM and thread after `delay`, under `timeout`.
    fn spawn(
        state: &Arc<StateStore>,
        script: String,
        delay: Duration,
        timeout: Duration,
    ) -> JoinHandle<Result<(), RunError>> {
        let state = state.clone();
        std::thread::spawn(move || {
            let rt = vm(&state);
            std::thread::sleep(delay);
            rt.run_with_timeout(&script, timeout)
        })
    }

    /// `n` VMs that start `script` together (behind a barrier).
    fn together(state: &Arc<StateStore>, n: usize, script: &str) -> Vec<Result<(), RunError>> {
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let (state, barrier, script) = (state.clone(), barrier.clone(), script.to_owned());
                std::thread::spawn(move || {
                    let rt = vm(&state);
                    barrier.wait();
                    rt.run_with_timeout(&script, Duration::from_secs(10))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    }

    fn check(state: &Arc<StateStore>, script: &str) {
        vm(state)
            .run(script)
            .unwrap_or_else(|e| panic!("check failed: {e}"));
    }

    fn ok(results: &[Result<(), RunError>]) {
        for r in results {
            if let Err(e) = r {
                panic!("a caller failed: {e}");
            }
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A loader that counts its runs in `runs`; the first run does `first`.
    fn counted(first: &str, then: &str) -> String {
        format!(
            "function()\n\
               if lur.state.incr('runs') == 1 then {first} end\n\
               {then}\n\
             end"
        )
    }

    #[test]
    fn concurrent_misses_across_vms_run_the_loader_once() {
        let state = Arc::new(StateStore::default());
        let load = counted("", "lur.async.sleep(300) return 'v'");
        let script = format!(
            "local v, s = lur.state.fetch('k', {O}, {load})\n\
             assert(v == 'v', tostring(v))\n\
             lur.state.incr('status:' .. s)"
        );
        ok(&together(&state, 8, &script));
        check(
            &state,
            "assert(lur.state.get('runs') == 1, 'loader ran once')\n\
             assert(lur.state.get('status:miss') == 1, 'the leader reports miss')\n\
             assert(lur.state.get('status:hit') == 7, 'waiters report hit')",
        );
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn concurrent_misses_on_one_vm_run_the_loader_once() {
        super::run(&format!(
            "local runs = 0\n\
             local function load() runs = runs + 1; lur.async.sleep(50); return runs end\n\
             local function task() return {{ lur.state.fetch('k', {O}, load) }} end\n\
             local out = lur.async.all({{ task, task, task, task }})\n\
             assert(runs == 1, 'ran ' .. runs)\n\
             local misses = 0\n\
             for _, r in ipairs(out) do\n\
               assert(r[1] == 1)\n\
               if r[2] == 'miss' then misses = misses + 1 else assert(r[2] == 'hit', r[2]) end\n\
             end\n\
             assert(misses == 1)"
        ));
    }

    #[test]
    fn different_keys_do_not_wait_on_each_other() {
        let state = Arc::new(StateStore::default());
        let slow = spawn(
            &state,
            format!("lur.state.fetch('slow', {O}, function() lur.async.sleep(1500) return 1 end)"),
            ms(0),
            secs(10),
        );
        std::thread::sleep(ms(100));
        let started = Instant::now();
        vm(&state)
            .run(&format!(
                "assert(lur.state.fetch('fast', {O}, function() return 2 end) == 2)"
            ))
            .unwrap();
        assert!(started.elapsed() < ms(1000), "{:?}", started.elapsed());
        assert_eq!(state.in_flight(), 1, "only 'slow' is in flight");
        slow.join().unwrap().unwrap();
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_failure_is_shared_with_every_waiter() {
        let state = Arc::new(StateStore::default());
        let load = counted("", "lur.async.sleep(300) error('upstream boom')");
        let script = format!("lur.state.fetch('k', {O}, {load})");
        let results = together(&state, 6, &script);
        for r in &results {
            let e = r.as_ref().expect_err("every caller raises").to_string();
            assert!(e.contains("upstream boom"), "{e}");
        }
        check(
            &state,
            "assert(lur.state.get('runs') == 1, 'not retried by the waiters')",
        );
        assert_eq!(state.in_flight(), 0);
        // The failure is not remembered: the next caller loads again.
        check(
            &state,
            &format!("assert(lur.state.fetch('k', {O}, function() return 'ok' end) == 'ok')"),
        );
    }

    #[test]
    fn a_shared_failure_falls_back_to_the_stale_value() {
        let state = Arc::new(StateStore::default());
        check(
            &state,
            "lur.state.fetch('k', { ttl_ms = 50, stale_ms = 60000 }, function() return 'old' end)\n\
             lur.async.sleep(150)",
        );
        let load = counted("", "lur.async.sleep(300) error('down')");
        let script = format!(
            "local v, s = lur.state.fetch('k', {{ ttl_ms = 50, stale_ms = 60000 }}, {load})\n\
             assert(v == 'old' and s == 'stale', tostring(v) .. ' ' .. tostring(s))"
        );
        ok(&together(&state, 5, &script));
        check(&state, "assert(lur.state.get('runs') == 1)");
    }

    #[test]
    fn nil_is_shared_but_not_cached() {
        let state = Arc::new(StateStore::default());
        let load = counted("", "lur.async.sleep(300) return nil");
        let script = format!(
            "local v, s = lur.state.fetch('k', {O}, {load})\n\
             assert(v == nil, tostring(v))\n\
             lur.state.incr('status:' .. s)"
        );
        ok(&together(&state, 5, &script));
        check(
            &state,
            &format!(
                "assert(lur.state.get('runs') == 1)\n\
                 assert(lur.state.get('status:miss') == 1 and lur.state.get('status:hit') == 4)\n\
                 local _, s = lur.state.fetch('k', {O}, function() return 1 end)\n\
                 assert(s == 'miss', 'nil was not cached')"
            ),
        );
    }

    #[test]
    fn a_non_primitive_result_is_raised_for_every_waiter() {
        let state = Arc::new(StateStore::default());
        let load = counted("", "lur.async.sleep(300) return {}");
        let results = together(&state, 4, &format!("lur.state.fetch('k', {O}, {load})"));
        for r in &results {
            let e = r.as_ref().expect_err("every caller raises").to_string();
            assert!(e.contains("stores only"), "{e}");
        }
        check(&state, "assert(lur.state.get('runs') == 1)");
    }

    #[test]
    fn waiters_get_a_value_that_does_not_fit() {
        let state = Arc::new(StateStore::with_limits(Limits {
            max_entries: 0,
            max_bytes: 200,
        }));
        let load = counted("", "lur.async.sleep(300) return string.rep('x', 1000)");
        let script = format!(
            "local v = lur.state.fetch('k', {O}, {load})\n\
             assert(v == string.rep('x', 1000))"
        );
        ok(&together(&state, 4, &script));
        check(
            &state,
            "assert(lur.state.get('runs') == 1)\nassert(lur.state.get('k') == nil, 'uncached')",
        );
    }

    /// The leader's own request times out; one waiter takes over and the
    /// loader runs exactly twice.
    fn handoff_after(first: &str) {
        let state = Arc::new(StateStore::default());
        let load = counted(first, "return 'v'");
        let script = format!(
            "local v, s = lur.state.fetch('k', {O}, {load})\n\
             assert(v == 'v', tostring(v))\n\
             lur.state.incr('status:' .. s)"
        );
        let leader = spawn(&state, script.clone(), ms(0), ms(300));
        let waiters: Vec<_> = (0..5)
            .map(|_| spawn(&state, script.clone(), ms(100), secs(10)))
            .collect();
        let led = leader.join().unwrap();
        assert!(matches!(led, Err(RunError::Timeout)), "{led:?}");
        for w in waiters {
            w.join().unwrap().unwrap();
        }
        check(
            &state,
            "assert(lur.state.get('runs') == 2, 'exactly one new leader')\n\
             assert(lur.state.get('status:miss') == 1)\n\
             assert(lur.state.get('status:hit') == 4)",
        );
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_leader_parked_on_io_past_its_timeout_hands_off() {
        handoff_after("lur.async.sleep(5000)");
    }

    #[test]
    fn a_cpu_bound_leader_past_its_deadline_hands_off() {
        handoff_after("while true do end");
    }

    #[test]
    fn a_leader_whose_deadline_fires_inside_pcall_still_hands_off() {
        // The interrupt keeps raising, so the loader cannot return normally.
        handoff_after("for _ = 1, 1e9 do pcall(function() while true do end end) end");
    }

    #[test]
    fn a_cancelled_leader_hands_off() {
        let state = Arc::new(StateStore::default());
        let load = counted("lur.async.sleep(5000)", "return 'v'");
        // `race` settles with the sleeper and drops the leading fetch. The
        // run then carries on, so only the dropped fetch can hand off: the
        // end of the run comes far too late for the waiters' deadline below.
        let started = Instant::now();
        let leader = spawn(
            &state,
            format!(
                "local r = lur.async.race({{\n\
                   function() return lur.state.fetch('k', {O}, {load}) end,\n\
                   function() lur.async.sleep(300) return 'cancelled' end,\n\
                 }})\n\
                 assert(r == 'cancelled', tostring(r))\n\
                 lur.async.sleep(3000)"
            ),
            ms(0),
            secs(10),
        );
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                spawn(
                    &state,
                    format!("assert(lur.state.fetch('k', {O}, {load}) == 'v')"),
                    ms(100),
                    secs(10),
                )
            })
            .collect();
        for w in waiters {
            w.join().unwrap().unwrap();
        }
        assert!(started.elapsed() < ms(1500), "{:?}", started.elapsed());
        leader.join().unwrap().unwrap();
        check(&state, "assert(lur.state.get('runs') == 2)");
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_waiter_is_bound_by_its_own_timeout() {
        let state = Arc::new(StateStore::default());
        let load = counted("", "lur.async.sleep(1500) return 'v'");
        let script = format!("assert(lur.state.fetch('k', {O}, {load}) == 'v')");
        let leader = spawn(&state, script.clone(), ms(0), secs(10));
        std::thread::sleep(ms(100));
        let started = Instant::now();
        let waiter = vm(&state).run_with_timeout(&script, ms(150));
        assert!(matches!(waiter, Err(RunError::Timeout)), "{waiter:?}");
        assert!(started.elapsed() < ms(1000), "{:?}", started.elapsed());
        assert_eq!(state.in_flight(), 1, "the leader carries on");
        leader.join().unwrap().unwrap();
        check(&state, "assert(lur.state.get('runs') == 1)");
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_loader_fetching_its_own_key_runs_it_inline() {
        let rt = vm(&Arc::new(StateStore::default()));
        rt.run_with_timeout(
            &format!(
                "local v, s = lur.state.fetch('k', {O}, function()\n\
                   local inner, si = lur.state.fetch('k', {O}, function() return 'inner' end)\n\
                   assert(si == 'miss', si)\n\
                   return 'outer:' .. inner\n\
                 end)\n\
                 assert(v == 'outer:inner' and s == 'miss', v)\n\
                 assert(lur.state.get('k') == 'inner', 'the first store wins')\n\
                 -- also from a lur.async task the loader starts\n\
                 local w = lur.state.fetch('t', {O}, function()\n\
                   return lur.async.all({{ function()\n\
                     return lur.state.fetch('t', {O}, function() return 'task' end)\n\
                   end }})[1]\n\
                 end)\n\
                 assert(w == 'task')"
            ),
            secs(5),
        )
        .expect("no self-deadlock");
    }

    #[test]
    fn a_cycle_between_two_keys_does_not_deadlock() {
        // a's loader waits on b while b's loader fetches a: b runs a inline.
        let rt = vm(&Arc::new(StateStore::default()));
        rt.run_with_timeout(
            &format!(
                "local function a() lur.async.sleep(30)\n\
                   return 'a+' .. lur.state.fetch('b', {O}, function() return 'b0' end) end\n\
                 local function b() lur.async.sleep(60)\n\
                   return 'b+' .. lur.state.fetch('a', {O}, function() return 'a0' end) end\n\
                 local out = lur.async.all({{\n\
                   function() return lur.state.fetch('a', {O}, a) end,\n\
                   function() return lur.state.fetch('b', {O}, b) end,\n\
                 }})\n\
                 assert(out[2] == 'b+a0', out[2])\n\
                 assert(out[1] == 'a+b+a0', out[1])"
            ),
            secs(5),
        )
        .expect("no cross-key deadlock");
    }

    #[test]
    fn a_cycle_across_vms_does_not_deadlock() {
        let state = Arc::new(StateStore::default());
        let a = spawn(
            &state,
            format!(
                "lur.state.fetch('a', {O}, function() lur.async.sleep(30)\n\
                   return lur.state.fetch('b', {O}, function() return 'b0' end) end)"
            ),
            ms(0),
            secs(5),
        );
        let b = spawn(
            &state,
            format!(
                "lur.state.fetch('b', {O}, function() lur.async.sleep(150)\n\
                   return lur.state.fetch('a', {O}, function() return 'a0' end) end)"
            ),
            ms(0),
            secs(5),
        );
        a.join().unwrap().expect("a finishes");
        b.join().unwrap().expect("b finishes");
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_timed_out_leader_on_a_kept_vm_removes_its_flight_at_once() {
        // The VM lives on (a pooled VM is reused, not dropped), so the flight
        // must go when the run ends, not when the VM is eventually collected.
        let state = Arc::new(StateStore::default());
        let rt = vm(&state);
        let fetch =
            format!("lur.state.fetch('k', {O}, function() lur.async.sleep(5000) return 1 end)");
        for script in [
            fetch.clone(),
            format!("pcall(function() return {fetch} end)"),
            format!("lur.async.race({{ function() return {fetch} end }})"),
            format!("pcall(lur.async.all, {{ function() return {fetch} end }})"),
            format!("lur.state.fetch('k', {O}, function() pcall(lur.async.sleep, 5000) end)"),
            format!("lur.state.fetch('k', {O}, function() while true do end end)"),
        ] {
            let r = rt.run_with_timeout(&script, ms(100));
            assert!(matches!(r, Err(RunError::Timeout)), "{script}: {r:?}");
            assert_eq!(state.in_flight(), 0, "{script}");
        }
    }

    /// A leader parked in a coroutine the script never resumes.
    const PARK: &str = "local co = coroutine.create(function()\n\
           return lur.state.fetch('k', { ttl_ms = 60000 }, function() lur.async.sleep(50) return 1 end)\n\
         end)\n\
         coroutine.resume(co)";

    /// The same, with the leader inside a `lur.async` task: the coroutine
    /// parks a whole combinator call, task and all.
    const PARK_TASK: &str = "local co = coroutine.create(function()\n\
           return lur.async.all({ function()\n\
             return lur.state.fetch('k', { ttl_ms = 60000 }, function() lur.async.sleep(50) return 1 end)\n\
           end })\n\
         end)\n\
         coroutine.resume(co)";

    #[test]
    fn a_leader_left_in_a_suspended_coroutine_is_removed_when_the_run_ends() {
        let state = Arc::new(StateStore::default());
        let rt = vm(&state);
        rt.run_with_timeout(&format!("{PARK}\nKEEP = co"), secs(5))
            .expect("the run itself ends normally");
        assert_eq!(state.in_flight(), 0);
        let started = Instant::now();
        check(
            &state,
            &format!("assert(lur.state.fetch('k', {O}, function() return 2 end) == 2)"),
        );
        assert!(started.elapsed() < ms(1000), "{:?}", started.elapsed());
    }

    #[test]
    fn a_parked_leader_kept_by_a_pooled_vm_does_not_block_the_key() {
        let state = Arc::new(StateStore::default());
        // `parked` is an app.lua upvalue: it outlives the request, so no GC
        // would ever drop the coroutine.
        let app = format!(
            "local parked = {{}}\n\
             lur.serve.http('GET', '/park', function()\n\
               {PARK}\n\
               table.insert(parked, co)\n\
               return {{ status = 200, body = 'parked' }}\n\
             end)\n\
             lur.serve.http('GET', '/get', function()\n\
               return {{ status = 200, body = tostring(lur.state.fetch('k', {O}, function() return 2 end)) }}\n\
             end)"
        );
        let server = Server::load(
            &app,
            RuntimeConfig {
                pool_size: 1,
                state: state.clone(),
                per_event_timeout: Some(secs(2)),
                ..Default::default()
            },
        )
        .expect("app loads");
        assert_eq!(server.dispatch("GET", "/park", b"").unwrap().status, 200);
        assert_eq!(state.in_flight(), 0);
        let started = Instant::now();
        let r = server.dispatch("GET", "/get", b"").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, b"2".as_slice()));
        assert!(started.elapsed() < ms(1000), "{:?}", started.elapsed());
    }

    /// Run `script` with no timeout on its own thread; panic if it has not
    /// finished within 5s (it would otherwise hang the test forever).
    fn finishes_untimed(state: &Arc<StateStore>, script: String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let state = state.clone();
        std::thread::spawn(move || {
            let _ = tx.send(vm(&state).run(&script));
        });
        rx.recv_timeout(secs(5))
            .expect("the run waited on its own parked leader")
            .unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn a_run_fetching_a_key_it_parked_runs_the_loader_inline() {
        // The leader sits in a coroutine this run suspended: nothing will
        // resume it while the run waits, so the run must not wait on it.
        let get = format!(
            "local v, s = lur.state.fetch('k', {O}, function() return 2 end)\n\
             assert(v == 2 and s == 'miss', tostring(v) .. ' ' .. tostring(s))"
        );
        for script in [
            format!("{PARK}\n{get}"),
            // from a lur.async task started after parking
            format!("{PARK}\nlur.async.all({{ function()\n{get}\nend }})"),
            // parked and fetched inside the same task
            format!("lur.async.all({{ function()\n{PARK}\n{get}\nend }})"),
            // parked in a task that has ended, fetched after it
            format!("lur.async.all({{ function()\n{PARK}\nKEEP = co\nend }})\n{get}"),
            format!(
                "lur.async.all({{ function()\n{PARK}\nKEEP = co\nend }})\n\
                 lur.async.all({{ function()\n{get}\nend }})"
            ),
            // the leader in a task inside the parked coroutine
            format!("{PARK_TASK}\n{get}"),
            format!("{PARK_TASK}\nlur.async.all({{ function()\n{get}\nend }})"),
            format!("lur.async.all({{ function()\n{PARK_TASK}\n{get}\nend }})"),
            format!(
                "lur.async.all({{ function()\n{PARK_TASK}\n\
                 return lur.async.all({{ function()\n{get}\nend }})\nend }})"
            ),
            // ... parked by a task that has ended, fetched after it
            format!("lur.async.all({{ function()\n{PARK_TASK}\nKEEP = co\nend }})\n{get}"),
            format!(
                "lur.async.all({{ function()\n{PARK_TASK}\nKEEP = co\nend }})\n\
                 lur.async.all({{ function()\n{get}\nend }})"
            ),
        ] {
            let state = Arc::new(StateStore::default());
            finishes_untimed(&state, script.clone());
            assert_eq!(state.in_flight(), 0);
            let state = Arc::new(StateStore::default());
            let started = Instant::now();
            vm(&state)
                .run_with_timeout(&script, secs(5))
                .unwrap_or_else(|e| panic!("{script}: {e}"));
            assert!(started.elapsed() < ms(1000), "{:?}", started.elapsed());
        }
    }

    #[test]
    fn a_leader_parked_by_a_sibling_task_hands_off_at_once() {
        // The resume that parked it returned, so the flight is already gone:
        // the sibling loads instead of waiting for the parking task to end.
        for park in [PARK, PARK_TASK] {
            let state = Arc::new(StateStore::default());
            finishes_untimed(
                &state,
                format!(
                    "local out = lur.async.all({{\n\
                       function() {park}\nKEEP = co\nlur.async.sleep(1000)\n\
                         assert(FETCHED, 'the sibling waited for this task') end,\n\
                       function() lur.async.sleep(50)\n\
                         local r = {{ lur.state.fetch('k', {O}, function() return 2 end) }}\n\
                         FETCHED = true return r\n\
                       end,\n\
                     }})\n\
                     assert(out[2][1] == 2 and out[2][2] == 'miss', tostring(out[2][1]))"
                ),
            );
            assert_eq!(state.in_flight(), 0);
        }
    }

    #[test]
    fn a_leader_moved_between_sibling_tasks_never_waits_on_itself() {
        // Review repro: t1 parks the leader, t2 resumes and re-parks it, then
        // fetches the key while t1 waits for t2. Both orders of parking.
        for park in [
            format!("{PARK}\nKEEP = co"),
            "co = coroutine.create(function()\n\
               return lur.state.fetch('k', { ttl_ms = 60000 }, function() lur.async.sleep(50) return 1 end)\n\
             end)"
                .to_owned(),
        ] {
            let state = Arc::new(StateStore::default());
            finishes_untimed(
                &state,
                format!(
                    "{park}\n\
                     lur.async.all({{\n\
                       function() if coroutine.status(co) == 'suspended' then coroutine.resume(co) end\n\
                         while not lur.state.get('t2done') do lur.async.sleep(10) end end,\n\
                       function() lur.async.sleep(20) coroutine.resume(co)\n\
                         local v = lur.state.fetch('k', {O}, function() return 2 end)\n\
                         assert(v == 2, tostring(v)) lur.state.set('t2done', true) end,\n\
                     }})"
                ),
            );
            assert_eq!(state.in_flight(), 0);
        }
    }

    #[test]
    fn a_sibling_that_parked_a_leader_then_waits_on_the_caller_does_not_deadlock() {
        // Review repro: t1 parks a leader of `k` and then waits for t2; t2
        // fetches `k`. Waiting on the parked leader would wait on t1, which
        // waits on t2.
        for park in [PARK, PARK_TASK] {
            let state = Arc::new(StateStore::default());
            finishes_untimed(
                &state,
                format!(
                    "lur.async.all({{\n\
                       function() {park}\nKEEP = co\n\
                         while not lur.state.get('t2done') do lur.async.sleep(10) end end,\n\
                       function() lur.async.sleep(20)\n\
                         local v = lur.state.fetch('k', {O}, function() return 2 end)\n\
                         assert(v == 2, tostring(v)) lur.state.set('t2done', true) end,\n\
                     }})"
                ),
            );
            assert_eq!(state.in_flight(), 0);
        }
    }

    #[test]
    fn every_way_to_park_a_leader_hands_off_when_its_resume_returns() {
        let get = format!(
            "local v, s = lur.state.fetch('k', {O}, function() return 2 end)\n\
             assert(v == 2 and s == 'miss', tostring(v) .. ' ' .. tostring(s))"
        );
        let lead =
            "lur.state.fetch('k', { ttl_ms = 60000 }, function() lur.async.sleep(50) return 1 end)";
        for (name, park) in [
            (
                "wrap",
                format!("local g = coroutine.wrap(function() return {lead} end) g() KEEP = g"),
            ),
            (
                "pcall(resume)",
                format!(
                    "local co = coroutine.create(function() return {lead} end) pcall(coroutine.resume, co) KEEP = co"
                ),
            ),
            (
                "closed",
                format!(
                    "local co = coroutine.create(function() return {lead} end) coroutine.resume(co) coroutine.close(co)"
                ),
            ),
            (
                "nested",
                format!(
                    "local co = coroutine.create(function()\n\
                       local c2 = coroutine.create(function() return {lead} end)\n\
                       coroutine.resume(c2) KEEP = c2 coroutine.yield() end)\n\
                     coroutine.resume(co) KEEP2 = co"
                ),
            ),
            (
                // The inner resume returning must restore the outer depth.
                "after an inner resume",
                format!(
                    "local co = coroutine.create(function()\n\
                       coroutine.resume(coroutine.create(function() end))\n\
                       return {lead} end)\n\
                     coroutine.resume(co) KEEP = co"
                ),
            ),
            (
                "in a loader",
                format!(
                    "lur.state.fetch('o', {O}, function()\n\
                       local co = coroutine.create(function() return {lead} end)\n\
                       coroutine.resume(co) KEEP = co return 1 end)"
                ),
            ),
            (
                "yield across pcall",
                format!(
                    "local co = coroutine.create(function() return pcall(function() return {lead} end) end) coroutine.resume(co) KEEP = co"
                ),
            ),
            (
                "loop",
                format!(
                    "for i = 1, 3 do local co = coroutine.create(function() return {lead} end) coroutine.resume(co) end"
                ),
            ),
        ] {
            let state = Arc::new(StateStore::default());
            let started = Instant::now();
            finishes_untimed(&state, format!("{park}\n{get}"));
            assert!(
                started.elapsed() < ms(1000),
                "{name}: {:?}",
                started.elapsed()
            );
            assert_eq!(state.in_flight(), 0, "{name}");
        }
    }

    #[test]
    fn a_scheduler_forwarding_the_pending_yield_completes_without_single_flight() {
        // A script-level scheduler that passes mlua's pending yield up to the
        // host. Each forward returns the resume, which hands the leader off;
        // the script still completes, but another VM's concurrent miss loads
        // too (a duplicate load, never a deadlock).
        let state = Arc::new(StateStore::default());
        let load = counted("", "lur.async.sleep(300) return 'v'");
        let forward = format!(
            "local co = coroutine.create(function() return lur.state.fetch('k', {O}, {load}) end)\n\
             local r\n\
             while true do\n\
               local ok, v = coroutine.resume(co) assert(ok, v)\n\
               if coroutine.status(co) == 'dead' then r = v break end\n\
               coroutine.yield(v)\n\
             end\n\
             assert(r == 'v', tostring(r))"
        );
        let a = spawn(&state, forward, ms(0), secs(10));
        let b = spawn(
            &state,
            format!("assert(lur.state.fetch('k', {O}, {load}) == 'v')"),
            ms(50),
            secs(10),
        );
        a.join().unwrap().expect("the forwarding script completes");
        b.join().unwrap().expect("the other VM completes");
        check(
            &state,
            "assert(lur.state.get('runs') == 2, lur.state.get('runs'))",
        );
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_parked_leader_is_spotted_when_its_coroutine_moved_between_tasks() {
        // `co` (a combinator call) is first resumed by t1, then driven by t2.
        // Inside it, x parks `inner`, whose task leads 'k' at resume depth
        // 2; resuming `inner` returned, so the flight is gone and x loads
        // itself rather than wait (here until t1 ends).
        let state = Arc::new(StateStore::default());
        finishes_untimed(
            &state,
            format!(
                "local O = {O}\n\
                 local co = coroutine.create(function()\n\
                   return lur.async.all({{ function()\n\
                     local inner = coroutine.create(function()\n\
                       lur.async.all({{ function()\n\
                         return lur.state.fetch('k', O, function() lur.async.sleep(50) return 1 end)\n\
                       end }})\n\
                     end)\n\
                     coroutine.resume(inner) KEEP = inner\n\
                     lur.async.sleep(100)\n\
                     local v, s = lur.state.fetch('k', O, function() return 2 end)\n\
                     assert(not DONE, 'waited for the task that parked the leader')\n\
                     assert(v == 2 and s == 'miss', tostring(v) .. ' ' .. tostring(s))\n\
                     FETCHED = true\n\
                   end }})\n\
                 end)\n\
                 lur.async.all({{\n\
                   function() assert(coroutine.resume(co)) lur.async.sleep(1500) DONE = true end,\n\
                   function() lur.async.sleep(20)\n\
                     while coroutine.status(co) ~= 'dead' do\n\
                       local ok, err = coroutine.resume(co) assert(ok, err) lur.async.sleep(10)\n\
                     end\n\
                   end,\n\
                 }})\n\
                 assert(FETCHED, 'x never fetched')"
            ),
        );
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_parked_leader_handing_off_leaves_a_live_leader_alone() {
        // 'k' is led live at depth 0; a sibling parks a leader of 'p' at
        // depth 1, so its resume returning sweeps the VM's nested flights:
        // 'k' must survive it, and the last task shares it.
        let state = Arc::new(StateStore::default());
        check(
            &state,
            &format!(
                "O = {O}\n\
                 local calls = 0\n\
                 local function load() calls = calls + 1 lur.async.sleep(200) return 1 end\n\
                 lur.async.all({{\n\
                   function() return lur.state.fetch('k', O, load) end,\n\
                   function() lur.async.sleep(20)\n\
                     local co = coroutine.create(function()\n\
                       return lur.state.fetch('p', O, function() lur.async.sleep(50) return 2 end)\n\
                     end)\n\
                     coroutine.resume(co) KEEP = co\n\
                   end,\n\
                   function() lur.async.sleep(60) return lur.state.fetch('k', O, load) end,\n\
                 }})\n\
                 assert(calls == 1, 'live leader dropped: ' .. calls)"
            ),
        );
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn sibling_tasks_share_a_live_leader() {
        // The leader runs under a sibling task (directly, or in a combinator
        // nested in it): it is live, so the others wait instead of loading.
        for (lead, name) in [
            ("lur.state.fetch('k', O, load)", "direct"),
            (
                "lur.async.all({ function() return lur.state.fetch('k', O, load) end })",
                "nested",
            ),
        ] {
            let state = Arc::new(StateStore::default());
            check(
                &state,
                &format!(
                    "O = {O}\n\
                     local calls = 0\n\
                     function load() calls = calls + 1 lur.async.sleep(100) return 1 end\n\
                     lur.async.all({{\n\
                       function() {lead} end,\n\
                       function() lur.async.sleep(20) return lur.state.fetch('k', O, load) end,\n\
                       function() lur.async.sleep(20) return lur.async.all({{ function()\n\
                         return lur.state.fetch('k', O, load) end }}) end,\n\
                     }})\n\
                     assert(calls == 1, '{name}: ' .. calls)"
                ),
            );
            assert_eq!(state.in_flight(), 0);
        }
    }

    #[test]
    fn a_leaders_memory_error_reaches_waiters_as_a_script_error() {
        let state = Arc::new(StateStore::default());
        let load = "function() lur.async.sleep(300)\n\
             local t = {} for i = 1, 1e8 do t[i] = string.rep('x', 64) .. i end end";
        let leader = {
            let state = state.clone();
            let script = format!("lur.state.fetch('k', {O}, {load})");
            std::thread::spawn(move || {
                let rt = Runtime::with_config(RuntimeConfig {
                    state,
                    memory_limit: 8 * 1024 * 1024,
                    ..Default::default()
                })
                .expect("runtime builds");
                rt.run_with_timeout(&script, secs(10))
            })
        };
        // Waiters run without a memory limit: their own VM never ran out, so
        // the leader's memory error reaches them as a plain script error.
        // Their own loader must never run; if timing slips and one does, it
        // fails cleanly instead of allocating without a limit.
        let waiter =
            format!("lur.state.fetch('k', {O}, function() error('a waiter ran the loader') end)");
        let waiters: Vec<_> = (0..3)
            .map(|_| spawn(&state, waiter.clone(), ms(100), secs(10)))
            .collect();
        let led = leader.join().unwrap();
        assert!(matches!(led, Err(RunError::OutOfMemory)), "{led:?}");
        for w in waiters {
            match w.join().unwrap() {
                Err(RunError::Script(e)) => assert!(e.to_string().contains("memory"), "{e}"),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(state.in_flight(), 0);
    }

    #[test]
    fn a_timed_out_handler_hands_off_on_the_server_pool() {
        let state = Arc::new(StateStore::default());
        let app = format!(
            "lur.serve.http('GET', '/', function()\n\
               local v = lur.state.fetch('k', {O}, {})\n\
               return {{ status = 200, body = v }}\n\
             end)",
            counted("lur.async.sleep(5000)", "return 'v'")
        );
        let server = Arc::new(
            Server::load(
                &app,
                RuntimeConfig {
                    pool_size: 2,
                    state: state.clone(),
                    per_event_timeout: Some(ms(300)),
                    ..Default::default()
                },
            )
            .expect("app loads"),
        );
        let leader = {
            let server = server.clone();
            std::thread::spawn(move || server.dispatch("GET", "/", b"").unwrap().status)
        };
        std::thread::sleep(ms(100));
        let waiter = server.dispatch("GET", "/", b"").unwrap();
        assert_eq!(leader.join().unwrap(), 503, "the leader timed out");
        assert_eq!(waiter.status, 200);
        assert_eq!(waiter.body, b"v", "the waiter took over");
        check(&state, "assert(lur.state.get('runs') == 2)");
        assert_eq!(state.in_flight(), 0);
    }

    /// A tiny LCG so the stress test is reproducible without a dependency.
    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % n
        }
    }

    #[test]
    fn random_cancellation_never_leaves_a_waiter_stuck() {
        let state = Arc::new(StateStore::default());
        let handles: Vec<_> = (0..12u64)
            .map(|t| {
                let state = state.clone();
                std::thread::spawn(move || {
                    let rt = vm(&state);
                    let mut rng = Rng(t + 1);
                    for _ in 0..25 {
                        // Leaders and waiters are cut off at random points:
                        // by their own timeout, by losing a race, or by the
                        // loader failing; some loaders spin on the CPU.
                        let key = rng.below(3);
                        let work = match rng.below(4) {
                            0 => format!("lur.async.sleep({})", rng.below(40)),
                            1 => "error('flaky')".to_owned(),
                            2 => format!(
                                "local t = os.clock() + {} / 1000 while os.clock() < t do end",
                                rng.below(30)
                            ),
                            _ => String::new(),
                        };
                        let script = format!(
                            "pcall(lur.async.race, {{\n\
                               function() return lur.state.fetch('k{key}', {{ ttl_ms = 1 }},\n\
                                 function() {work} return 1 end) end,\n\
                               function() lur.async.sleep({}) end,\n\
                             }})",
                            rng.below(40)
                        );
                        let _ = rt.run_with_timeout(&script, ms(5 + rng.below(40)));
                    }
                    // Nothing left behind may block a fresh caller. Other
                    // threads may still be loading, so joining a 'flaky'
                    // flight shares its failure: retry until a value lands.
                    rt.run_with_timeout(
                        "for i = 0, 2 do\n\
                           repeat\n\
                             local ok, v = pcall(lur.state.fetch, 'k' .. i, { ttl_ms = 1 }, function() return 1 end)\n\
                             assert(ok or tostring(v):find('flaky'), tostring(v))\n\
                           until ok and v == 1\n\
                         end",
                        secs(5),
                    )
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().expect("no caller is stuck");
        }
        assert_eq!(state.in_flight(), 0, "every flight was removed");
    }

    #[test]
    fn the_server_pool_shares_one_load_across_vms() {
        let state = Arc::new(StateStore::default());
        let app = "lur.serve.http('GET', '/', function()\n\
               local v, s = lur.state.fetch('page', { ttl_ms = 300 }, function()\n\
                 lur.state.incr('runs')\n\
                 lur.async.sleep(200)\n\
                 return 'body'\n\
               end)\n\
               return { status = 200, body = v .. ':' .. s }\n\
             end)";
        let server = Arc::new(
            Server::load(
                app,
                RuntimeConfig {
                    pool_size: 4,
                    state: state.clone(),
                    per_event_timeout: Some(secs(10)),
                    ..Default::default()
                },
            )
            .expect("app loads"),
        );
        let round = || {
            let barrier = Arc::new(Barrier::new(4));
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let (server, barrier) = (server.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        let r = server.dispatch("GET", "/", b"").expect("dispatches");
                        String::from_utf8(r.body).unwrap()
                    })
                })
                .collect();
            let mut bodies: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            bodies.sort();
            bodies
        };
        assert_eq!(round(), ["body:hit", "body:hit", "body:hit", "body:miss"]);
        assert_eq!(state.in_flight(), 0);
        std::thread::sleep(ms(400)); // expire it
        assert_eq!(round(), ["body:hit", "body:hit", "body:hit", "body:miss"]);
        check(&state, "assert(lur.state.get('runs') == 2)");
        assert_eq!(state.in_flight(), 0);
    }
}
