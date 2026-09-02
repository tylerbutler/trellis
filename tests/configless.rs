//! End-to-end tests for member auto-discovery: fully configless workspaces
//! (no [tools.trellis] anywhere, root inferred from git), configured
//! workspaces without `members`, the `@members` exclusion key, and how
//! member globs honor git ignore rules.

mod common;

use common::*;
use predicates::prelude::*;
use std::path::Path;

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
    git(root, &["init", "-q"]);
    scaffold_two_packages(root);

    // Nothing is committed: discovery must see untracked packages too.
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("core  hex\ncli   hex\n");
}

#[test]
fn configless_works_from_inside_a_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
    scaffold_two_packages(root);

    trellis(&root.join("packages/cli"))
        .arg("list")
        .assert()
        .success()
        .stdout("core  hex\ncli   hex\n");
}

#[test]
fn configless_single_package_repo_has_the_root_as_member() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
    write(
        &root.join("gleam.toml"),
        "name = \"solo\"\nversion = \"2.0.0\"\n",
    );

    let document = json_output(root, &["list", "--json"], true);
    let items = document["packages"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "solo");
    assert_eq!(items[0]["path"], ".");
}

#[test]
fn configless_skips_gitignored_paths_and_build() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
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
        .stdout("core  hex\ncli   hex\n");
}

#[test]
fn configless_doctor_announces_the_inference() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
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
    git(root, &["init", "-q"]);
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
    git(root, &["init", "-q"]);
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
    git(root, &["init", "-q"]);
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
        .stdout("core  hex\ncli   hex\ndemo  workspace\n");
    trellis(root)
        .args(["list", "--releasable"])
        .assert()
        .success()
        .stdout("core  hex\ncli   hex\n");
}

#[test]
fn at_members_excludes_directories_from_membership() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"]);
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
        .stdout("core  hex\ncli   hex\n");
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
        .stdout("core  hex\n");
}

// ---- member globs and git ignores ---------------------------------------

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
