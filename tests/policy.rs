use std::fs;

use lur::policy::{Policy, PolicyError};
use tempfile::tempdir;

#[test]
fn read_allow_grants_subtree_and_blocks_outside() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let inside = sub.join("f.txt");
    fs::write(&inside, b"x").unwrap();
    let outside = dir.path().join("other.txt");
    fs::write(&outside, b"y").unwrap();

    let policy = Policy::from_roots(std::slice::from_ref(&sub), &[]).unwrap();
    assert!(policy.allows_read(&inside).is_ok());
    assert!(matches!(
        policy.allows_read(&outside),
        Err(PolicyError::Denied { .. })
    ));
}

#[test]
fn read_allow_blocks_dotdot_escape() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let secret = dir.path().join("secret.txt");
    fs::write(&secret, b"s").unwrap();

    let policy = Policy::from_roots(std::slice::from_ref(&sub), &[]).unwrap();
    // sub/../secret.txt canonicalizes to dir/secret.txt, outside the granted sub.
    let escape = sub.join("../secret.txt");
    assert!(matches!(
        policy.allows_read(&escape),
        Err(PolicyError::Denied { .. })
    ));
}

#[test]
fn file_grant_is_exact_not_prefix() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    fs::write(&a, b"a").unwrap();
    fs::write(&b, b"b").unwrap();

    let policy = Policy::from_roots(std::slice::from_ref(&a), &[]).unwrap();
    assert!(policy.allows_read(&a).is_ok());
    assert!(matches!(
        policy.allows_read(&b),
        Err(PolicyError::Denied { .. })
    ));
}

#[test]
fn read_and_write_allowlists_are_separate() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("f.txt");
    fs::write(&f, b"x").unwrap();

    let policy = Policy::from_roots(&[dir.path().to_path_buf()], &[]).unwrap();
    assert!(policy.allows_read(&f).is_ok());
    assert!(matches!(
        policy.allows_write(&f),
        Err(PolicyError::Denied { .. })
    ));
}

#[test]
fn write_to_a_new_file_canonicalizes_its_parent() {
    let dir = tempdir().unwrap();
    let policy = Policy::from_roots(&[], &[dir.path().to_path_buf()]).unwrap();
    let new_file = dir.path().join("new.txt"); // does not exist yet
    assert!(policy.allows_write(&new_file).is_ok());
}

#[test]
fn env_allowlist_is_exact() {
    let p = Policy::strict().with_env(vec!["API_KEY".to_string()]);
    assert!(p.allows_env("API_KEY"));
    assert!(!p.allows_env("OTHER"));
    assert!(!Policy::strict().allows_env("API_KEY"));
}

#[test]
fn strict_policy_denies_everything() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("f.txt");
    fs::write(&f, b"x").unwrap();

    let policy = Policy::strict();
    assert!(matches!(
        policy.allows_read(&f),
        Err(PolicyError::Denied { .. })
    ));
    assert!(matches!(
        policy.allows_write(&f),
        Err(PolicyError::Denied { .. })
    ));
}
