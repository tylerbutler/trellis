//! End-to-end tests for the native changelog/version engine: fragments,
//! check, plan, apply, and template rendering. No external changie binary —
//! trellis is the engine.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn trellis(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trellis").unwrap();
    cmd.current_dir(dir);
    // Deterministic dates in rendered changelogs: 2026-07-11.
    cmd.env("SOURCE_DATE_EPOCH", "1783728000");
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

// ---- changelog new ---------------------------------------------------------

#[test]
fn new_fragment_writes_toml_and_validates_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Added",
            "--body",
            "grow more vines",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ".changes/unreleased/lat_core-1.toml",
        ));
    let fragment = fs::read_to_string(root.join(".changes/unreleased/lat_core-1.toml")).unwrap();
    assert_eq!(
        fragment,
        "project = \"lat_core\"\nkind = \"Added\"\nbody = \"grow more vines\"\n"
    );

    // A second fragment gets the next free name.
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Fixed",
            "--body",
            "x",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core-2.toml"));

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "nope",
            "--kind",
            "Added",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown package"));
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "examples",
            "--kind",
            "Added",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("excluded from release"));
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Invented",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown kind `Invented`"))
        .stderr(predicate::str::contains("Added"));
    trellis(root)
        .args(["changelog", "new", "--kind", "Added", "--body", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--package is required"));
}

// ---- changelog check ---------------------------------------------------------

#[test]
fn changelog_check_maps_diff_to_missing_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
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
    // Change two releasable packages and examples; add a fragment for one.
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    write(&root.join("packages/lat_mid/src/new.gleam"), "// x\n");
    write(&root.join("examples/src/new.gleam"), "// x\n");
    add_fragment(root, "lat_core", "Added", "something");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "change"]);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "lat_mid lacks a fragment");
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["has-entries"], true);
    assert_eq!(payload["needs-entry"], true);
    let packages = payload["packages"].as_array().unwrap();
    // examples changed too but is not releasable, so only two rows.
    assert_eq!(packages.len(), 2);
    let core = packages.iter().find(|p| p["name"] == "lat_core").unwrap();
    assert_eq!(core["has-entry"], true);
    let mid = packages.iter().find(|p| p["name"] == "lat_mid").unwrap();
    assert_eq!(mid["has-entry"], false);
    assert!(payload["preview"].as_str().unwrap().contains("lat_mid"));

    // Adding the missing fragment turns the check green.
    add_fragment(root, "lat_mid", "Fixed", "more");
    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_mid: 1 fragment(s)"));
}

#[test]
fn invalid_fragments_fail_check_and_doctor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_typo", "Added", "x"); // unknown project
    add_fragment(root, "lat_core", "Invented", "x"); // unknown kind
    write(
        &root.join(".changes/unreleased/broken-1.toml"),
        "not toml at all {{{\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "`lat_typo` is not a workspace member",
        ))
        .stdout(predicate::str::contains("kind `Invented` is not one of"))
        .stdout(predicate::str::contains("broken-1.toml"));

    // plan/apply refuse loudly instead of dropping fragments.
    trellis(root)
        .args(["version", "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid changelog fragment(s)"));
}

// ---- version plan / apply ------------------------------------------------

#[test]
fn version_plan_bumps_by_the_largest_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "patch-level change");
    add_fragment(root, "lat_core", "Added", "minor-level change");
    add_fragment(root, "lat_mid", "Breaking", "major-level change");

    let output = trellis(root)
        .args(["version", "plan", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        plan,
        serde_json::json!([
            {"name": "lat_core", "current": "1.2.0", "next": "1.3.0", "fragments": 2},
            {"name": "lat_mid", "current": "0.5.0", "next": "1.0.0", "fragments": 1},
        ])
    );
}

