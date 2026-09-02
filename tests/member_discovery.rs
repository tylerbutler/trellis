//! End-to-end tests for workspace member discovery.

mod common;

use common::*;
use predicates::prelude::*;
use std::path::Path;

fn write_package(root: &Path, path: &str, name: &str) {
    write(
        &root.join(path).join("gleam.toml"),
        &format!("name = \"{name}\"\nversion = \"0.1.0\"\n"),
    );
}

#[test]
fn recursive_member_glob_respects_repository_git_ignores() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);

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
        .stdout("chatrooms           hex\ncollab_docs_client  hex\n");
}

#[test]
fn literal_member_path_includes_an_ignored_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);

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
        .stdout("generated_package  hex\n");
}

#[test]
fn wildcard_with_only_ignored_packages_reports_no_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);

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
