//! End-to-end tests for the changelog/version layer, run against temp copies
//! of the fixture workspace with a fake `changie` binary (via
//! TRELLIS_CHANGIE_BIN) so no real changie install is needed.

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

/// A fake changie that supports the subset trellis drives:
/// - `next auto --project K`  → prints the contents of `.fake/next-K`
/// - `batch auto --project K` → rewrites the package's gleam.toml version to
///   that value and deletes K's unreleased fragments (what replacements +
///   batching do)
/// - `merge`                  → logs
fn install_fake_changie(root: &Path) -> PathBuf {
    let script = root.join("fake-changie.sh");
    write(
        &script,
        concat!(
            "#!/bin/sh\n",
            "set -eu\n",
            "cmd=\"$1\"; shift\n",
            "case \"$cmd\" in\n",
            "  next)\n",
            "    key=\"$3\"\n",
            "    cat \".fake/next-$key\"\n",
            "    ;;\n",
            "  batch)\n",
            "    key=\"$3\"\n",
            "    next=$(cat \".fake/next-$key\")\n",
            "    dir=$(cat \".fake/dir-$key\")\n",
            "    sed -i \"s/^version = .*/version = \\\"$next\\\"/\" \"$dir/gleam.toml\"\n",
            "    rm -f .changes/unreleased/\"$key\"-*.yaml\n",
            "    echo \"batch $key\" >> .fake/log\n",
            "    ;;\n",
            "  merge)\n",
            "    echo merge >> .fake/log\n",
            "    ;;\n",
            "  *)\n",
            "    echo \"unexpected: $cmd\" >&2\n",
            "    exit 1\n",
            "    ;;\n",
            "esac\n",
        ),
    );
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(root.join(".fake")).unwrap();
    script
}

fn add_fragment(root: &Path, project: &str, index: u32) {
    write(
        &root.join(format!(".changes/unreleased/{project}-{index}.yaml")),
        &format!("project: {project}\nkind: Added\nbody: something new\n"),
    );
}

fn plan_release(root: &Path, project: &str, dir: &str, next: &str) {
    write(&root.join(format!(".fake/next-{project}")), next);
    write(&root.join(format!(".fake/dir-{project}")), dir);
}

// ---- changelog sync ---------------------------------------------------

#[test]
fn sync_creates_starter_config_with_generated_projects() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture_to(tmp.path());

    trellis(tmp.path())
        .args(["changelog", "sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created .changie.yaml"));

    let config = fs::read_to_string(tmp.path().join(".changie.yaml")).unwrap();
    // One project block per releasable member; examples (ignore-release) gets none.
    assert!(config.contains("key: lat_core"));
    assert!(config.contains("key: lat_mid"));
    assert!(config.contains("key: lat_cli"));
    assert!(!config.contains("key: examples"));
    assert!(config.contains("changelog: packages/lat_core/CHANGELOG.md"));
    assert!(config.contains("path: packages/lat_core/gleam.toml"));
    assert!(config.contains("{{.VersionNoPrefix}}"));
    // Separator derived from tag-format {name}-v{version}.
    assert!(config.contains("projectsVersionSeparator: \"-v\""));

    // Now in sync.
    trellis(tmp.path())
        .args(["changelog", "sync", "--check"])
        .assert()
        .success();
}

#[test]
fn sync_updates_only_the_projects_section() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture_to(tmp.path());
    write(
        &tmp.path().join(".changie.yaml"),
        "# hand-written header comment\nchangesDir: .changes\nunreleasedDir: unreleased\nprojects:\n  - label: stale\n    key: stale\nkinds:\n  - label: Added\n",
    );

    trellis(tmp.path())
        .args(["changelog", "sync", "--check"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("out of date"));

    trellis(tmp.path())
        .args(["changelog", "sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated .changie.yaml"));

    let config = fs::read_to_string(tmp.path().join(".changie.yaml")).unwrap();
    assert!(config.contains("# hand-written header comment"));
    assert!(config.contains("kinds:"));
    assert!(config.contains("key: lat_core"));
    assert!(!config.contains("key: stale"));
}

