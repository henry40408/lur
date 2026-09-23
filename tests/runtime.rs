use std::time::Duration;

use lur::runtime::{RunError, Runtime};

#[test]
fn runs_a_trivial_script_to_completion() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run("local x = 1 + 1").expect("script runs");
}

#[test]
fn require_is_removed_from_the_sandbox() {
    let rt = Runtime::new().expect("runtime builds");
    // `sandbox(true)` keeps `require`, which reads disk and bypasses lur.fs.
    rt.run("assert(require == nil, 'require must be removed')")
        .expect("require should be nil");
}

#[test]
fn global_env_escapes_are_removed_from_the_sandbox() {
    let rt = Runtime::new().expect("runtime builds");
    // Each reaches the writable global env, bypassing per-request isolation (spec §3/§5).
    rt.run(
        "assert(getfenv == nil, 'getfenv must be removed')\n\
         assert(setfenv == nil, 'setfenv must be removed')\n\
         assert(loadstring == nil, 'loadstring must be removed')",
    )
    .expect("env-escape globals should be nil");
}

#[test]
fn dangerous_lua_stdlib_is_absent_under_strict() {
    let rt = Runtime::new().expect("runtime builds");
    // Lua 5.x escape hatches are absent (spec §9 sandbox blocking).
    rt.run(
        "for _, name in ipairs({ 'io', 'package', 'loadfile', 'dofile', 'load' }) do\n\
         \tassert(_G[name] == nil, name .. ' must be absent')\n\
         end\n\
         assert(type(os) == 'table', 'os table present')\n\
         assert(os.execute == nil, 'os.execute must be absent')\n\
         assert(type(os.time) == 'function', 'safe os.time kept')",
    )
    .expect("dangerous stdlib absent, safe os.* kept");
}

#[test]
fn readonly_globals_reject_raw_writes() {
    let rt = Runtime::new().expect("runtime builds");
    // Frozen globals must resist even `rawset`, or pooled VMs leak across requests (spec §3).
    rt.run(
        "assert(not pcall(function() rawset(_G, 'INJECTED', 1) end),\n\
         \t'rawset must not bypass the readonly global table')\n\
         assert(rawget(_G, 'INJECTED') == nil, 'no global was injected')",
    )
    .expect("rawset on the global table is rejected");
}

#[test]
fn debug_escape_members_are_absent_but_traceback_kept() {
    let rt = Runtime::new().expect("runtime builds");
    // Only harmless debug members (traceback) remain; upvalue/registry access is gone (spec §9).
    rt.run(
        "assert(type(debug) == 'table', 'debug table present')\n\
         for _, name in ipairs({ 'getupvalue', 'setupvalue', 'getregistry', 'getinfo' }) do\n\
         \tassert(debug[name] == nil, 'debug.' .. name .. ' must be absent')\n\
         end\n\
         assert(type(debug.traceback) == 'function', 'debug.traceback kept')\n\
         assert(string.dump == nil, 'string.dump must be absent')",
    )
    .expect("debug escape members absent, traceback kept");
}

#[test]
fn lur_log_exposes_level_functions() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "assert(type(lur) == 'table', 'lur table missing')\n\
         assert(type(lur.log) == 'table', 'lur.log is a table')\n\
         assert(type(lur.log.info) == 'function', 'info missing')\n\
         assert(type(lur.log.warn) == 'function', 'warn missing')\n\
         assert(type(lur.log.error) == 'function', 'error missing')\n\
         lur.log.info('hi'); lur.log.warn('w'); lur.log.error('e')",
    )
    .expect("lur.log level loggers callable post-sandbox");
}

#[test]
fn memory_limit_aborts_a_runaway_allocation() {
    let rt = Runtime::with_memory_limit(2 * 1024 * 1024).expect("runtime builds");
    let err = rt
        .run("local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 1024) end")
        .expect_err("runaway allocation must be stopped");
    assert!(matches!(err, RunError::OutOfMemory), "got {err:?}");
}

#[test]
fn exit_code_maps_a_returned_number() {
    let rt = Runtime::new().expect("runtime builds");
    assert_eq!(rt.run_to_exit_code("return 3", None).unwrap(), 3);
}

#[test]
fn exit_code_is_one_for_a_falsy_return() {
    let rt = Runtime::new().expect("runtime builds");
    assert_eq!(rt.run_to_exit_code("return nil", None).unwrap(), 1);
    assert_eq!(rt.run_to_exit_code("return false", None).unwrap(), 1);
}

#[test]
fn exit_code_is_zero_with_no_return() {
    let rt = Runtime::new().expect("runtime builds");
    assert_eq!(rt.run_to_exit_code("local x = 1", None).unwrap(), 0);
}

#[test]
fn exit_code_propagates_timeout() {
    let rt = Runtime::new().expect("runtime builds");
    let err = rt
        .run_to_exit_code("while true do end", Some(Duration::from_millis(50)))
        .expect_err("infinite loop must time out");
    assert!(matches!(err, RunError::Timeout), "got {err:?}");
}

#[test]
fn async_sleep_completes_within_budget() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run_with_timeout("lur.async.sleep(10)", Duration::from_millis(500))
        .expect("a short sleep completes");
}

#[test]
fn io_park_is_killed_by_the_wall_clock_layer() {
    // Parked on sleep, the interrupt can't fire; only the wall-clock layer can (spec §5).
    let rt = Runtime::new().expect("runtime builds");
    let started = std::time::Instant::now();
    let err = rt
        .run_with_timeout("lur.async.sleep(5000)", Duration::from_millis(50))
        .expect_err("a sleep past the deadline must be cut off");
    assert!(matches!(err, RunError::Timeout), "got {err:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "must be cut at the deadline, not after the full sleep"
    );
}

#[test]
fn deadline_interrupt_aborts_an_infinite_loop() {
    let rt = Runtime::new().expect("runtime builds");
    let err = rt
        .run_with_timeout("while true do end", Duration::from_millis(50))
        .expect_err("infinite loop must be interrupted");
    assert!(matches!(err, RunError::Timeout), "got {err:?}");
}

#[test]
fn timeout_cannot_be_swallowed_by_a_pcall_loop() {
    let rt = Runtime::new().expect("runtime builds");
    // Keep-raising: re-entering pcall forever still can't outlive the deadline.
    let err = rt
        .run_with_timeout(
            "while true do pcall(function() while true do end end) end",
            Duration::from_millis(50),
        )
        .expect_err("pcall-loop must not outlive the deadline");
    assert!(matches!(err, RunError::Timeout), "got {err:?}");
}
