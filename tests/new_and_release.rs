//! End-to-end tests for `trellis release pr` (release-PR management via a
//! mock GitHub API) and doctor's .tool-versions advisory.

mod common;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn trellis(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trellis").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn copy_fixture_to(root: &Path) {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let from = fixture("basic");
    let mut files = Vec::new();
    walk(&from, &mut files);
    for file in files {
        let dest = root.join(file.strip_prefix(&from).unwrap());
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&file, &dest).unwrap();
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn make_executable(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

// ---- trellis release pr ----------------------------------------------------

fn add_fragment(root: &Path, project: &str, kind: &str, body: &str) {
    let dir = root.join(".changes/unreleased");
    fs::create_dir_all(&dir).unwrap();
    for n in 1u32.. {
        let path = dir.join(format!("{project}-{n}.toml"));
        if !path.exists() {
            write(
                &path,
                &format!("project = \"{project}\"\nkind = \"{kind}\"\nbody = \"{body}\"\n"),
            );
            return;
        }
    }
}

/// A trellis invocation aimed at the mock GitHub API: base URL and repo
/// overridden, a token in the environment, and proxy variables stripped so
/// requests reach the localhost mock.
fn trellis_github(dir: &Path, api: &str) -> Command {
    let mut cmd = trellis(dir);
    cmd.env("TRELLIS_GITHUB_API_URL", api)
        .env("TRELLIS_GITHUB_REPO", "example/repo")
        .env("GITHUB_TOKEN", "test-token");
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

#[test]
fn release_pr_creates_then_updates_the_pull_request() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let api = common::mock_github(root);

    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );

    add_fragment(root, "lat_core", "Added", "pending change");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fragment"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(&root.join("feature-only.txt"), "not part of the release\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "feature-only"]);

    trellis_github(root, &api)
        .args(["release", "pr", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "created release PR: https://github.com/example/repo/pull/7",
        ));

    // The PR was created against the right base with the bump in the body,
    // including the changelog section the native engine just rendered.
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert!(
        log.contains("GET /repos/example/repo/pulls?head=example:release/pending&state=open"),
        "{log}"
    );
    assert!(log.contains("POST /repos/example/repo/pulls\n"), "{log}");
    assert!(log.contains(r#""base": "main""#), "{log}");
    assert!(log.contains(r#""head": "release/pending""#), "{log}");
    assert!(
        log.contains(r#""title": "release: lat_core v1.3.0"#),
        "{log}"
    );
    assert!(log.contains("| lat_core | 1.2.0 | 1.3.0 | 1 |"));
    assert!(
        log.contains("- pending change"),
        "body includes the changelog section:\n{log}"
    );

    // The release branch is on the remote with the release commit…
    let remote_branch = std::process::Command::new("git")
        .args([
            "--git-dir",
            remote.path().to_str().unwrap(),
            "rev-parse",
            "refs/heads/release/pending",
        ])
        .output()
        .unwrap();
    assert!(remote_branch.status.success());
    let feature_file = std::process::Command::new("git")
        .args([
            "--git-dir",
            remote.path().to_str().unwrap(),
            "ls-tree",
            "--name-only",
            "refs/heads/release/pending",
            "feature-only.txt",
        ])
        .output()
        .unwrap();
    assert!(
        feature_file.stdout.is_empty(),
        "release branch must not include commits from the caller branch"
    );
    // …while the working tree is back on feature, clean, and unbumped.
    let head = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "feature");
    let manifest = fs::read_to_string(root.join("packages/lat_core/gleam.toml")).unwrap();
    assert!(manifest.contains("version = \"1.2.0\""));

    // Second run with a new fragment updates the existing PR instead. The
    // bump is computed against main's version (1.2.0), so Fixed -> 1.2.1.
    git(root, &["checkout", "-q", "main"]);
    add_fragment(root, "lat_core", "Fixed", "more");
    write(&root.join(".fake/pr-list"), "[{\"number\": 42}]");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "more fragments"]);

    trellis_github(root, &api)
        .args(["release", "pr", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated release PR #42"));
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert!(log.contains("PATCH /repos/example/repo/pulls/42"), "{log}");
    assert!(log.contains(r#""title": "release: lat_core v"#), "{log}");
}

#[test]
fn release_pr_requires_a_clean_tree_and_pending_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);

    // No fragments: a clean no-op. Both paths stop before any GitHub call,
    // so no API mock (or token) is needed.
    trellis(root)
        .args(["release", "pr"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to release"));

    // Dirty tree: refuse before touching anything.
    write(&root.join("packages/lat_core/src/wip.gleam"), "// wip\n");
    trellis(root)
        .args(["release", "pr"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("working tree is not clean"));
}

#[test]
fn release_pr_noop_preserves_an_existing_release_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["checkout", "-q", "-b", "release/pending"]);
    write(&root.join("existing-release.txt"), "keep this commit\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "existing release work"]);
    git(root, &["checkout", "-q", "main"]);

    trellis(root)
        .args(["release", "pr", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to release"));

    let preserved = std::process::Command::new("git")
        .args(["show", "release/pending:existing-release.txt"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        preserved.status.success(),
        "a no-op must not reset the existing release branch"
    );
}

#[test]
fn release_pr_failure_preserves_an_existing_release_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    add_fragment(root, "lat_core", "Added", "pending change");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fragment"]);
    git(root, &["checkout", "-q", "-b", "release/pending"]);
    write(&root.join("existing-release.txt"), "keep this commit\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "existing release work"]);
    let original_release = std::process::Command::new("git")
        .args(["rev-parse", "release/pending"])
        .current_dir(root)
        .output()
        .unwrap()
        .stdout;
    git(root, &["checkout", "-q", "main"]);

    trellis(root)
        .args(["release", "pr", "--base", "main"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to push branch"));

    let preserved_release = std::process::Command::new("git")
        .args(["rev-parse", "release/pending"])
        .current_dir(root)
        .output()
        .unwrap()
        .stdout;
    assert_eq!(
        preserved_release, original_release,
        "a failed release must not move the existing local release branch"
    );
}

// ---- doctor .tool-versions advisory ----------------------------------------

#[test]
fn doctor_warns_on_tool_versions_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    write(&root.join(".tool-versions"), "erlang 27.0\ngleam 1.5.0\n");
    let gleam = root.join("fake-gleam.sh");
    write(&gleam, "#!/bin/sh\necho 'gleam 1.4.1'\n");
    make_executable(&gleam);

    // Mismatch is a warning, not an error: doctor still succeeds.
    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "gleam on PATH is 1.4.1 but .tool-versions pins 1.5.0",
        ));

    // Matching versions: no warning.
    write(&root.join(".tool-versions"), "gleam 1.4.1\n");
    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("gleam on PATH is").not());
}
