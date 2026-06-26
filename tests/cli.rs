use std::path::PathBuf;

use assert_cmd::Command;
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
fn out_of_memory_exits_137() {
    lur()
        .arg(fixture("alloc.lua"))
        .arg("--max-memory")
        .arg("2097152") // 2 MiB
        .assert()
        .code(137);
}

#[test]
fn stdout_write_emits_raw_bytes_without_newline() {
    lur()
        .arg(fixture("stdout.lua"))
        .assert()
        .code(0)
        .stdout(predicate::eq("ABC"));
}

#[test]
fn stdin_read_all_returns_every_byte() {
    lur()
        .arg(fixture("stdin_all.lua"))
        .write_stdin("hello world")
        .assert()
        .code(0)
        .stdout(predicate::eq("hello world"));
}

#[test]
fn stdin_read_n_returns_up_to_n_bytes() {
    lur()
        .arg(fixture("stdin_n.lua"))
        .write_stdin("abcdef")
        .assert()
        .code(0)
        .stdout(predicate::eq("abc"));
}

#[test]
fn stdin_lines_iterates_newline_stripped_lines() {
    lur()
        .arg(fixture("stdin_lines.lua"))
        .write_stdin("a\nb\nc\n")
        .assert()
        .code(0)
        .stdout(predicate::eq("a,b,c"));
}

#[test]
fn script_args_expose_flags_and_positional() {
    lur()
        .arg(fixture("args.lua"))
        .args(["--name", "alice", "--mode=fast", "input.txt", "--verbose"])
        .assert()
        .code(0);
}

#[test]
fn fs_read_works_with_an_allow_fs_read_grant() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, b"granted").unwrap();

    lur()
        .arg("--allow-fs-read")
        .arg(dir.path())
        .arg(fixture("read_arg.lua"))
        .arg(&f)
        .assert()
        .code(0)
        .stdout(predicate::eq("granted"));
}

#[test]
fn fs_read_is_denied_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, b"secret").unwrap();

    // No grant: strict default denies, the script errors out.
    lur().arg(fixture("read_arg.lua")).arg(&f).assert().code(1);
}

#[test]
fn env_returns_value_when_allowlisted() {
    lur()
        .arg("--allow-env")
        .arg("LUR_TEST_VAR")
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "secret-value")
        .assert()
        .code(0)
        .stdout(predicate::eq("secret-value"));
}

#[test]
fn env_returns_nil_when_not_allowlisted() {
    // Set but not granted → nil (indistinguishable from unset; oracle-proof).
    lur()
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "secret-value")
        .assert()
        .code(0)
        .stdout(predicate::eq("nil"));
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
