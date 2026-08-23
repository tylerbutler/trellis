//! End-to-end tests for `[tools.trellis.adapter]`: a workspace whose members
//! are not Gleam packages, configured from a `trellis.toml`.
//!
//! Two shapes are covered, matching the two motivating repositories — an APM
//! monorepo (`apm.yml`, YAML at the member root) and a Claude Code plugin
//! marketplace (`.claude-plugin/plugin.json`, JSON inside a hidden directory).

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

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
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

const TRELLIS_TOML: &str = r#"[tools.trellis]
members = ["packages/*"]

[tools.trellis.adapter]
manifest = "apm.yml"

[tools.trellis.publish]
package_tags = ["exact"]
"#;

/// An `apm.yml` with the comments and layout a hand-maintained manifest has,
/// so a surgical bump has something to preserve.
fn apm_manifest(name: &str, version: &str) -> String {
    format!(
        "# The {name} package.\nname: {name}\nversion: \"{version}\"  # bumped by trellis\n\
         dependencies:\n  apm:\n    - microsoft/apm-sample-package#v1.0.0\n"
    )
}

/// A two-package APM workspace configured from `trellis.toml`.
fn scaffold(root: &Path) {
    git_init(root);
    write(&root.join("trellis.toml"), TRELLIS_TOML);
    write(
        &root.join("packages/alpha/apm.yml"),
        &apm_manifest("alpha", "1.2.0"),
    );
    write(
        &root.join("packages/beta/apm.yml"),
        &apm_manifest("beta", "0.3.1"),
    );
}

// ---- discovery and introspection -------------------------------------------

#[test]
fn members_are_discovered_by_the_adapter_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);

    // No gleam.toml anywhere: the root lives in trellis.toml, and identity
    // comes from each member's apm.yml. `git_only` is the adapter default.
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("alpha  git_only\nbeta   git_only\n");

    // A directory without the adapter manifest is not a member.
    write(&root.join("packages/notes/README.md"), "not a package\n");
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("alpha  git_only\nbeta   git_only\n");
}

#[test]
fn members_auto_discover_when_no_members_glob_is_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);
    write(
        &root.join("trellis.toml"),
        "[tools.trellis]\n\n[tools.trellis.adapter]\nmanifest = \"apm.yml\"\n",
    );

    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("alpha  git_only\nbeta   git_only\n");
}

#[test]
fn commands_work_from_inside_a_member() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);

    trellis(&root.join("packages/alpha"))
        .args(["info", "alpha"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1.2.0"));
}

// ---- the release pipeline ---------------------------------------------------

#[test]
fn version_apply_bumps_the_manifest_surgically() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);
    let alpha = root.join("packages/alpha/apm.yml");
    let before = read(&alpha);

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "alpha",
            "--kind",
            "Added",
            "--body",
            "Support monorepo diffs",
        ])
        .assert()
        .success();
    trellis(root).args(["version", "apply"]).assert().success();

    // Exactly one thing changed: comments, key order, and the quote style all
    // survive, and the untouched sibling is byte-identical.
    assert_eq!(read(&alpha), before.replace("1.2.0", "1.3.0"));
    assert_eq!(
        read(&root.join("packages/beta/apm.yml")),
        apm_manifest("beta", "0.3.1")
    );
    assert!(read(&root.join("packages/alpha/CHANGELOG.md")).contains("Support monorepo diffs"));

    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha-v1.3.0"));
}

#[test]
fn json_manifests_in_a_hidden_directory_work_the_same() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git_init(root);
    write(
        &root.join("trellis.toml"),
        "[tools.trellis]\nmembers = [\"plugins/*\"]\n\n[tools.trellis.adapter]\n\
         manifest = \".claude-plugin/plugin.json\"\n",
    );
    let plugin = root.join("plugins/code-reviewer/.claude-plugin/plugin.json");
    let before = "{\n  \"name\": \"code-reviewer\",\n  \"version\": \"1.2.0\",\n  \
                  \"description\": \"Reviews code\"\n}\n";
    write(&plugin, before);

    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("code-reviewer  git_only\n");

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "code-reviewer",
            "--kind",
            "Fixed",
            "--body",
            "Handle empty diffs",
        ])
        .assert()
        .success();
    trellis(root).args(["version", "apply"]).assert().success();

    assert_eq!(read(&plugin), before.replace("1.2.0", "1.2.1"));
}

// ---- what an adapter workspace refuses --------------------------------------

#[test]
fn gleam_only_commands_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);

    for args in [
        vec!["publish", "alpha"],
        vec!["lockfile", "refresh"],
        vec!["run", "test"],
        vec!["new", "gamma"],
    ] {
        trellis(root)
            .args(&args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("apm.yml"));
    }
}

#[test]
fn a_declared_task_still_runs_under_an_adapter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);
    write(
        &root.join("trellis.toml"),
        &format!("{TRELLIS_TOML}\n[tools.trellis.tasks.check]\ncommand = \"echo checked\"\n"),
    );

    trellis(root)
        .args(["run", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("checked"));
}

#[test]
fn a_hex_lifecycle_is_rejected_by_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);
    write(
        &root.join("trellis.toml"),
        &format!("{TRELLIS_TOML}\n[tools.trellis.publish.lifecycle]\ndefault = \"hex\"\n"),
    );

    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no Hex packages to publish"));
}

// ---- doctor -----------------------------------------------------------------

#[test]
fn doctor_is_clean_and_names_the_adapter_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);
    // A releasable member wants a CHANGELOG.md; seed both so the run is clean.
    trellis(root).args(["doctor", "--fix"]).assert().success();

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("parseable apm.yml"))
        .stdout(predicate::str::contains("declares a semver version"))
        // The Gleam-only checks are not claimed.
        .stdout(predicate::str::contains("manifest.toml locked versions").not())
        .stdout(predicate::str::contains("gleam on PATH").not());
}

#[test]
fn doctor_reports_adapter_specific_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);

    write(
        &root.join("packages/alpha/apm.yml"),
        "name: renamed\nversion: not-a-version\n",
    );
    trellis(root)
        .args(["doctor", "--format", "json"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("is not valid semver"))
        .stdout(predicate::str::contains(
            "the manifest name and the directory disagree",
        ));

    // A manifest missing the field the adapter points at fails to load at all.
    write(&root.join("packages/alpha/apm.yml"), "name: alpha\n");
    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no `version` (the `adapter.version` field path)",
        ));
}

#[test]
fn a_member_level_trellis_toml_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);
    write(
        &root.join("packages/alpha/trellis.toml"),
        "[tools.trellis]\nmembers = [\"nope/*\"]\n",
    );

    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "only the workspace root may have one",
        ));
}

#[test]
fn init_refuses_a_repository_already_configured_by_trellis_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);

    // Writing a second config home is exactly the state `list` refuses below.
    trellis(root)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already a trellis workspace"))
        .stderr(predicate::str::contains("trellis.toml"));
    assert!(!root.join("gleam.toml").exists());
}

#[test]
fn two_config_homes_carrying_the_table_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    scaffold(root);
    write(
        &root.join("gleam.toml"),
        "name = \"root\"\nversion = \"1.0.0\"\n\n[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );

    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains("keep exactly one"));
}
