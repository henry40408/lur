use std::time::{Duration, Instant};

use lur::runtime::Runtime;

#[test]
fn all_returns_results_in_order() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "local r = lur.async.all({\n\
         \tfunction() return 'a' end,\n\
         \tfunction() return 'b' end,\n\
         \tfunction() return 'c' end,\n\
         })\n\
         assert(#r == 3, 'three results')\n\
         assert(r[1] == 'a' and r[2] == 'b' and r[3] == 'c', 'in argument order')",
    )
    .expect("all works");
}

#[test]
fn all_re_raises_on_first_error() {
    let rt = Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.async.all({ function() return 1 end, function() error('boom') end })")
            .is_err(),
        "all must re-raise a task error"
    );
}

#[test]
fn settled_never_raises_and_reports_each() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "local r = lur.async.settled({\n\
         \tfunction() return 'ok' end,\n\
         \tfunction() error('boom') end,\n\
         })\n\
         assert(r[1].ok == true and r[1].value == 'ok', 'first settled ok')\n\
         assert(r[2].ok == false, 'second settled as failed')\n\
         assert(string.find(r[2].err, 'boom') ~= nil, 'err carries the message')",
    )
    .expect("settled works");
}

#[test]
fn race_returns_first_to_settle() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "local r = lur.async.race({\n\
         \tfunction() lur.async.sleep(200) return 'slow' end,\n\
         \tfunction() return 'fast' end,\n\
         })\n\
         assert(r == 'fast', 'fast wins, got ' .. tostring(r))",
    )
    .expect("race works");
}

#[test]
fn any_returns_first_success_skipping_failures() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "local r = lur.async.any({\n\
         \tfunction() error('first fails') end,\n\
         \tfunction() return 'second' end,\n\
         })\n\
         assert(r == 'second', 'first success wins, got ' .. tostring(r))",
    )
    .expect("any works");
}

#[test]
fn any_raises_aggregate_when_all_fail() {
    let rt = Runtime::new().expect("runtime builds");
    assert!(
        rt.run("lur.async.any({ function() error('a') end, function() error('b') end })")
            .is_err(),
        "any must raise when every task fails"
    );
}

#[test]
fn all_runs_tasks_concurrently() {
    let rt = Runtime::new().expect("runtime builds");
    let start = Instant::now();
    rt.run(
        "lur.async.all({\n\
         \tfunction() lur.async.sleep(150) end,\n\
         \tfunction() lur.async.sleep(150) end,\n\
         \tfunction() lur.async.sleep(150) end,\n\
         })",
    )
    .expect("all of sleeps works");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(400),
        "three 150ms tasks should run concurrently (~150ms), took {elapsed:?}"
    );
}

fn run_ok(src: &str) {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect("script succeeds");
}

fn run_err(src: &str) -> String {
    Runtime::new()
        .expect("runtime builds")
        .run(src)
        .expect_err("script must fail")
        .to_string()
}

#[test]
fn map_returns_results_in_input_order_with_index() {
    // Earlier items sleep longer, so completion order is the reverse.
    run_ok(
        "local r = lur.async.map({ 'a', 'b', 'c' }, function(item, i)\n\
         \tlur.async.sleep((4 - i) * 20)\n\
         \treturn item .. i\n\
         end)\n\
         assert(#r == 3 and r[1] == 'a1' and r[2] == 'b2' and r[3] == 'c3', 'ordered')",
    );
}

#[test]
fn map_runs_concurrently_when_unbounded() {
    let start = Instant::now();
    run_ok("lur.async.map({ 1, 2, 3, 4 }, function() lur.async.sleep(150) end)");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "four 150ms items should overlap, took {elapsed:?}"
    );
}

#[test]
fn map_limit_bounds_in_flight_tasks() {
    run_ok(
        "local cur, peak = 0, 0\n\
         local r = lur.async.map({ 1, 2, 3, 4, 5, 6, 7 }, function(x)\n\
         \tcur = cur + 1\n\
         \tif cur > peak then peak = cur end\n\
         \tlur.async.sleep(20)\n\
         \tcur = cur - 1\n\
         \treturn x * 2\n\
         end, { limit = 2 })\n\
         assert(peak == 2, 'peak should be exactly 2, got ' .. peak)\n\
         assert(#r == 7 and r[1] == 2 and r[7] == 14, 'results ordered')",
    );
}

