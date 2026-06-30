use assert_cmd::Command;

#[test]
fn docs_prints_the_guide() {
    Command::cargo_bin("lur")
        .unwrap()
        .arg("docs")
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .stdout(predicates::str::contains("lur.json"));
}

#[test]
fn docs_honors_no_color() {
    let out = Command::cargo_bin("lur")
        .unwrap()
        .arg("docs")
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        !out.contains(&0x1b),
        "no ANSI escape with NO_COLOR: {out:?}"
    );
}
