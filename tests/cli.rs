use std::path::PathBuf;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn lur() -> Command {
    Command::cargo_bin("lur").expect("binary builds")
}

#[test]
fn runs_a_script_and_exits_zero() {
    lur().arg(fixture("ok.lua")).assert().code(0);
}

#[test]
fn maps_returned_number_to_exit_code() {
    lur().arg(fixture("exit3.lua")).assert().code(3);
}

#[test]
fn script_error_exits_one_with_traceback() {
    lur()
        .arg(fixture("raise.lua"))
        .assert()
        .code(1)
        .stderr(predicate::str::contains("boom"));
}

#[test]
fn timeout_exits_124() {
    lur()
        .arg(fixture("loop.lua"))
        .arg("--timeout-ms")
        .arg("50")
        .assert()
        .code(124);
}

#[test]
fn lur_log_reaches_stderr() {
    lur()
        .arg(fixture("log.lua"))
        .assert()
        .code(0)
        .stderr(predicate::str::contains("hello from script"));
}

#[test]
fn missing_script_exits_with_a_clear_error() {
    lur()
        .arg(fixture("does-not-exist.lua"))
        .assert()
        .code(2)
        .stderr(predicate::str::contains("does-not-exist.lua"));
}
