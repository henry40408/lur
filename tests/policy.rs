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
fn net_allowlist_matches_host_and_port() {
    let p = Policy::strict().with_net(vec![
        "api.github.com".to_string(),
        "10.0.0.5:6379".to_string(),
    ]);
    assert!(p.allows_net("api.github.com", 443));
    assert!(p.allows_net("api.github.com", 80)); // host-only → any port
    assert!(p.allows_net("10.0.0.5", 6379));
    assert!(!p.allows_net("10.0.0.5", 5432)); // wrong port
    assert!(!p.allows_net("evil.com", 443));
    assert!(!Policy::strict().allows_net("api.github.com", 443));
}

#[test]
fn net_allowlist_matches_ipv6_by_address() {
    let p = Policy::strict().with_net(vec!["::1".to_string(), "[fd00::5]:6379".to_string()]);
    assert!(p.allows_net("[::1]", 80)); // Url::host_str form
    assert!(p.allows_net("::1", 80));
    assert!(p.allows_net("[0:0:0:0:0:0:0:1]", 80)); // same address, other spelling
    assert!(p.allows_net("[FD00::5]", 6379));
    assert!(!p.allows_net("[fd00::5]", 5432));
    assert!(!p.allows_net("[::2]", 80));
    // An IPv4 rule does not match its IPv4-mapped IPv6 form, or vice versa.
    let v4 = Policy::strict().with_net(vec!["127.0.0.1".to_string()]);
    assert!(!v4.allows_net("[::ffff:7f00:1]", 80));
}

#[test]
fn net_wildcard_allows_any_host() {
    let p = Policy::strict().with_net(vec!["*".to_string()]);
    assert!(p.allows_net("anything.example", 443));
}

#[test]
fn private_ip_ranges_are_detected() {
    use std::net::IpAddr;
    for s in [
        "127.0.0.1",
        "10.1.2.3",
        "192.168.0.1",
        "172.16.5.5",
        "169.254.169.254",
        "::1",
        // IPv4 special-purpose ranges.
        "0.0.0.0",
        "0.1.2.3",
        "100.64.0.1",
        "100.100.100.200", // Alibaba Cloud metadata
        "100.127.255.255",
        "192.0.0.192", // Oracle Cloud metadata
        "198.18.0.1",
        "198.19.255.255",
        "224.0.0.1",
        "239.255.255.250",
        "240.0.0.1",
        "255.255.255.255",
        // IPv6 native ranges.
        "::",
        "fc00::1",
        "fd00:ec2::254", // AWS IPv6 metadata
        "fe80::1",
        "fec0::1",
        "ff02::1",
        "64:ff9b:1::1",
        // IPv6 forms embedding a private IPv4 address.
        "::ffff:127.0.0.1",
        "::ffff:169.254.169.254",
        "::127.0.0.1",
        "::10.0.0.1",
        "64:ff9b::127.0.0.1",
        "64:ff9b::a9fe:a9fe",
        "2002:7f00:1::",
        "2002:a9fe:a9fe::1",
    ] {
        assert!(
            Policy::is_private_ip(s.parse::<IpAddr>().unwrap()),
            "{s} should be private"
        );
    }
    for s in [
        "8.8.8.8",
        "1.1.1.1",
        "93.184.216.34",
        "100.63.255.255",
        "100.128.0.1",
        "198.17.255.255",
        "198.20.0.1",
        "223.255.255.255",
        "2606:4700:4700::1111",
        "::ffff:8.8.8.8",
        "64:ff9b::808:808",
        "2002:808:808::1",
    ] {
        assert!(
            !Policy::is_private_ip(s.parse::<IpAddr>().unwrap()),
            "{s} should be public"
        );
    }
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
