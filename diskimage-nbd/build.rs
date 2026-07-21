use std::{env, process::Command, str};

/// Run a git command in the crate directory, returning trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| str::from_utf8(&out.stdout).ok().map(|s| s.trim().to_string()))
        .flatten()
        .filter(|s| !s.is_empty())
}

/// Embed the current commit hash as the `GIT_COMMIT` compile-time env var.
///
/// The release build script (`scripts/build-release.sh`) sets `GIT_COMMIT`
/// itself after verifying a clean tree; honor that when present. Otherwise fall
/// back to asking git directly and append `-dirty` when tracked files have
/// uncommitted changes, so ad-hoc dev builds still carry a truthful marker.
fn main() {
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }

    let commit = env::var("GIT_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let short = git(&["rev-parse", "--short", "HEAD"])?;
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            Some(if dirty { format!("{short}-dirty") } else { short })
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_COMMIT={commit}");
}