#[test]
fn version_plan_is_empty_without_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture_to(tmp.path());
    trellis(tmp.path())
        .args(["version", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to bump"));
}

#[test]
fn version_apply_batches_renders_bumps_and_patches_lockfiles() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "grow more vines");
    add_fragment(root, "lat_core", "Fixed", "repair the trellis");

    let output = trellis(root)
        .args(["version", "apply", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["bumped"][0]["name"], "lat_core");
    assert_eq!(payload["bumped"][0]["next"], "1.3.0");
    assert_eq!(
        payload["lockfiles"],
        serde_json::json!(["packages/lat_mid/manifest.toml"])
    );

    // gleam.toml was bumped surgically: version changed, the rest untouched.
    let manifest = fs::read_to_string(root.join("packages/lat_core/gleam.toml")).unwrap();
    assert!(manifest.contains("version = \"1.3.0\""));
    assert!(manifest.contains("licences = [\"MIT\"]"));
    // The version section was batched…
    let section = fs::read_to_string(root.join(".changes/lat_core/v1.3.0.md")).unwrap();
    assert_eq!(
        section,
        "## v1.3.0 - 2026-07-11\n\n### Added\n\n- grow more vines\n\n### Fixed\n\n- repair the trellis\n"
    );
    // …the CHANGELOG was reassembled from header + sections…
    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert!(changelog.starts_with("# lat_core changelog\n"));
    assert!(changelog.contains("## v1.3.0 - 2026-07-11"));
    assert!(changelog.contains("- grow more vines"));
    // …fragments were consumed, and the dependent's lockfile patched.
    assert_eq!(
        fs::read_dir(root.join(".changes/unreleased"))
            .unwrap()
            .count(),
        0
    );
    let lock = fs::read_to_string(root.join("packages/lat_mid/manifest.toml")).unwrap();
    assert!(lock.contains("{ name = \"lat_core\", version = \"1.3.0\""));
    assert!(lock.contains("# This file was generated by Gleam"));

    // Everything is consistent afterwards…
    trellis(root).arg("doctor").assert().success();
    // …and re-running apply is a no-op.
    trellis(root)
        .args(["version", "apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to apply"));
}

#[test]
fn version_apply_preflights_all_manifests_before_consuming_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "core change");
    add_fragment(root, "lat_mid", "Fixed", "mid change");

    let core_manifest_path = root.join("packages/lat_core/gleam.toml");
    let core_manifest = fs::read_to_string(&core_manifest_path).unwrap();
    let mid_manifest_path = root.join("packages/lat_mid/gleam.toml");
    let mid_manifest = fs::read_to_string(&mid_manifest_path).unwrap();
    let mid_without_version = mid_manifest
        .lines()
        .filter(|line| !line.starts_with("version = "))
        .collect::<Vec<_>>()
        .join("\n");
    write(&mid_manifest_path, &format!("{mid_without_version}\n"));

    trellis(root)
        .args(["version", "apply"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no version field"));

    assert!(root.join(".changes/unreleased/lat_core-1.toml").is_file());
    assert!(root.join(".changes/unreleased/lat_mid-1.toml").is_file());
    assert_eq!(
        fs::read_to_string(core_manifest_path).unwrap(),
        core_manifest
    );
}

#[test]
fn version_apply_preflights_all_changelog_merges_before_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "core change");
    add_fragment(root, "lat_mid", "Fixed", "mid change");
    write(
        &root.join(".changes/lat_mid/not-semver.md"),
        "invalid stored section\n",
    );

    let core_manifest_path = root.join("packages/lat_core/gleam.toml");
    let core_manifest = fs::read_to_string(&core_manifest_path).unwrap();

    trellis(root)
        .args(["version", "apply"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not named v<semver>.md"));

    assert!(root.join(".changes/unreleased/lat_core-1.toml").is_file());
    assert!(root.join(".changes/unreleased/lat_mid-1.toml").is_file());
    assert_eq!(
        fs::read_to_string(core_manifest_path).unwrap(),
        core_manifest
    );
}

#[test]
fn version_apply_accumulates_sections_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    add_fragment(root, "lat_core", "Fixed", "first release");
    trellis(root).args(["version", "apply"]).assert().success();
    add_fragment(root, "lat_core", "Added", "second release");
    trellis(root).args(["version", "apply"]).assert().success();

    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    let newer = changelog.find("## v1.3.0").expect("second release section");
    let older = changelog.find("## v1.2.1").expect("first release section");
    assert!(newer < older, "newest section first:\n{changelog}");
}

#[test]
fn custom_minijinja_templates_shape_the_output() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let config = fs::read_to_string(root.join("gleam.toml")).unwrap();
    write(
        &root.join("gleam.toml"),
        &format!(
            concat!(
                "{config}\n",
                "[tools.trellis.changelog]\n",
                "header-format = \"# Changes to {{{{ name }}}}\"\n",
                "version-format = \"## {{{{ tag }}}} ({{{{ date }}}})\"\n",
                "kind-format = \"**{{{{ kind | upper }}}}**\"\n",
                "change-format = \"* {{{{ body }}}}\"\n",
                "kinds = [{{ label = \"Tweaked\", bump = \"patch\" }}]\n",
            ),
            config = config
        ),
    );
    add_fragment(root, "lat_core", "Tweaked", "polished the finish");

    trellis(root).args(["version", "apply"]).assert().success();

    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert!(changelog.starts_with("# Changes to lat_core\n"));
    assert!(changelog.contains("## lat_core-v1.2.1 (2026-07-11)"));
    assert!(changelog.contains("**TWEAKED**"));
    assert!(changelog.contains("* polished the finish"));
}