#[test]
fn map_limit_composes_with_global_cap_without_deadlock() {
    let rt = Runtime::with_config(lur::runtime::RuntimeConfig {
        max_concurrency: Some(2),
        ..Default::default()
    })
    .expect("runtime builds");
    rt.run_with_timeout(
        "local cur, peak = 0, 0\n\
         local r = lur.async.map({ 1, 2, 3, 4, 5, 6 }, function(x)\n\
         \tcur = cur + 1\n\
         \tif cur > peak then peak = cur end\n\
         \tlur.async.sleep(10)\n\
         \tcur = cur - 1\n\
         \treturn x\n\
         end, { limit = 4 })\n\
         assert(#r == 6, 'all items ran')\n\
         assert(peak == 2, 'global cap of 2 wins, got ' .. peak)",
        Duration::from_secs(5),
    )
    .expect("map under global cap");
}

#[test]
fn map_fails_fast_and_cancels_remaining() {
    run_ok(
        "local started = 0\n\
         local ok, err = pcall(lur.async.map, { 1, 2, 3 }, function(x)\n\
         \tstarted = started + 1\n\
         \tif x == 1 then error('boom') end\n\
         \tlur.async.sleep(100)\n\
         \tstarted = started + 100\n\
         end)\n\
         assert(not ok and string.find(tostring(err), 'boom'), 'error propagates')\n\
         lur.async.sleep(200)\n\
         assert(started < 100, 'remaining tasks were cancelled, started=' .. started)",
    );
}

#[test]
fn map_fail_fast_with_limit_does_not_start_queued_items() {
    run_ok(
        "local ran = {}\n\
         local ok = pcall(lur.async.map, { 1, 2, 3, 4 }, function(x)\n\
         \tran[#ran + 1] = x\n\
         \tlur.async.sleep(10)\n\
         \tif x == 1 then error('boom') end\n\
         end, { limit = 1 })\n\
         assert(not ok, 'raised')\n\
         lur.async.sleep(100)\n\
         assert(#ran == 1, 'queued items never ran, ran=' .. #ran)",
    );
}

#[test]
fn map_drop_errors_omits_failures_and_keeps_order() {
    run_ok(
        "local r = lur.async.map({ 1, 2, 3, 4, 5 }, function(x)\n\
         \tlur.async.sleep((6 - x) * 5)\n\
         \tif x % 2 == 0 then error('even') end\n\
         \treturn x * 10\n\
         end, { drop_errors = true })\n\
         assert(#r == 3 and r[1] == 10 and r[2] == 30 and r[3] == 50, 'dense and ordered')",
    );
}

#[test]
fn map_drop_errors_false_is_fail_fast() {
    let msg = run_err(
        "lur.async.map({ 1, 2 }, function(x) if x == 2 then error('bad') end end,\n\
         { drop_errors = false })",
    );
    assert!(msg.contains("bad"), "{msg}");
}

#[test]
fn map_empty_list_returns_empty_table() {
    run_ok(
        "local r = lur.async.map({}, function() error('never') end, { limit = 3 })\n\
         assert(type(r) == 'table' and #r == 0 and next(r) == nil)",
    );
}

#[test]
fn map_accepts_huge_valid_limit_and_rejects_math_huge() {
    run_ok(
        "local r = lur.async.map({ 1, 2, 3 }, function(x) return x end,\n\
         { limit = 2^62 })\n\
         assert(#r == 3 and r[3] == 3, 'huge limit works')\n\
         local r2 = lur.async.map({ 1, 2 }, function(x) return x end,\n\
         { limit = math.maxinteger })\n\
         assert(#r2 == 2)",
    );
    let msg = run_err("lur.async.map({ 1 }, function() end, { limit = math.huge })");
    assert!(msg.contains("opts.limit"), "{msg}");
}

#[test]
fn map_fail_fast_is_immediate_beyond_thirty_items() {
    let start = Instant::now();
    run_ok(
        "local started = 0\n\
         local ok, err = pcall(lur.async.map, (function()\n\
         \tlocal t = {} for i = 1, 40 do t[i] = i end return t end)(),\n\
         \tfunction(x)\n\
         \t\tstarted = started + 1\n\
         \t\tif x == 1 then lur.async.sleep(1000) return x end\n\
         \t\tif x == 2 then error('boom') end\n\
         \t\tlur.async.sleep(5)\n\
         \t\treturn x\n\
         \tend, { limit = 3 })\n\
         assert(not ok and string.find(tostring(err), 'boom'), 'error propagates')\n\
         assert(started < 10, 'queued items were not started, started=' .. started)",
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(700),
        "error must not wait for the sleeping item, took {elapsed:?}"
    );
}

