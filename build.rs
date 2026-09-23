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
    watch_git_state();

    let version = env_version()
        .or_else(git_describe)
        .unwrap_or_else(|| "dev".to_string());

    println!("cargo:rustc-env=GIT_VERSION={version}");
}

/// Rerun when `git describe` could change: checkout, commit, new tag, or a
/// dirty tree. Paths come from `git rev-parse --git-path` so worktrees (where
/// `.git` is a file) resolve too. Only existing paths are watched, since a
/// missing one would rerun the script on every build.
fn watch_git_state() {
    let mut paths = vec![
        "HEAD".to_string(),
        "index".to_string(),
        "packed-refs".to_string(),
        "refs/tags".to_string(),
    ];
    // The branch HEAD points at, e.g. `refs/heads/main`; absent when detached.
    paths.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for path in paths {
        if let Some(resolved) = git(&["rev-parse", "--git-path", &path])
            && Path::new(&resolved).exists()
        {
            println!("cargo:rerun-if-changed={resolved}");
        }
    }
    // Unstaged edits flip `--dirty` without touching the index.
    println!("cargo:rerun-if-changed=src");
}

/// `dev` is the Dockerfile's default `ARG GIT_VERSION`, so it falls through.
fn env_version() -> Option<String> {
    std::env::var("GIT_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "dev")
}

fn git_describe() -> Option<String> {
    git(&["describe", "--tags", "--always", "--dirty"])
}

/// Trimmed stdout of a successful, non-empty `git` run.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}
