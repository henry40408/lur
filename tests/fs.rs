use std::fs;
use std::sync::Arc;

use lur::policy::Policy;
use lur::runtime::{Runtime, RuntimeConfig};

fn runtime_with(policy: Policy) -> Runtime {
    Runtime::with_config(RuntimeConfig {
        policy: Arc::new(policy),
        ..Default::default()
    })
    .expect("runtime builds")
}

/// A Lua string literal for a path (temp paths have no special chars).
fn lit(p: &std::path::Path) -> String {
    format!("{:?}", p.to_str().unwrap())
}

#[test]
fn fs_read_returns_bytes_of_a_granted_file() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("data.txt");
    fs::write(&f, b"contents").unwrap();

    let rt = runtime_with(Policy::from_roots(&[dir.path().to_path_buf()], &[]).unwrap());
    rt.run(&format!(
        "assert(lur.fs.read({}) == 'contents', 'read content')",
        lit(&f)
    ))
    .expect("granted read works");
}

#[test]
fn fs_read_is_denied_outside_the_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("data.txt");
    fs::write(&f, b"x").unwrap();

    let rt = runtime_with(Policy::strict());
    assert!(
        rt.run(&format!("lur.fs.read({})", lit(&f))).is_err(),
        "ungranted read must error"
    );
}

#[test]
fn fs_write_creates_a_file_in_a_granted_dir() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("out.txt");

    let rt = runtime_with(Policy::from_roots(&[], &[dir.path().to_path_buf()]).unwrap());
    rt.run(&format!("lur.fs.write({}, 'written')", lit(&f)))
        .expect("granted write works");
    assert_eq!(fs::read(&f).unwrap(), b"written");
}

#[test]
fn fs_write_is_denied_with_only_read_grant() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("out.txt");

    // Read grant on the dir must not permit writing (lists are separate).
    let rt = runtime_with(Policy::from_roots(&[dir.path().to_path_buf()], &[]).unwrap());
    assert!(
        rt.run(&format!("lur.fs.write({}, 'x')", lit(&f))).is_err(),
        "write with only a read grant must error"
    );
    assert!(!f.exists(), "denied write must not create the file");
}
