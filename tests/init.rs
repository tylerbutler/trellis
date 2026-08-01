//! End-to-end tests for `trellis init` — bootstrapping a workspace.

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

fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Two packages in a git repo, with no workspace configuration at all — the
/// state someone adopting trellis actually starts from.
fn repo_with_packages(root: &Path) {
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );
    write(
        &root.join("packages/b/gleam.toml"),
        "name = \"b\"\nversion = \"1.0.0\"\n[dependencies]\na = { path = \"../a\" }\n",
    );
    git(root, &["init", "-q", "-b", "main"]);
}

fn root_config(root: &Path) -> String {
    fs::read_to_string(root.join("gleam.toml")).unwrap()
}

#[test]
fn init_writes_a_table_that_declares_nothing_derivable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_packages(root);

    trellis(root)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "members are auto-discovered; found 2:",
        ))
        .stdout(predicate::str::contains("packages/a"))
        .stdout(predicate::str::contains("packages/b"))
        // It finishes by running doctor, per the issue.
        .stdout(predicate::str::contains("checked:"))
        .stdout(predicate::str::contains("ok: 2 package(s)"));

    let config = root_config(root);
    assert!(config.contains("[tools.trellis]"), "{config}");
    // The point of the command: `members` is derivable, so it is not declared.
    // The comment block mentions it, so this looks for a real key, not the word.
    let declared: Vec<&str> = config
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.is_empty() && !line.starts_with('['))
        .collect();
    assert!(declared.is_empty(), "expected no keys, got {declared:?}");
    // The comments are the part a reference page is bad at.
    assert!(config.contains("auto-discovered"), "{config}");
    assert!(
        config.contains("https://trellis.tylerbutler.com/docs/configuration/"),
        "{config}"
    );
}

/// `-q` is a global flag, so it reaches `init` too — including the `doctor`
/// run it finishes with — even though `init` writes the file regardless of
/// whether anything printed.
#[test]
fn quiet_suppresses_init_and_the_doctor_run_it_finishes_with() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_packages(root);

    trellis(root)
        .args(["init", "--quiet"])
        .assert()
        .success()
        .stdout("");

    let config = root_config(root);
    assert!(config.contains("[tools.trellis]"), "{config}");
}

#[test]
fn the_written_table_is_what_root_discovery_finds() {
    // Before init the root is inferred from git; after it, it is declared. Both
    // must agree, and commands must work from inside a package either way.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_packages(root);
    trellis(root).arg("init").assert().success();

    trellis(&root.join("packages/b"))
        .arg("list")
        .assert()
        .success()
        .stdout("a  hex\nb  hex\n");
    // The configless note is gone: the workspace is now declared.
    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("no [tools.trellis] configuration found").not());
}

#[test]
fn init_refuses_a_repository_that_is_already_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_packages(root);
    trellis(root).arg("init").assert().success();
    let before = root_config(root);

    trellis(root)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already a trellis workspace"));
    // Refusing means refusing: the existing config is untouched.
    assert_eq!(root_config(root), before);
}

#[test]
fn init_refuses_when_a_member_carries_the_table() {
    // A member-level [tools.trellis] hijacks root discovery, so writing a
    // second one at the root would produce two roots and no clear winner.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_packages(root);
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n[tools.trellis]\n",
    );

    trellis(root)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "a member manifest cannot carry one",
        ));
    assert!(!root.join("gleam.toml").exists());
}

#[test]
fn init_preserves_a_root_that_is_itself_a_package() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_packages(root);
    let existing = "name = \"app\"\nversion = \"2.1.0\"\n\n\
                    [dependencies]\ngleam_stdlib = \">= 0.44.0\"\n";
    write(&root.join("gleam.toml"), existing);

    trellis(root)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("added [tools.trellis] to"));

    let config = root_config(root);
    assert!(config.starts_with(existing), "{config}");
    assert!(config.contains("[tools.trellis]"), "{config}");
    // A package root is a member too, so it joins the other two.
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("app"));
}

#[test]
fn init_keeps_an_existing_tools_table_for_another_tool() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    repo_with_packages(root);
    write(
        &root.join("gleam.toml"),
        "name = \"app\"\nversion = \"1.0.0\"\n\n[tools.other]\nsetting = true\n",
    );

    trellis(root).arg("init").assert().success();

    let config = root_config(root);
    assert!(config.contains("[tools.other]"), "{config}");
    assert!(config.contains("setting = true"), "{config}");
    assert!(config.contains("[tools.trellis]"), "{config}");
    trellis(root).arg("list").assert().success();
}

#[test]
fn init_outside_a_git_repository_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs a git repository"));
    assert!(!root.join("gleam.toml").exists());
}

#[test]
fn init_in_an_empty_repository_still_writes_a_usable_root() {
    // Nothing to discover yet, which doctor reports; init's job is to leave a
    // root behind that works once packages arrive.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);

    trellis(root)
        .arg("init")
        .assert()
        .failure()
        .stdout(predicate::str::contains("no packages discovered yet"));
    assert!(root_config(root).contains("[tools.trellis]"));

    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("a  hex\n");
}
