//! End-to-end tests for workspace member discovery.

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

fn write_package(root: &Path, path: &str, name: &str) {
    write(
        &root.join(path).join("gleam.toml"),
        &format!("name = \"{name}\"\nversion = \"0.1.0\"\n"),
    );
}

fn init_git(root: &Path) {
    let status = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn recursive_member_glob_respects_repository_git_ignores() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_git(root);

    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"examples/**\"]\n",
    );
    write(&root.join(".gitignore"), "build/\n");
    write(&root.join(".git/info/exclude"), "scratch/\n");
    write(
        &root.join("examples/collab_docs/.gitignore"),
        "generated/\n",
    );

    write_package(root, "examples/chatrooms", "chatrooms");
    write_package(root, "examples/collab_docs/client", "collab_docs_client");
    write_package(root, "examples/scratch", "scratch");
    write_package(root, "examples/collab_docs/generated", "generated");

    // These duplicate vendored packages reproduce issue #21 when build/
    // directories are traversed.
    write_package(root, "examples/chatrooms/build/packages/vendor", "vendor");
    write_package(root, "examples/collab_docs/build/packages/vendor", "vendor");

    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("chatrooms\ncollab_docs_client\n");
}

#[test]
fn literal_member_path_includes_an_ignored_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_git(root);

    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"generated/package\"]\n",
    );
    write(&root.join(".gitignore"), "generated/\n");
    write_package(root, "generated/package", "generated_package");

    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("generated_package\n");
}

#[test]
fn wildcard_with_only_ignored_packages_reports_no_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_git(root);

    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"generated/**\"]\n",
    );
    write(&root.join(".gitignore"), "generated/\n");
    write_package(root, "generated/package", "generated_package");

    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "member glob `generated/**` matches no packages",
        ));
}
