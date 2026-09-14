//! End-to-end tests for `trellis release`.

mod common;

use common::{
    add_fragment, bare_origin, commit_of, copy_fixture_to, git, git_stdout, init_repo, series_repo,
    trellis_github, trellis_with_local_http as trellis, version_of, write,
};
use predicates::prelude::*;
use std::fs;

// ---- trellis release pr ----------------------------------------------------

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
// ---- release bootstrap -----------------------------------------------------

#[test]
fn bootstrap_uses_current_versions_with_no_fragments_required() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    // The fixture ships no `.changes/unreleased` directory at all — bootstrap
    // must not need one.
    assert!(!root.join(".changes").exists());

    trellis(root)
        .args(["release", "bootstrap"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tagged lat_core-v1.2.0"))
        .stdout(predicate::str::contains("tagged lat_mid-v0.5.0"))
        .stdout(predicate::str::contains("tagged lat_cli-v0.3.1"));

    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec!["lat_cli-v0.3.1", "lat_core-v1.2.0", "lat_mid-v0.5.0"]
    );
    // No version bump: bootstrap never runs `version plan`/`version apply`.
    assert_eq!(version_of(root, "lat_core"), "1.2.0");
    assert_eq!(version_of(root, "lat_mid"), "0.5.0");
    assert_eq!(version_of(root, "lat_cli"), "0.3.1");
}

#[test]
fn bootstrap_dry_run_reports_every_action_and_mutates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let remote = bare_origin(root);
    let api = common::mock_github(root);

    trellis_github(root, &api)
        .args([
            "release",
            "bootstrap",
            "--dry-run",
            "--push",
            "--github-release",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("would tag lat_core-v1.2.0"))
        .stdout(predicate::str::contains("would push lat_core-v1.2.0"))
        .stdout(predicate::str::contains(
            "would create GitHub release lat_core-v1.2.0",
        ))
        .stdout(predicate::str::contains("would tag lat_mid-v0.5.0"))
        .stdout(predicate::str::contains(
            "would create GitHub release lat_cli-v0.3.1",
        ));

    // Nothing was actually created, locally, on origin, or via the API.
    assert_eq!(git_stdout(root, &["tag", "--list"]), "");
    assert_eq!(git_stdout(remote.path(), &["tag", "--list"]), "");
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap_or_default();
    assert!(
        !log.contains("POST /repos/example/repo/releases\n"),
        "{log}"
    );
}

#[test]
fn bootstrap_creates_both_exact_and_series_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }",
    );

    trellis(root)
        .args(["release", "bootstrap", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would tag lat_cli-v0.3.1"))
        .stdout(predicate::str::contains("would tag lat_cli-v0.3\n"));

    trellis(root)
        .args(["release", "bootstrap"])
        .assert()
        .success();
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec![
            "lat_cli-v0.3",
            "lat_cli-v0.3.1",
            "lat_core-v1.2.0",
            "lat_mid-v0.5.0"
        ]
    );
}

#[test]
fn bootstrap_leaves_release_excluded_packages_untagged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);

    trellis(root)
        .args(["release", "bootstrap", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("package_a").not());

    trellis(root)
        .args(["release", "bootstrap"])
        .assert()
        .success();
    let tags = git_stdout(root, &["tag", "--list"]);
    assert!(!tags.contains("package_a"), "{tags}");
}

#[test]
fn bootstrap_fetches_a_remote_only_tag_instead_of_recreating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    // lat_core is already tagged and pushed by some earlier process; the
    // local clone bootstrapping now has never seen the tag.
    git(root, &["tag", "-a", "lat_core-v1.2.0", "-m", "existing"]);
    git(root, &["push", "origin", "lat_core-v1.2.0"]);
    let original = commit_of(root, "lat_core-v1.2.0");
    git(root, &["tag", "-d", "lat_core-v1.2.0"]);

    trellis(root)
        .args(["release", "bootstrap", "--dry-run", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would fetch lat_core-v1.2.0"));

    trellis(root)
        .args(["release", "bootstrap", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fetched lat_core-v1.2.0"));
    assert_eq!(commit_of(root, "lat_core-v1.2.0"), original);
}

#[test]
fn bootstrap_reports_existing_releases_and_reruns_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);

    trellis_github(root, &api)
        .args(["release", "bootstrap", "--push", "--github-release"])
        .assert()
        .success();
    let first_log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        first_log
            .matches("POST /repos/example/repo/releases\n")
            .count(),
        3
    );

    trellis_github(root, &api)
        .args([
            "release",
            "bootstrap",
            "--dry-run",
            "--push",
            "--github-release",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "GitHub release lat_core-v1.2.0 already exists; skipping",
        ))
        .stdout(predicate::str::contains("would").not());

    trellis_github(root, &api)
        .args(["release", "bootstrap", "--push", "--github-release"])
        .assert()
        .success();
    let second_log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        second_log
            .matches("POST /repos/example/repo/releases\n")
            .count(),
        3
    );
}

#[test]
fn bootstrap_preflights_conflicts_before_mutating_any_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    // lat_core is tagged and pushed, then the local tag is force-moved to a
    // later commit — an immutable tag disagreeing with origin.
    git(root, &["tag", "-a", "lat_core-v1.2.0", "-m", "first"]);
    git(root, &["push", "origin", "lat_core-v1.2.0"]);
    write(&root.join("later.txt"), "later\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "later"]);
    git(root, &["tag", "-f", "-a", "lat_core-v1.2.0", "-m", "moved"]);
    // lat_mid and lat_cli have no tag yet, and would otherwise be created.

    trellis(root)
        .args(["release", "bootstrap", "--dry-run", "--push"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("different objects"));

    trellis(root)
        .args(["release", "bootstrap", "--push"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("different objects"));

    // Neither the dry-run nor the failed real run tagged anything else —
    // the conflict on lat_core blocks the whole batch.
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(tags.lines().collect::<Vec<_>>(), vec!["lat_core-v1.2.0"]);
}
