//! End-to-end tests for `trellis list`.

mod common;

use common::{copy_fixture_to, fixture, trellis, write};
use predicates::prelude::*;

// ---- list ------------------------------------------------------------

#[test]
fn list_prints_members_in_topological_order() {
    trellis(&fixture("basic"))
        .arg("list")
        .assert()
        .success()
        .stdout("lat_core   hex\nlat_mid    hex\nlat_cli    hex\npackage_a  workspace\n");
}

#[test]
fn list_treats_git_deps_with_subdirectory_paths_as_external() {
    // Gleam 1.18+ git deps may carry a `path` key selecting a subdirectory
    // of the remote repo. They must not join the workspace path-dep graph
    // (regression: they were misread as workspace path deps, making every
    // command fail with "workspace is invalid").
    trellis(&fixture("git-path-deps"))
        .arg("list")
        .assert()
        .success()
        .stdout("gp_core  hex\ngp_app   hex\n");
}

#[test]
fn list_works_from_inside_a_package() {
    trellis(&fixture("basic").join("packages/lat_mid"))
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("lat_core"));
}

#[test]
fn list_releasable_excludes_release_excluded_members() {
    trellis(&fixture("basic"))
        .args(["list", "--releasable"])
        .assert()
        .success()
        .stdout("lat_core  hex\nlat_mid   hex\nlat_cli   hex\n");
}

#[test]
fn list_json_includes_graph_facts() {
    let output = trellis(&fixture("basic"))
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "trellis.list/1");
    let items = document["packages"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    let mid = items.iter().find(|i| i["name"] == "lat_mid").unwrap();
    assert_eq!(mid["version"], "0.5.0");
    assert_eq!(mid["path"], "packages/lat_mid");
    assert_eq!(mid["lifecycle"], "hex");
    assert_eq!(mid["releasable"], true);
    assert_eq!(mid["dependencies"], serde_json::json!(["lat_core"]));
    assert_eq!(mid["dependents"], serde_json::json!(["lat_cli"]));
    let package_a = items.iter().find(|i| i["name"] == "package_a").unwrap();
    assert_eq!(package_a["lifecycle"], "workspace");
    assert_eq!(package_a["releasable"], false);
}
// ---- --since ---------------------------------------------------------

#[test]
fn since_selects_changed_packages_and_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Copy the basic fixture into a real git repo.
    copy_fixture_to(root);

    let git = |args: &[&str]| {
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
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["checkout", "-q", "-b", "feature"]);
    write(&root.join("packages/lat_mid/src/new.gleam"), "// change\n");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "touch mid"]);

    trellis(root)
        .args(["list", "--since", "main"])
        .assert()
        .success()
        .stdout("lat_mid  hex\n");

    trellis(root)
        .args(["list", "--since", "main", "--with-dependents"])
        .assert()
        .success()
        .stdout("lat_mid    hex\nlat_cli    hex\npackage_a  workspace\n");

    // Uncommitted changes count too.
    write(&root.join("packages/lat_core/src/wip.gleam"), "// wip\n");
    trellis(root)
        .args(["list", "--since", "main"])
        .assert()
        .success()
        .stdout("lat_core  hex\nlat_mid   hex\n");
}
