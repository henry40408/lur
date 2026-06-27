use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use assert_cmd::Command;
use predicates::prelude::*;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A stable empty XDG config dir, so the binary's default-location config
/// discovery never bleeds the developer's real `~/.config/lur/config` into a
/// test. Tests that exercise discovery override `XDG_CONFIG_HOME` themselves.
fn empty_xdg() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().unwrap()).path()
}

fn lur() -> Command {
    let mut c = Command::cargo_bin("lur").expect("binary builds");
    c.env("XDG_CONFIG_HOME", empty_xdg());
    c
}

/// Write a config file into a fresh temp dir; returns the dir (keep it alive)
/// and the file path.
fn write_config(contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    std::fs::write(&path, contents).unwrap();
    (dir, path)
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
fn loose_profile_grants_env_without_allowlist() {
    // --loose selects the permissive profile: env is readable with no --allow-env.
    lur()
        .arg("--loose")
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "secret-value")
        .assert()
        .code(0)
        .stdout(predicate::eq("secret-value"));
}

#[test]
fn loose_profile_grants_fs_read_without_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("f.txt");
    std::fs::write(&f, b"granted").unwrap();

    lur()
        .arg("--loose")
        .arg(fixture("read_arg.lua"))
        .arg(&f)
        .assert()
        .code(0)
        .stdout(predicate::eq("granted"));
}

#[test]
fn strict_and_loose_are_mutually_exclusive() {
    // Passing both is a usage error (clap exits 2).
    lur()
        .arg("--strict")
        .arg("--loose")
        .arg(fixture("ok.lua"))
        .assert()
        .code(2);
}

#[test]
fn config_env_grant_is_honored() {
    let (_d, cfg) = write_config("[allow]\nenv = [\"LUR_TEST_VAR\"]\n");
    lur()
        .arg("--config")
        .arg(&cfg)
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "secret-value")
        .assert()
        .code(0)
        .stdout(predicate::eq("secret-value"));
}

#[test]
fn config_default_profile_loose_is_permissive() {
    let (_d, cfg) = write_config("default_profile = \"loose\"\n");
    lur()
        .arg("--config")
        .arg(&cfg)
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "v")
        .assert()
        .code(0)
        .stdout(predicate::eq("v"));
}

#[test]
fn strict_flag_overrides_config_loose() {
    // Scalar settings are last-wins: an explicit --strict beats config loose.
    let (_d, cfg) = write_config("default_profile = \"loose\"\n");
    lur()
        .arg("--config")
        .arg(&cfg)
        .arg("--strict")
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "v")
        .assert()
        .code(0)
        .stdout(predicate::eq("nil"));
}

#[test]
fn config_and_flag_env_grants_are_unioned() {
    // A flag-granted var works alongside config…
    let (_d, cfg) = write_config("[allow]\nenv = [\"FROM_CONFIG\"]\n");
    lur()
        .arg("--config")
        .arg(&cfg)
        .arg("--allow-env")
        .arg("FROM_FLAG")
        .arg(fixture("env_read.lua"))
        .arg("FROM_FLAG")
        .env("FROM_FLAG", "flagval")
        .assert()
        .code(0)
        .stdout(predicate::eq("flagval"));

    // …and the config-granted var still resolves with the flag present.
    let (_d2, cfg2) = write_config("[allow]\nenv = [\"FROM_CONFIG\"]\n");
    lur()
        .arg("--config")
        .arg(&cfg2)
        .arg("--allow-env")
        .arg("FROM_FLAG")
        .arg(fixture("env_read.lua"))
        .arg("FROM_CONFIG")
        .env("FROM_CONFIG", "cfgval")
        .assert()
        .code(0)
        .stdout(predicate::eq("cfgval"));
}

#[test]
fn default_location_config_is_discovered_and_no_config_ignores_it() {
    let xdg = tempfile::tempdir().unwrap();
    let lur_dir = xdg.path().join("lur");
    std::fs::create_dir_all(&lur_dir).unwrap();
    std::fs::write(
        lur_dir.join("config"),
        "[allow]\nenv = [\"LUR_TEST_VAR\"]\n",
    )
    .unwrap();

    // Discovered at the default location → env granted.
    lur()
        .env("XDG_CONFIG_HOME", xdg.path())
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "v")
        .assert()
        .code(0)
        .stdout(predicate::eq("v"));

    // --no-config drops the config layer → nil.
    lur()
        .env("XDG_CONFIG_HOME", xdg.path())
        .arg("--no-config")
        .arg(fixture("env_read.lua"))
        .arg("LUR_TEST_VAR")
        .env("LUR_TEST_VAR", "v")
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
