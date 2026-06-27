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
