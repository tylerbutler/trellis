//! End-to-end tests for `trellis pin`, using real git repos on disk as
//! remotes — `ls-remote`, `fetch`, and ancestry all work against a local
//! path, so no network is touched.

mod common;

use common::*;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;

/// A repository serving as the git remote: one commit on `main`, with an
/// annotated tag `v1` on it. Returns the tempdir and the commit SHA.
fn remote_repo() -> (tempfile::TempDir, String) {
    let remote = tempfile::tempdir().unwrap();
    write(&remote.path().join("file.txt"), "one\n");
    git(remote.path(), &["init", "-q", "-b", "main"]);
    git(remote.path(), &["add", "."]);
    git(remote.path(), &["commit", "-q", "-m", "one"]);
    git(remote.path(), &["tag", "-a", "v1", "-m", "v1"]);
    let sha = git_stdout(remote.path(), &["rev-parse", "v1^{commit}"]);
    (remote, sha)
}

/// A workspace with one member depending on the remote twice: `dep_a` tracks
/// the annotated tag `v1`, `dep_b` tracks the branch `main` and carries an
/// unrelated trailing comment that must survive pinning.
fn workspace(url: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    write(
        &root.path().join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    let manifest = root.path().join("packages/app/gleam.toml");
    write(
        &manifest,
        &format!(
            "name = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\n\
             gleam_stdlib = \">= 0.34.0 and < 2.0.0\"\n\
             dep_a = {{ git = \"{url}\", ref = \"v1\" }}\n\n\
             [dev-dependencies]\n\
             dep_b = {{ git = \"{url}\", ref = \"main\" }} # keep\n"
        ),
    );
    init_repo(root.path());
    (root, manifest)
}

#[test]
fn pin_rewrites_refs_records_intent_and_is_idempotent() {
    let (remote, sha) = remote_repo();
    let url = remote.path().display().to_string();
    let (root, manifest) = workspace(&url);

    trellis(root.path()).arg("pin").assert().success();
    let text = fs::read_to_string(&manifest).unwrap();
    // The annotated tag pins to the peeled commit, not the tag object.
    assert!(
        text.contains(&format!(
            "dep_a = {{ git = \"{url}\", ref = \"{sha}\" }} # trellis:pin v1"
        )),
        "unexpected gleam.toml:\n{text}"
    );
    // The branch dep pins too, and its own comment survives in front.
    assert!(text.contains(&format!(
        "dep_b = {{ git = \"{url}\", ref = \"{sha}\" }} # keep # trellis:pin main"
    )));
    assert!(text.contains("gleam_stdlib = \">= 0.34.0 and < 2.0.0\""));

    trellis(root.path()).arg("pin").assert().success();
    assert_eq!(
        fs::read_to_string(&manifest).unwrap(),
        text,
        "second pin must be a no-op"
    );
}

#[test]
fn check_passes_when_the_tracked_ref_merely_advances() {
    let (remote, _) = remote_repo();
    let url = remote.path().display().to_string();
    let (root, _) = workspace(&url);
    trellis(root.path()).arg("pin").assert().success();

    // The pinned commit stays an ancestor: history grew, nothing was rewritten.
    write(&remote.path().join("file.txt"), "two\n");
    git(remote.path(), &["commit", "-q", "-am", "two"]);
    git(remote.path(), &["tag", "-fa", "v1", "-m", "v1"]);

    trellis(root.path())
        .args(["pin", "--check"])
        .assert()
        .success();
}

#[test]
fn check_and_doctor_flag_a_force_moved_ref() {
    let (remote, _) = remote_repo();
    let url = remote.path().display().to_string();
    let (root, _) = workspace(&url);
    trellis(root.path()).arg("pin").assert().success();

    // Rewrite the remote's only commit: the pinned SHA is no longer reachable
    // from either `v1` or `main`.
    git(
        remote.path(),
        &[
            "commit",
            "--amend",
            "--allow-empty",
            "-q",
            "-m",
            "rewritten",
        ],
    );
    git(remote.path(), &["tag", "-fa", "v1", "-m", "v1"]);

    trellis(root.path())
        .args(["pin", "--check"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "not reachable from its tracked ref",
        ));

    // doctor reports the same drift as an advisory warning, not a failure.
    trellis(root.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "not reachable from its tracked ref",
        ));
}

#[test]
fn update_follows_the_moved_ref_and_unpin_restores_the_original() {
    let (remote, _) = remote_repo();
    let url = remote.path().display().to_string();
    let (root, manifest) = workspace(&url);
    let original = fs::read_to_string(&manifest).unwrap();
    trellis(root.path()).arg("pin").assert().success();

    write(&remote.path().join("file.txt"), "two\n");
    git(remote.path(), &["commit", "-q", "-am", "two"]);
    git(remote.path(), &["tag", "-fa", "v1", "-m", "v1"]);
    let new_sha = git_stdout(remote.path(), &["rev-parse", "v1^{commit}"]);

    trellis(root.path())
        .args(["pin", "--update"])
        .assert()
        .success();
    let text = fs::read_to_string(&manifest).unwrap();
    assert!(
        text.contains(&format!(
            "dep_a = {{ git = \"{url}\", ref = \"{new_sha}\" }} # trellis:pin v1"
        )),
        "update did not follow v1:\n{text}"
    );
    assert!(text.contains(&format!("ref = \"{new_sha}\" }} # keep # trellis:pin main")));

    trellis(root.path())
        .args(["pin", "--unpin"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&manifest).unwrap(), original);
}

#[test]
fn pin_patches_the_locked_manifest_commit() {
    let (remote, sha) = remote_repo();
    let url = remote.path().display().to_string();
    let (root, _) = workspace(&url);
    let lockfile = root.path().join("packages/app/manifest.toml");
    write(
        &lockfile,
        &format!(
            "# This file was generated by Gleam\n\
             packages = [\n  \
               {{ name = \"dep_a\", version = \"1.0.0\", source = \"git\", repo = \"{url}\", commit = \"0000000000000000000000000000000000000000\" }},\n\
             ]\n\n\
             [requirements]\n\
             dep_a = {{ git = \"{url}\", ref = \"v1\" }}\n"
        ),
    );

    trellis(root.path()).arg("pin").assert().success();
    let text = fs::read_to_string(&lockfile).unwrap();
    assert!(
        text.contains(&format!("commit = \"{sha}\"")),
        "locked commit not patched:\n{text}"
    );
    assert!(text.contains("# This file was generated by Gleam"));
}
