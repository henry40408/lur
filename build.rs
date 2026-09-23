//! Bake the version string into the binary, first match wins:
//!   1. `GIT_VERSION` env var unless blank or `dev` (set by the release and
//!      Docker workflows, since the Docker context has no `.git`);
//!   2. `git describe --tags --always --dirty`;
//!   3. `dev`.
//!
//! Independent of the Cargo.toml `version` field.
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GIT_VERSION");
    // Re-resolve on branch switch. HEAD holds a symbolic ref, so a new commit on
    // the same branch does not trigger this; a worktree's `.git` is a file, so
    // there it never fires. Guarded because a watched missing path would rerun
    // the script on every build.
    if Path::new(".git/HEAD").exists() {
        println!("cargo:rerun-if-changed=.git/HEAD");
    }

    let version = env_version()
        .or_else(git_describe)
        .unwrap_or_else(|| "dev".to_string());

    println!("cargo:rustc-env=GIT_VERSION={version}");
}

/// `dev` is the Dockerfile's default `ARG GIT_VERSION`, so it falls through.
fn env_version() -> Option<String> {
    std::env::var("GIT_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "dev")
}

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}
