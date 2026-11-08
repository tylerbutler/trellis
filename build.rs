//! Embeds git describe output so dev builds can report the commit they were
//! built from. Missing git metadata, such as in a crates.io tarball, leaves
//! the package version unchanged.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    if let Some(describe) = git(&["describe", "--tags", "--always", "--dirty"]) {
        println!("cargo:rustc-env=VERGEN_GIT_DESCRIBE={describe}");
    }
}
