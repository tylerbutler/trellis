//! End-to-end tests for `trellis new` (scaffolding), `trellis release pr`
//! (release-PR management via a fake gh), and doctor's .tool-versions
//! advisory.

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

// ---- trellis new ----------------------------------------------------------

#[test]
fn new_scaffolds_a_member_with_copied_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    trellis(root)
        .args(["new", "lat_extra"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created packages/lat_extra/"))
        .stdout(predicate::str::contains(
            "metadata copied from packages/lat_core",
        ));

    let manifest = fs::read_to_string(root.join("packages/lat_extra/gleam.toml")).unwrap();
    assert!(manifest.contains("name = \"lat_extra\""));
    assert!(manifest.contains("version = \"0.1.0\""));
    // Copied from the lat_core sibling.
    assert!(manifest.contains("gleam = \">= 1.0.0\""));
    assert!(manifest.contains("licences = [\"MIT\"]"));
    assert!(manifest.contains("repository = { type = \"github\""));
    assert!(manifest.contains("gleam_stdlib = \">= 0.34.0 and < 2.0.0\""));
    assert!(manifest.contains("gleeunit = \">= 1.2.0 and < 2.0.0\""));

    assert!(
        root.join("packages/lat_extra/src/lat_extra.gleam")
            .is_file()
    );
    let test_stub =
        fs::read_to_string(root.join("packages/lat_extra/test/lat_extra_test.gleam")).unwrap();
    assert!(test_stub.contains("import lat_extra"));
    assert!(root.join("packages/lat_extra/CHANGELOG.md").is_file());

    // The new member is immediately part of the workspace…
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_extra"));
    // …and healthy.
    trellis(root).arg("doctor").assert().success();
}

#[test]
fn new_member_is_immediately_releasable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    trellis(root).args(["new", "lat_extra"]).assert().success();

    // No registration step: the new member can take fragments and release
    // right away, because everything is derived from the files on disk.
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_extra",
            "--kind",
            "Added",
            "--body",
            "born",
        ])
        .assert()
        .success();
    trellis(root)
        .args(["version", "apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bumped lat_extra: 0.1.0 -> 0.2.0"));
    let changelog = fs::read_to_string(root.join("packages/lat_extra/CHANGELOG.md")).unwrap();
    assert!(changelog.contains("- born"));
}

#[test]
fn new_rejects_bad_names_and_unmatched_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    trellis(root)
        .args(["new", "lat_core"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    trellis(root)
        .args(["new", "Bad-Name"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid gleam package name"));
    trellis(root)
        .args(["new", "fine", "--path", "elsewhere"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not match any members glob"));
    trellis(root)
        .args(["new", "fine", "--template", "service"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown template"));
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

/// Fake gh: logs invocations; `pr list` replies with `.fake/pr-list` (or an
/// empty array), `pr create` prints a PR URL.
fn install_fake_gh(root: &Path) -> PathBuf {
    let script = root.join("fake-gh.sh");
    write(
        &script,
        concat!(
            "#!/bin/sh\n",
            "printf 'gh %s\\n' \"$*\" >> .fake/gh-log\n",
            "case \"$1 $2\" in\n",
            "  'pr list') cat .fake/pr-list 2>/dev/null || echo '[]' ;;\n",
            "  'pr create') echo 'https://github.com/example/repo/pull/7' ;;\n",
            "esac\n",
        ),
    );
    make_executable(&script);
    fs::create_dir_all(root.join(".fake")).unwrap();
    script
}

#[test]
fn release_pr_creates_then_updates_the_pull_request() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gh = install_fake_gh(root);

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

    trellis(root)
        .env("TRELLIS_GH_BIN", &gh)
        .args(["release", "pr", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "created release PR: https://github.com/example/repo/pull/7",
        ));

    // The PR was created against the right base with the bump in the body,
    // including the changelog section the native engine just rendered.
    let log = fs::read_to_string(root.join(".fake/gh-log")).unwrap();
    assert!(log.contains("pr create --base main --head release/pending"));
    assert!(log.contains("--title Release: lat_core v1.3.0"));
    assert!(log.contains("| lat_core | 1.2.0 | 1.3.0 | 1 |"));
    assert!(
        log.contains("- pending change"),
        "body includes the changelog section:\n{log}"
    );

    // The release branch is on the remote with the release commit…
    let branches = std::process::Command::new("git")
        .args(["branch", "-a", "--contains", "release/pending"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).contains("remotes/origin/release/pending"));
    // …while the working tree is back on main, clean, and unbumped.
    let head = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "main");
    let manifest = fs::read_to_string(root.join("packages/lat_core/gleam.toml")).unwrap();
    assert!(manifest.contains("version = \"1.2.0\""));

    // Second run with a new fragment updates the existing PR instead. The
    // bump is computed against main's version (1.2.0), so Fixed -> 1.2.1.
    add_fragment(root, "lat_core", "Fixed", "more");
    write(&root.join(".fake/pr-list"), "[{\"number\": 42}]");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "more fragments"]);

    trellis(root)
        .env("TRELLIS_GH_BIN", &gh)
        .args(["release", "pr", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated release PR #42"));
    let log = fs::read_to_string(root.join(".fake/gh-log")).unwrap();
    assert!(log.contains("pr edit 42 --title Release: lat_core v"));
}

#[test]
fn release_pr_requires_a_clean_tree_and_pending_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gh = install_fake_gh(root);
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);

    // No fragments: a clean no-op.
    trellis(root)
        .env("TRELLIS_GH_BIN", &gh)
        .args(["release", "pr"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to release"));

    // Dirty tree: refuse before touching anything.
    write(&root.join("packages/lat_core/src/wip.gleam"), "// wip\n");
    trellis(root)
        .env("TRELLIS_GH_BIN", &gh)
        .args(["release", "pr"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("working tree is not clean"));
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