#[test]
fn all_fail_fast_is_immediate_beyond_thirty_tasks() {
    let start = Instant::now();
    run_ok(
        "local tasks = {}\n\
         tasks[1] = function() lur.async.sleep(1000) return 1 end\n\
         tasks[2] = function() error('boom') end\n\
         for i = 3, 40 do tasks[i] = function() lur.async.sleep(1000) return i end end\n\
         local ok, err = pcall(lur.async.all, tasks)\n\
         assert(not ok and string.find(tostring(err), 'boom'), 'error propagates')",
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(700),
        "error must not wait for earlier sleeping tasks, took {elapsed:?}"
    );
}

#[test]
fn all_under_global_cap_keeps_order_and_does_not_start_queued_after_error() {
    let rt = Runtime::with_config(lur::runtime::RuntimeConfig {
        max_concurrency: Some(2),
        ..Default::default()
    })
    .expect("runtime builds");
    rt.run_with_timeout(
        "local started, peak, cur = 0, 0, 0\n\
         local tasks = {}\n\
         for i = 1, 40 do tasks[i] = function()\n\
         \tstarted = started + 1\n\
         \tcur = cur + 1\n\
         \tif cur > peak then peak = cur end\n\
         \tlur.async.sleep(2)\n\
         \tcur = cur - 1\n\
         \treturn i\n\
         end end\n\
         local r = lur.async.all(tasks)\n\
         assert(#r == 40 and r[1] == 1 and r[40] == 40, 'ordered results')\n\
         assert(peak == 2, 'global cap holds, got ' .. peak)\n\
         started = 0\n\
         tasks[1] = function() started = started + 1 lur.async.sleep(1000) return 1 end\n\
         tasks[2] = function() started = started + 1 error('boom') end\n\
         local t0 = lur.time.monotonic_ms()\n\
         local ok = pcall(lur.async.all, tasks)\n\
         assert(not ok, 'error propagates')\n\
         assert(lur.time.monotonic_ms() - t0 < 500, 'error must not wait behind task 1')\n\
         assert(started <= 3, 'queued tasks not started, started=' .. started)",
        Duration::from_secs(5),
    )
    .expect("all under global cap");
}

#[test]
fn map_rejects_bad_arguments() {
    let cases = [
        ("lur.async.map(1, function() end)", "argument #1"),
        ("lur.async.map({ a = 1 }, function() end)", "argument #1"),
        ("lur.async.map({ 1 }, 'x')", "argument #2"),
        ("lur.async.map({ 1 }, function() end, 5)", "argument #3"),
        (
            "lur.async.map({ 1 }, function() end, { limit = 0 })",
            "opts.limit",
        ),
        (
            "lur.async.map({ 1 }, function() end, { limit = 1.5 })",
            "argument #3",
        ),
        (
            "lur.async.map({ 1 }, function() end, { limit = 'x' })",
            "argument #3",
        ),
        (
            "lur.async.map({ 1 }, function() end, { drop_errors = 1 })",
            "opts.drop_errors",
        ),
    ];
    for (src, want) in cases {
        let msg = run_err(src);
        assert!(
            msg.contains("lur.async.map") && msg.contains(want),
            "{src}: unexpected message {msg}"
        );
    }
}

// Error tables (`lur.validate`, `lur.serve.abort`) raised in a task reach the
// combinator's caller as the same table.

/// `bad()` raises a validation error table; `kind(e)` is its kind or the type.
const TABLE_ERR_PRELUDE: &str = "local v = lur.validate\n\
     local function bad(s) return v.parse(v.int(), s or 'x') end\n\
     local function kind(e) return type(e) == 'table' and e.kind or type(e) end\n";

fn run_table_err(src: &str) {
    run_ok(&format!("{TABLE_ERR_PRELUDE}{src}"));
}

#[test]
fn validation_error_table_survives_all_race_map() {
    run_table_err(
        "local ok, e = pcall(lur.async.all, { function() return 1 end, function() return bad() end })\n\
         assert(not ok and kind(e) == 'validation', kind(e))\n\
         assert(e.issues[1].path == '' and tostring(e) == 'validation failed: must be an integer (got \"x\")', tostring(e))\n\
         ok, e = pcall(lur.async.race, { function() return bad() end, function() lur.async.sleep(50) return 1 end })\n\
         assert(not ok and kind(e) == 'validation', 'race: ' .. kind(e))\n\
         ok, e = pcall(lur.async.map, { '1', 'y' }, function(s) return bad(s) end)\n\
         assert(not ok and kind(e) == 'validation', 'map: ' .. kind(e))\n\
         assert(e.issues[1].message == 'must be an integer (got \"y\")', e.issues[1].message)\n\
         -- after a yield inside the task too\n\
         ok, e = pcall(lur.async.all, { function() lur.async.sleep(5) return bad() end })\n\
         assert(not ok and kind(e) == 'validation', 'after sleep: ' .. kind(e))",
    );
}

