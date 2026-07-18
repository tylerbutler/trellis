//! End-to-end tests for member auto-discovery: fully configless workspaces
//! (no [tools.trellis] anywhere, root inferred from git), configured
//! workspaces without `members`, and the `@members` exclusion key.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn trellis(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trellis").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn git_init(root: &Path) {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");
}

/// Two packages with a path dependency between them, no config anywhere.
fn scaffold_two_packages(root: &Path) {
    write(
        &root.join("packages/core/gleam.toml"),
        "name = \"core\"\nversion = \"1.0.0\"\n",
    );
    write(
        &root.join("packages/cli/gleam.toml"),
        "name = \"cli\"\nversion = \"0.1.0\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
    );
}

// ---- fully configless (rung 1) ----------------------------------------

#[test]
fn configless_list_discovers_members_from_the_git_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);

    // Nothing is committed: discovery must see untracked packages too.
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("core\ncli\n");
}

#[test]
fn configless_works_from_inside_a_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);

    trellis(&root.join("packages/cli"))
        .arg("list")
        .assert()
        .success()
        .stdout("core\ncli\n");
}

#[test]
fn configless_single_package_repo_has_the_root_as_member() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    write(
        &root.join("gleam.toml"),
        "name = \"solo\"\nversion = \"2.0.0\"\n",
    );

    let output = trellis(root).args(["list", "--json"]).output().unwrap();
    assert!(output.status.success());
    let items: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "solo");
    assert_eq!(items[0]["path"], ".");
}

#[test]
fn configless_skips_gitignored_paths_and_build() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);
    write(&root.join(".gitignore"), "vendor/\n");
    write(
        &root.join("vendor/dep/gleam.toml"),
        "name = \"vendored\"\nversion = \"0.0.1\"\n",
    );
    // Gleam's build tree holds a manifest per downloaded dependency; it must
    // never become a member even if it is not gitignored.
    write(
        &root.join("build/packages/wibble/gleam.toml"),
        "name = \"wibble\"\nversion = \"0.9.0\"\n",
    );

    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("core\ncli\n");
}

#[test]
fn configless_doctor_announces_the_inference() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);

    trellis(root)
        .arg("doctor")
        .assert()
        .stdout(predicate::str::contains(
            "note: no [tools.trellis] configuration found; workspace root inferred from git, \
             2 member(s) auto-discovered",
        ));
}

#[test]
fn configless_errors_on_a_stray_trellis_table() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);
    write(
        &root.join("nested/gleam.toml"),
        "name = \"nested\"\nversion = \"0.1.0\"\n\n[tools.trellis]\n",
    );

    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains("workspace root was inferred as"))
        .stderr(predicate::str::contains("run trellis from `nested`"));
}

#[test]
fn no_config_outside_a_git_repo_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold_two_packages(root);

    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not inside a git repository"));
}

#[test]
fn unparseable_ancestor_manifest_blocks_the_configless_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);
    write(&root.join("gleam.toml"), "name = \"broken\nversion=\n");

    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains("could not be parsed"));
}

// ---- configured but members omitted (rung 2) ---------------------------

#[test]
fn table_without_members_auto_discovers_and_keeps_exclusions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);
    write(
        &root.join("examples/demo/gleam.toml"),
        "name = \"demo\"\nversion = \"0.0.1\"\n",
    );
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nexclude = { \"@release\" = [\"examples/*\"] }\n",
    );

    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("core\ncli\ndemo\n");
    trellis(root)
        .args(["list", "--releasable"])
        .assert()
        .success()
        .stdout("core\ncli\n");
}

#[test]
fn at_members_excludes_directories_from_membership() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);
    // A committed fixture package: gitignore cannot exclude it, @members can.
    write(
        &root.join("tests/fixtures/sample/gleam.toml"),
        "name = \"sample_fixture\"\nversion = \"0.0.1\"\n",
    );
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nexclude = { \"@members\" = [\"tests/**\"] }\n",
    );

    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("core\ncli\n");
}

#[test]
fn at_members_also_filters_explicit_member_globs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold_two_packages(root);
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\
         exclude = { \"@members\" = [\"packages/cli\"] }\n",
    );

    // No git needed: explicit members never touch discovery.
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("core\n");
}

#[test]
fn new_package_is_discovered_without_a_members_glob() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    scaffold_two_packages(root);

    trellis(root)
        .args(["new", "extra"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created packages/extra/"));
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("extra\n"));
}
