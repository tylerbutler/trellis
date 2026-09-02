//! The exit-code contract. CI workflows branch on these, so every code gets a
//! case here and a change to the mapping breaks a test.
//!
//! | code | meaning                                                        |
//! |------|----------------------------------------------------------------|
//! | 0    | success, no findings                                           |
//! | 1    | ran correctly, found problems                                  |
//! | 2    | usage error (clap's default)                                   |
//! | 3    | internal/environment error — bad config, no git repo, no tool  |

mod common;

use common::*;
use predicates::prelude::*;
use std::path::Path;

/// A workspace whose only problem is an unfixable one, so `doctor` reports a
/// finding rather than failing to load.
fn workspace_with_findings(root: &Path) {
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n[dependencies]\nout = { path = \"../../../elsewhere\" }\n",
    );
}

// ---- 0: success -------------------------------------------------------

#[test]
fn success_exits_zero() {
    trellis(&fixture("basic")).arg("list").assert().code(0);
}

// ---- 1: the command ran and found problems ----------------------------

#[test]
fn doctor_findings_exit_one() {
    let dir = tempfile::tempdir().unwrap();
    workspace_with_findings(dir.path());

    trellis(dir.path())
        .arg("doctor")
        .assert()
        .code(1)
        .stdout(predicate::str::contains("points outside the workspace"));
}

#[test]
fn a_failed_task_exits_one_and_does_not_propagate_the_child_code() {
    // The child exits 3 — trellis reports its own outcome, not the child's,
    // so 3 must not leak out and masquerade as an internal error.
    trellis(&fixture("basic"))
        .args(["exec", "lat_core", "--", "sh", "-c", "exit 3"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("FAILED"));
}

// ---- 2: usage --------------------------------------------------------

#[test]
fn usage_error_exits_two() {
    trellis(&fixture("basic"))
        .arg("--no-such-flag")
        .assert()
        .code(2);
}

#[test]
fn json_together_with_format_exits_two() {
    // `--json` is a deprecated alias for `--format json`. Passing both is
    // ambiguous rather than redundant, so clap rejects it as usage.
    trellis(&fixture("basic"))
        .args([
            "changelog",
            "check",
            "--base",
            "main",
            "--json",
            "--format",
            "github",
        ])
        .assert()
        .code(2);
}

// ---- 3: trellis itself could not run ---------------------------------

#[test]
fn unparseable_root_manifest_exits_three() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(&root.join("gleam.toml"), "name = \"broken\nversion=\n");
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root).arg("list").assert().code(3);
}

#[test]
fn outside_a_git_repository_exits_three() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(dir.path())
        .arg("list")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("not inside a git repository"));
}

#[test]
fn a_members_glob_matching_nothing_exits_three() {
    // Bad configuration, not a finding: the workspace model never loaded.
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"pkgs/*\"]\n",
    );

    trellis(dir.path())
        .arg("list")
        .assert()
        .code(3)
        .stderr(predicate::str::contains("matches no packages"));
}