#[test]
fn settled_puts_the_error_table_in_err() {
    run_table_err(
        "local r = lur.async.settled({\n\
         \tfunction() return bad() end,\n\
         \tfunction() error('boom') end,\n\
         \tfunction() return 3 end,\n\
         })\n\
         assert(r[1].ok == false and kind(r[1].err) == 'validation', kind(r[1].err))\n\
         assert(r[2].ok == false and type(r[2].err) == 'string' and r[2].err:find('boom'))\n\
         assert(r[3].ok and r[3].value == 3)",
    );
}

#[test]
fn any_still_aggregates_failures_as_text() {
    run_table_err(
        "assert(lur.async.any({ function() return bad() end, function() return 2 end }) == 2)\n\
         local ok, e = pcall(lur.async.any, { function() return bad() end, function() return bad('z') end })\n\
         assert(not ok and type(e) ~= 'table')\n\
         local s = tostring(e)\n\
         assert(s:find('all 2 tasks failed', 1, true) and s:find('(got \"z\")', 1, true), s)",
    );
}

#[test]
fn error_table_survives_nested_combinators() {
    run_table_err(
        "local ok, e = pcall(lur.async.map, { 1, 2 }, function(i)\n\
         \treturn lur.async.race({ function()\n\
         \t\treturn lur.async.all({ function() lur.async.sleep(i) if i == 2 then bad('n') end return i end })\n\
         \tend })\n\
         end)\n\
         assert(not ok and kind(e) == 'validation', kind(e))\n\
         assert(e.issues[1].message == 'must be an integer (got \"n\")')\n\
         local r = lur.async.settled({ function() return lur.async.all({ function() return bad() end }) end })\n\
         assert(kind(r[1].err) == 'validation')",
    );
}

#[test]
fn caught_then_reraised_table_survives() {
    run_table_err(
        "local ok, e = pcall(lur.async.all, { function()\n\
         \tlocal ok, err = pcall(bad)\n\
         \tlur.async.sleep(5)\n\
         \terror(err)\n\
         end, function() local _ = pcall(bad, 'other') return 1 end })\n\
         assert(not ok and kind(e) == 'validation', kind(e))\n\
         assert(e.issues[1].message == 'must be an integer (got \"x\")', e.issues[1].message)",
    );
}

#[test]
fn non_table_errors_are_unchanged() {
    run_table_err(
        "local ok, e = pcall(lur.async.all, { function() error('plain') end })\n\
         assert(not ok and typeof(e) == 'error' and tostring(e):find('plain'), typeof(e))\n\
         -- a plain table without lur's error metatable still arrives flattened\n\
         ok, e = pcall(lur.async.all, { function() error({ code = 1 }) end })\n\
         assert(not ok and typeof(e) == 'error', typeof(e))\n\
         -- a task that rendered an error table, then raised a string\n\
         ok, e = pcall(lur.async.all, { function()\n\
         \tlocal _, err = pcall(bad)\n\
         \terror('other: ' .. tostring(err))\n\
         end })\n\
         assert(not ok and typeof(e) == 'error' and tostring(e):find('other: validation failed', 1, true))\n\
         -- a recorded table is handed back at most once: a dropped map error\n\
         -- does not turn a later string error with the same text into a table\n\
         local r = lur.async.map({ 'x' }, function(s) return bad(s) end, { drop_errors = true })\n\
         assert(#r == 0)\n\
         ok, e = pcall(lur.async.all, { function() error('validation failed: must be an integer (got \"x\")', 0) end })\n\
         assert(not ok and typeof(e) == 'error', typeof(e))",
    );
}

#[test]
fn uncaught_error_table_from_a_task_reports_its_message() {
    let msg = run_err(&format!(
        "{TABLE_ERR_PRELUDE}lur.async.all({{ function() return bad() end }})"
    ));
    assert!(
        msg.contains("validation failed: must be an integer (got \"x\")"),
        "{msg}"
    );
}