#[test]
fn doctor_flags_changie_drift_and_fix_repairs_it() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture_to(tmp.path());
    write(
        &tmp.path().join(".changie.yaml"),
        "changesDir: .changes\nprojects:\n  - label: stale\n    key: stale\n",
    );

    trellis(tmp.path())
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            ".changie.yaml projects are out of date",
        ));

    trellis(tmp.path())
        .args(["doctor", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fixed: regenerated .changie.yaml"));

    trellis(tmp.path())
        .args(["changelog", "sync", "--check"])
        .assert()
        .success();
}

// ---- changelog check ----------------------------------------------------

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
    add_fragment(root, "lat_core", 1);
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
    add_fragment(root, "lat_mid", 1);
    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_mid: 1 fragment(s)"));
}

#[test]
fn changelog_check_rejects_fragments_for_unknown_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    add_fragment(root, "lat_typo", 1);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "fragment"]);

    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("lat_typo"))
        .stdout(predicate::str::contains("not a workspace member"));
}

// ---- version plan / apply ------------------------------------------------

#[test]
fn version_plan_reports_pending_bumps() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let changie = install_fake_changie(root);
    add_fragment(root, "lat_core", 1);
    add_fragment(root, "lat_core", 2);
    plan_release(root, "lat_core", "packages/lat_core", "1.3.0");

    let output = trellis(root)
        .env("TRELLIS_CHANGIE_BIN", &changie)
        .args(["version", "plan", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        plan,
        serde_json::json!([{
            "name": "lat_core",
            "current": "1.2.0",
            "next": "1.3.0",
            "fragments": 2,
        }])
    );
}

#[test]
fn version_plan_is_empty_without_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture_to(tmp.path());
    let changie = install_fake_changie(tmp.path());
    trellis(tmp.path())
        .env("TRELLIS_CHANGIE_BIN", &changie)
        .args(["version", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to bump"));
}

#[test]
fn version_apply_bumps_and_patches_lockfiles() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let changie = install_fake_changie(root);
    add_fragment(root, "lat_core", 1);
    plan_release(root, "lat_core", "packages/lat_core", "1.3.0");

    let output = trellis(root)
        .env("TRELLIS_CHANGIE_BIN", &changie)
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

    // gleam.toml was bumped by the (fake) changie replacement…
    let core = fs::read_to_string(root.join("packages/lat_core/gleam.toml")).unwrap();
    assert!(core.contains("version = \"1.3.0\""));
    // …and the dependent's lockfile was patched surgically: version updated,
    // comments and the Hex dep untouched.
    let lock = fs::read_to_string(root.join("packages/lat_mid/manifest.toml")).unwrap();
    assert!(lock.contains("# This file was generated by Gleam"));
    assert!(lock.contains("{ name = \"lat_core\", version = \"1.3.0\""));
    assert!(lock.contains("{ name = \"gleam_stdlib\", version = \"0.40.0\""));
    // batch ran for the project and merge ran once.
    let log = fs::read_to_string(root.join(".fake/log")).unwrap();
    assert_eq!(log, "batch lat_core\nmerge\n");

    // The bump is now consistent: doctor's lockfile check passes.
    trellis(root).arg("doctor").assert().success();

    // Re-running apply is a no-op (fragments were consumed by batch).
    trellis(root)
        .env("TRELLIS_CHANGIE_BIN", &changie)
        .args(["version", "apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to apply"));
}

#[test]
fn version_apply_fails_loudly_when_replacement_does_not_land() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let changie = install_fake_changie(root);
    add_fragment(root, "lat_core", 1);
    // Point the fake's replacement at the wrong directory, simulating a
    // stale/missing replacements block in .changie.yaml.
    plan_release(root, "lat_core", "packages/lat_mid", "1.3.0");

    trellis(root)
        .env("TRELLIS_CHANGIE_BIN", &changie)
        .args(["version", "apply"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("did not update `lat_core`"));
}

#[test]
fn version_plan_rejects_fragment_for_unreleasable_project() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let changie = install_fake_changie(root);
    add_fragment(root, "examples", 1);

    trellis(root)
        .env("TRELLIS_CHANGIE_BIN", &changie)
        .args(["version", "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("excluded from release"));
}
