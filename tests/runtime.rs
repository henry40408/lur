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
    // `sandbox(true)` alone leaves `require`, which loads on-disk .luau files
    // and bypasses lur.fs. The global must be gone entirely.
    rt.run("assert(require == nil, 'require must be removed')")
        .expect("require should be nil");
}

#[test]
fn lur_log_is_injected_and_callable() {
    let rt = Runtime::new().expect("runtime builds");
    rt.run(
        "assert(type(lur) == 'table', 'lur table missing')\n\
         assert(type(lur.log) == 'function', 'lur.log missing')\n\
         lur.log('hello from lua')",
    )
    .expect("lur.log is injected and callable post-sandbox");
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
    // The whole point of keep-raising: a script that re-enters pcall forever
    // still cannot outlive the deadline.
    let err = rt
        .run_with_timeout(
            "while true do pcall(function() while true do end end) end",
            Duration::from_millis(50),
        )
        .expect_err("pcall-loop must not outlive the deadline");
    assert!(matches!(err, RunError::Timeout), "got {err:?}");
}
