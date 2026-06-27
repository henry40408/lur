//! Bake the user-facing version string into the binary at compile time.
//!
//! Resolution is layered, most-authoritative first:
//!   1. the `LUR_VERSION` env var, if non-empty — the release workflow injects
//!      the published tag here, and the rolling `main` image a `<ref>-<sha>`
//!      marker (see `.github/workflows/docker.yml`);
//!   2. otherwise `git describe` against the working checkout — gives local
//!      builds a `<tag>-<n>-g<sha>[-dirty]` (or bare short-sha) string;
//!   3. otherwise the literal `dev`.
//!
//! Step 2 covers local development; the Docker build context excludes `.git`,
//! so inside the image only steps 1 and 3 apply — which is exactly why the
//! workflow injects `LUR_VERSION`. The result is independent of the Cargo.toml
//! `version` field.
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LUR_VERSION");
    // Re-resolve when HEAD moves (branch switch / new commit) so a local build's
    // version doesn't go stale. Guarded so a missing `.git` (the Docker context)
    // doesn't make cargo rerun the script on every build.
    if Path::new(".git/HEAD").exists() {
        println!("cargo:rerun-if-changed=.git/HEAD");
    }

    let version = env_version()
        .or_else(git_describe)
        .unwrap_or_else(|| "dev".to_string());

    println!("cargo:rustc-env=LUR_VERSION={version}");
}

/// The injected `LUR_VERSION`, trimmed; `None` if unset or blank.
fn env_version() -> Option<String> {
    std::env::var("LUR_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `git describe --tags --always --dirty`, or `None` if git is absent, this is
/// not a checkout, or the command otherwise fails.
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
