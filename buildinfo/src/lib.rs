//! Shared build-time helpers for embedding the git commit in a binary.
//!
//! Crates that ship a binary use this two ways:
//!   - their `build.rs` calls [`emit_git_commit`] (as a `[build-dependencies]`
//!     entry) to set the `GIT_COMMIT` compile-time env var, and
//!   - the binary uses [`long_version!`] (as a `[dependencies]` entry) for
//!     clap's `long_version`, e.g. `"0.1.1 (a1b2c3d)"`.

use std::{env, process::Command, str};

/// Run a git command in the current directory, returning trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| str::from_utf8(&out.stdout).ok().map(|s| s.trim().to_string()))
        .flatten()
        .filter(|s| !s.is_empty())
}

/// Emit `cargo:rustc-env=GIT_COMMIT=<hash>` for the calling crate. Call this from
/// a crate's `build.rs`; the crate's code can then read it via [`long_version!`]
/// (or `env!("GIT_COMMIT")` directly).
///
/// The release build script (`scripts/build-release.sh`) sets `GIT_COMMIT`
/// itself after verifying a clean tree; that is honored when present. Otherwise
/// this asks git directly and appends `-dirty` when tracked files have
/// uncommitted changes, so ad-hoc dev builds still carry a truthful marker.
pub fn emit_git_commit() {
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

/// `"<CARGO_PKG_VERSION> (<GIT_COMMIT>)"` as a `&'static str`, for clap's
/// `long_version`.
///
/// It expands in the caller, so it reads the caller crate's own version and the
/// `GIT_COMMIT` its `build.rs` set via [`emit_git_commit`] -- both are per-crate
/// compile-time env vars, which is why this is a macro rather than a constant
/// here.
#[macro_export]
macro_rules! long_version {
    () => {
        concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_COMMIT"), ")")
    };
}
