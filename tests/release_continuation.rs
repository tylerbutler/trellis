mod common;

use common::*;
use predicates::prelude::*;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

struct ReleaseFixture {
    root: tempfile::TempDir,
    _origin: tempfile::TempDir,
    api: String,
}

impl ReleaseFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        copy_fixture_to(root.path());
        write(&root.path().join(".gitignore"), ".fake/\n");
        let api = mock_github(root.path());
        add_fragment(root.path(), "lat_core", "Added", "first feature");
        init_repo(root.path());
        let origin = bare_origin(root.path());
        Self {
            root,
            _origin: origin,
            api,
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn prepare(&self, args: &[&str]) -> Value {
        let output = trellis_github(self.path(), &self.api)
            .args(["release", "pr", "--json"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn pending(&self) {
        write(&self.path().join(".fake/pr-list"), &json!([{
            "number": 7, "state": "open",
            "html_url": "https://github.com/example/repo/pull/7",
            "head": {"ref": "release/pending", "sha": commit_of(self.path(), "origin/release/pending").trim(),
                     "repo": {"full_name": "example/repo"}},
            "base": {"ref": "main", "repo": {"full_name": "example/repo"}}
        }]).to_string());
    }

    fn merge(&self) {
        git(
            self.path(),
            &["merge", "--ff-only", "origin/release/pending"],
        );
        write(&self.path().join(".fake/pr-list"), "[]");
    }

    fn commit(&self) {
        git(self.path(), &["add", "."]);
        git(self.path(), &["commit", "-qm", "new inputs"]);
    }
}

#[test]
fn pending_proposed_targets_survive_updates_and_field_overrides() {
    let f = ReleaseFixture::new();
    let first = f.prepare(&[
        "--pre",
        "rc",
        "--set",
        "lat_core=2.0.0",
        "--set",
        "lat_mid=1.0.0",
    ]);
    assert_eq!(first["outcome"], "prepared");
    assert_eq!(first["pr_action"], "created");
    f.pending();
    let unchanged = f.prepare(&["--resume", "--skip-unchanged"]);
    assert_eq!(unchanged["outcome"], "unchanged");
    add_fragment(f.path(), "lat_cli", "Fixed", "joining change");
    f.commit();
    let updated = f.prepare(&["--resume", "--set", "lat_mid=1.0.0"]);
    assert_eq!(updated["pr_action"], "updated");
    let tree = git_stdout(
        f.path(),
        &[
            "show",
            "origin/release/pending:packages/lat_core/gleam.toml",
        ],
    );
    assert!(tree.contains("2.0.0-rc.0"), "{tree}");
    assert!(
        git_stdout(
            f.path(),
            &["show", "origin/release/pending:packages/lat_mid/gleam.toml"]
        )
        .contains("1.0.0-rc.0")
    );
}

#[test]
fn derived_pending_versions_become_fixed_targets_without_a_state_file() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    assert!(
        git_stdout(
            f.path(),
            &[
                "ls-tree",
                "origin/release/pending",
                "--",
                ".changes/release.toml"
            ]
        )
        .is_empty()
    );
    f.pending();
    add_fragment(f.path(), "lat_core", "Breaking", "breaking change");
    f.commit();
    assert_eq!(f.prepare(&["--resume"])["bumped"][0]["next"], "1.3.0-rc.0");
}

#[test]
fn workspace_bump_cannot_replace_pending_candidate_series() {
    let f = ReleaseFixture::new();
    f.prepare(&[
        "--pre",
        "rc",
        "--set",
        "lat_core=2.0.0",
        "--set",
        "lat_mid=1.0.0",
    ]);
    f.pending();
    let before = commit_of(f.path(), "origin/release/pending");
    for args in [
        vec!["--resume", "--bump", "patch"],
        vec!["--no-resume", "--pre", "rc", "--set", "lat_core=3.0.0"],
    ] {
        trellis_github(f.path(), &f.api)
            .args(["release", "pr"])
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("candidate series"));
        assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
        assert_eq!(git_stdout(f.path(), &["status", "--porcelain"]), "");
    }
}

#[test]
fn changed_source_versions_reject_resumption_without_push() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    let manifest = f.path().join("packages/lat_core/gleam.toml");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace("1.2.0", "1.2.1");
    write(&manifest, &text);
    f.commit();
    let before = commit_of(f.path(), "origin/release/pending");
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected 1.2.0"));
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
    assert_eq!(git_stdout(f.path(), &["status", "--porcelain"]), "");
}

#[test]
fn preparation_removes_old_release_metadata() {
    let f = ReleaseFixture::new();
    write(&f.path().join(".changes/release.toml"), "old metadata\n");
    f.commit();
    f.prepare(&["--pre", "rc"]);
    assert!(
        git_stdout(
            f.path(),
            &[
                "ls-tree",
                "origin/release/pending",
                "--",
                ".changes/release.toml"
            ]
        )
        .is_empty()
    );
}

#[test]
fn pending_candidate_can_be_promoted_within_its_series() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    assert_eq!(
        f.prepare(&["--resume", "--pre", "none"])["bumped"][0]["next"],
        "1.3.0"
    );
}

#[test]
#[cfg(unix)]
fn metadata_cleanup_rejects_symlinks_before_preparation() {
    let f = ReleaseFixture::new();
    write(&f.path().join("important.toml"), "keep this\n");
    std::os::unix::fs::symlink("../important.toml", f.path().join(".changes/release.toml"))
        .unwrap();
    f.commit();
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--pre", "rc"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("contains a symlink"));
    assert_eq!(version_of(f.path(), "lat_core"), "1.2.0");
    assert_eq!(
        fs::read_to_string(f.path().join("important.toml")).unwrap(),
        "keep this\n"
    );
    assert_eq!(git_stdout(f.path(), &["status", "--porcelain"]), "");
}

#[test]
fn automatic_continuation_skips_docs_and_uses_independent_bases() {
    let f = ReleaseFixture::new();
    f.prepare(&[
        "--pre",
        "beta",
        "--set",
        "lat_core=2.0.0",
        "--set",
        "lat_mid=1.0.0",
    ]);
    f.merge();
    let before = commit_of(f.path(), "origin/release/pending");
    write(&f.path().join("README.md"), "unrelated docs");
    f.commit();
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
    add_fragment(f.path(), "lat_core", "Fixed", "second feature");
    f.commit();
    let result = f.prepare(&["--auto-pre"]);
    assert_eq!(result["outcome"], "prepared");
    assert_eq!(result["bumped"][0]["next"], "2.0.0-beta.1");
    assert!(
        result["bumped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "lat_mid" && p["next"] == "1.0.0-beta.1")
    );
}

#[test]
fn pending_promotion_and_stable_targets_resume_without_implicit_promotion() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.merge();
    add_fragment(f.path(), "lat_mid", "Added", "another change");
    f.commit();
    f.prepare(&["--pre", "none"]);
    f.pending();
    let result = f.prepare(&["--resume"]);
    assert_eq!(result["bumped"][0]["next"], "1.3.0");
    assert_eq!(version_of(f.path(), "lat_core"), "1.3.0-rc.0");
}

#[test]
fn stale_pending_intent_fails_without_remote_mutation() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    git(f.path(), &["merge", "--ff-only", "origin/release/pending"]);
    let before = commit_of(f.path(), "origin/release/pending");
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume", "--auto-pre"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("stale"));
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
    assert_eq!(git_stdout(f.path(), &["status", "--porcelain"]), "");
}

#[test]
fn machine_no_changes_and_github_output_are_clean() {
    let root = tempfile::tempdir().unwrap();
    copy_fixture_to(root.path());
    init_repo(root.path());
    let result = trellis_with_local_http(root.path())
        .args(["release", "pr", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let document: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(document["schema"], "trellis.release_pr/1");
    assert_eq!(document["outcome"], "no_changes");
    assert_eq!(document["bumped"], json!([]));
    insta::assert_json_snapshot!("release_no_changes", document);
    trellis_with_local_http(root.path())
        .args(["release", "pr", "--github-output"])
        .assert()
        .success()
        .stdout(predicate::str::contains("outcome=no_changes\n"))
        .stdout(predicate::str::contains("bumped=[]\n"));
}

#[test]
fn prepared_and_unchanged_results_have_stable_shapes() {
    let f = ReleaseFixture::new();
    let prepared = f.prepare(&["--pre", "rc"]);
    insta::assert_json_snapshot!("release_prepared", prepared);
    f.pending();
    let before = commit_of(f.path(), "origin/release/pending");
    let unchanged = f.prepare(&["--resume", "--skip-unchanged"]);
    insta::assert_json_snapshot!("release_unchanged", unchanged);
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
    trellis_github(f.path(), &f.api)
        .args([
            "release",
            "pr",
            "--resume",
            "--skip-unchanged",
            "--github-output",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("outcome=unchanged\n"))
        .stdout(predicate::str::contains("pr_number=7\n"))
        .stdout(predicate::str::contains(
            "pr_url=https://github.com/example/repo/pull/7\n",
        ));
}

#[test]
fn explicit_promotion_clears_inherited_candidate_pins_but_keeps_joining_targets() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc", "--set", "lat_core=2.0.0"]);
    f.merge();
    write(
        &f.path().join("packages/joiner/gleam.toml"),
        "name = \"joiner\"\nversion = \"0.1.0\"\n",
    );
    add_fragment(f.path(), "joiner", "Added", "joining release");
    f.commit();
    f.prepare(&["--pre", "rc", "--set", "joiner=1.0.0"]);
    f.pending();
    let promotion = f.prepare(&["--resume", "--pre", "none"]);
    assert!(
        promotion["bumped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "joiner" && p["next"] == "1.0.0")
    );
    f.pending();
    add_fragment(f.path(), "lat_core", "Fixed", "late fix");
    f.commit();
    let resumed = f.prepare(&["--resume"]);
    assert!(
        resumed["bumped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "lat_core" && p["next"] == "2.0.0")
    );
}

#[test]
fn pending_stable_targets_do_not_revert_to_fragment_bumps() {
    let f = ReleaseFixture::new();
    f.prepare(&["--set", "lat_core=2.0.0"]);
    f.pending();
    add_fragment(f.path(), "lat_core", "Fixed", "another fix");
    f.commit();
    assert_eq!(f.prepare(&["--resume"])["bumped"][0]["next"], "2.0.0");
}

#[test]
fn package_bump_cannot_replace_pending_candidate_series() {
    let f = ReleaseFixture::new();
    f.prepare(&[
        "--pre",
        "rc",
        "--set",
        "lat_core=2.0.0",
        "--set",
        "lat_mid=1.0.0",
    ]);
    f.pending();
    let before = commit_of(f.path(), "origin/release/pending");
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume", "--bump", "lat_core=patch"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("candidate series"));
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
}

#[test]
fn explicit_requests_and_force_bypass_automatic_unchanged_skips() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.merge();
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
    assert_eq!(
        f.prepare(&["--pre", "rc"])["bumped"][0]["next"],
        "1.3.0-rc.1"
    );
    f.merge();
    assert_eq!(
        f.prepare(&["--auto-pre", "--force"])["bumped"][0]["next"],
        "1.3.0-rc.2"
    );
    f.merge();
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
    assert_eq!(
        f.prepare(&["--auto-pre", "--skip-unchanged", "--pre", "none"])["bumped"][0]["next"],
        "1.3.0"
    );
}

#[test]
fn configuration_opt_ins_and_cli_disable_flags_use_selected_base() {
    let f = ReleaseFixture::new();
    let config = fs::read_to_string(f.path().join("gleam.toml")).unwrap();
    write(
        &f.path().join("gleam.toml"),
        &format!(
            "{config}\n[tools.trellis.release]\nresume = true\nauto_pre = true\nskip_unchanged = true\n"
        ),
    );
    f.commit();
    f.prepare(&["--pre", "rc"]);
    f.merge();
    assert_eq!(f.prepare(&[])["outcome"], "unchanged");
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--no-auto-pre"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--pre none"));
    assert_eq!(
        f.prepare(&["--pre", "rc", "--no-skip-unchanged"])["outcome"],
        "prepared"
    );
    f.pending();
    assert_eq!(f.prepare(&[])["outcome"], "unchanged");
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--no-resume", "--no-auto-pre"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--pre none"));
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume", "--no-resume"])
        .assert()
        .code(2);
}

#[test]
fn conflicting_labels_and_unplanned_source_changes_are_actionable() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.merge();
    let manifest = f.path().join("packages/lat_mid/gleam.toml");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace("0.5.1-rc.0", "0.5.1-beta.0");
    write(&manifest, &text);
    f.commit();
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--auto-pre"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflicting candidate labels"))
        .stderr(predicate::str::contains("lat_mid=0.5.1-beta.0"));
}

#[test]
fn source_bundles_are_release_owned_and_examples_do_not_trigger_candidates() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\nexclude = { \"@release\" = [\"examples/*\"] }\n\
         [tools.trellis.publish.lifecycle]\ndefault = \"git_only\"\npackages = { \"packages/beryl_protocol\" = \"workspace\" }\n\
         [tools.trellis.publish.bundle]\nberyl = [\"beryl_protocol\"]\n",
    );
    for (name, version) in [
        ("beryl", "0.5.0"),
        ("beryl_mist", "0.4.0"),
        ("beryl_protocol", "0.1.0"),
    ] {
        write(
            &root.join(format!("packages/{name}/gleam.toml")),
            &format!("name = \"{name}\"\nversion = \"{version}\"\n"),
        );
        write(
            &root.join(format!("packages/{name}/src/{name}.gleam")),
            "pub fn main() { Nil }\n",
        );
    }
    write(
        &root.join("packages/beryl/gleam.toml"),
        "name = \"beryl\"\nversion = \"0.5.0\"\n[dependencies]\nberyl_protocol = { path = \"../beryl_protocol\" }\n",
    );
    write(
        &root.join("examples/example/gleam.toml"),
        "name = \"example\"\nversion = \"0.1.0\"\n",
    );
    let api = mock_github(root);
    add_fragment(root, "beryl", "Added", "protocol support");
    init_repo(root);
    let _origin = bare_origin(root);
    trellis_github(root, &api)
        .args(["release", "pr", "--pre", "rc", "--set", "beryl=1.0.0"])
        .assert()
        .success();
    git(root, &["merge", "--ff-only", "origin/release/pending"]);
    write(
        &root.join("examples/example/src/example.gleam"),
        "pub fn main() { Nil }\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "example"]);
    trellis_github(root, &api)
        .args(["release", "pr", "--auto-pre", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"outcome\": \"unchanged\""));
    write(
        &root.join("packages/beryl_protocol/src/beryl_protocol.gleam"),
        "pub fn main() { 1 }\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "protocol"]);
    trellis_github(root, &api)
        .args(["release", "pr", "--auto-pre"])
        .assert()
        .success();
    assert!(
        git_stdout(
            root,
            &["show", "origin/release/pending:packages/beryl/gleam.toml"]
        )
        .contains("1.0.0-rc.1")
    );
    assert!(
        git_stdout(
            root,
            &[
                "show",
                "origin/release/pending:packages/beryl_protocol/gleam.toml"
            ]
        )
        .contains("0.1.0")
    );
    assert!(
        !fs::read_to_string(root.join(".fake/github-log"))
            .unwrap()
            .contains("/releases")
    );
    // A stable package without fragments cannot silently ship a source change at its old version.
    git(root, &["merge", "--ff-only", "origin/release/pending"]);
    write(
        &root.join("packages/beryl_mist/src/beryl_mist.gleam"),
        "pub fn main() { 2 }\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "mist"]);
    trellis_github(root, &api)
        .args(["release", "pr", "--auto-pre"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("beryl_mist"))
        .stderr(predicate::str::contains("add a changelog fragment"));
    add_fragment(root, "beryl_mist", "Fixed", "mist fix");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "fragment"]);
    trellis_github(root, &api)
        .args(["release", "pr", "--auto-pre"])
        .assert()
        .success();
    assert!(
        git_stdout(
            root,
            &[
                "show",
                "origin/release/pending:packages/beryl_mist/gleam.toml"
            ]
        )
        .contains("0.4.1-rc.0")
    );
}

#[test]
fn fork_prs_are_ignored_and_matching_conflicts_are_rejected() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    let mut prs: Value =
        serde_json::from_str(&fs::read_to_string(f.path().join(".fake/pr-list")).unwrap()).unwrap();
    prs[0]["head"]["repo"]["full_name"] = json!("someone/fork");
    write(&f.path().join(".fake/pr-list"), &prs.to_string());
    assert_eq!(
        f.prepare(&["--resume", "--pre", "rc"])["pr_action"],
        "created"
    );
    f.pending();
    let mut prs: Value =
        serde_json::from_str(&fs::read_to_string(f.path().join(".fake/pr-list")).unwrap()).unwrap();
    let mut second = prs[0].clone();
    second["number"] = json!(8);
    prs.as_array_mut().unwrap().push(second.clone());
    write(&f.path().join(".fake/pr-list"), &prs.to_string());
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("multiple same-repository"));
    prs.as_array_mut().unwrap().pop();
    prs[0]["base"]["ref"] = json!("other");
    write(&f.path().join(".fake/pr-list"), &prs.to_string());
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("different --branch"));
    // A match on the second page is not hidden by 100 fork PRs.
    second["number"] = json!(9);
    let mut fork = second.clone();
    fork["head"]["repo"]["full_name"] = json!("someone/fork");
    write(
        &f.path().join(".fake/pr-list"),
        &json!(vec![fork; 100]).to_string(),
    );
    write(
        &f.path().join(".fake/pr-list-2"),
        &json!([second]).to_string(),
    );
    assert_eq!(f.prepare(&["--resume", "--skip-unchanged"])["pr_number"], 9);
}

#[test]
fn deleting_all_pending_content_is_not_a_successful_noop() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    fs::remove_file(f.path().join(".changes/unreleased/lat_core-1.toml")).unwrap();
    f.commit();
    let before = commit_of(f.path(), "origin/release/pending");
    let output = trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no valid replacement plan"));
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
}

#[test]
fn api_failure_and_closed_pr_do_not_emit_success_or_push() {
    let f = ReleaseFixture::new();
    write(&f.path().join(".fake/fail-pr"), "");
    let output = trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 500"));
    fs::remove_file(f.path().join(".fake/fail-pr")).unwrap();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    let mut pr: Value =
        serde_json::from_str::<Value>(&fs::read_to_string(f.path().join(".fake/pr-list")).unwrap())
            .unwrap()[0]
            .clone();
    pr["state"] = json!("closed");
    write(&f.path().join(".fake/pr-detail"), &pr.to_string());
    let before = commit_of(f.path(), "origin/release/pending");
    let output = trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
    assert_eq!(
        git_stdout(f.path(), &["branch", "--show-current"]).trim(),
        "main"
    );
}

#[test]
fn merged_candidates_cannot_change_series() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.merge();
    let before = commit_of(f.path(), "origin/release/pending");
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--auto-pre", "--set", "lat_core=2.0.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("candidate series"));
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
}

#[test]
fn edited_pending_versions_are_the_resumed_targets() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    git(
        f.path(),
        &["checkout", "--detach", "origin/release/pending"],
    );
    let path = f.path().join("packages/lat_core/gleam.toml");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("1.3.0-rc.0", "1.3.0-rc.7");
    write(&path, &text);
    f.commit();
    git(
        f.path(),
        &["push", "-f", "origin", "HEAD:refs/heads/release/pending"],
    );
    git(f.path(), &["checkout", "main"]);
    f.pending();
    assert_eq!(f.prepare(&["--resume"])["bumped"][0]["next"], "1.3.0-rc.7");
}

#[test]
fn backward_pending_versions_fail_without_push() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    git(
        f.path(),
        &["checkout", "--detach", "origin/release/pending"],
    );
    let manifest = f.path().join("packages/lat_core/gleam.toml");
    let text = fs::read_to_string(&manifest)
        .unwrap()
        .replace("1.3.0-rc.0", "1.1.0");
    write(&manifest, &text);
    f.commit();
    git(
        f.path(),
        &["push", "origin", "HEAD:refs/heads/release/pending"],
    );
    git(f.path(), &["checkout", "main"]);
    f.pending();
    let before = commit_of(f.path(), "origin/release/pending");
    trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not forward"));
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
    assert_eq!(git_stdout(f.path(), &["status", "--porcelain"]), "");
}

#[test]
fn missing_legacy_baseline_errors_instead_of_creating_repeated_candidates() {
    let f = ReleaseFixture::new();
    trellis_with_local_http(f.path())
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    fs::remove_file(f.path().join(".changes/lat_core/v1.3.0-rc.0.md")).unwrap();
    f.commit();
    let output = trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--auto-pre", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no release section"));
    assert_eq!(f.prepare(&["--pre", "rc"])["outcome"], "prepared");
}

#[test]
fn independently_committed_legacy_candidates_have_separate_baselines() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    for (name, version) in [("alpha", "1.0.0"), ("beta", "0.3.0")] {
        write(
            &root.join(format!("packages/{name}/gleam.toml")),
            &format!("name = \"{name}\"\nversion = \"{version}\"\n"),
        );
    }
    let api = mock_github(root);
    init_repo(root);
    let _origin = bare_origin(root);
    add_fragment(root, "alpha", "Fixed", "alpha fix");
    write(
        &root.join("packages/alpha/gleam.toml"),
        "name = \"alpha\"\nversion = \"1.0.1-rc.0\"\n",
    );
    write(
        &root.join(".changes/alpha/v1.0.1-rc.0.md"),
        "## 1.0.1-rc.0\n\n- alpha fix\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "alpha candidate"]);
    add_fragment(root, "beta", "Fixed", "beta fix");
    write(
        &root.join("packages/beta/gleam.toml"),
        "name = \"beta\"\nversion = \"0.3.1-rc.0\"\n",
    );
    write(
        &root.join(".changes/beta/v0.3.1-rc.0.md"),
        "## 0.3.1-rc.0\n\n- beta fix\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "beta candidate"]);
    trellis_github(root, &api)
        .args(["release", "pr", "--auto-pre", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"outcome\": \"unchanged\""));
}

#[test]
fn history_baseline_works_after_a_squash_merge_and_from_detached_callers() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    git(f.path(), &["merge", "--squash", "origin/release/pending"]);
    git(f.path(), &["commit", "-qm", "squashed candidate"]);
    let original = commit_of(f.path(), "HEAD");
    git(f.path(), &["checkout", "--detach", "HEAD"]);
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
    assert_eq!(commit_of(f.path(), "HEAD"), original);
    assert_eq!(
        git_stdout(f.path(), &["branch", "--show-current"]).trim(),
        ""
    );
}

#[test]
fn history_baseline_works_after_a_merge_commit() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    git(
        f.path(),
        &[
            "merge",
            "--no-ff",
            "-m",
            "merge candidate",
            "origin/release/pending",
        ],
    );
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
}

#[test]
fn nested_workspace_baselines_compare_workspace_relative_inputs() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("nested");
    copy_fixture_to(&workspace);
    let api = mock_github(root.path());
    add_fragment(&workspace, "lat_core", "Added", "nested feature");
    init_repo(root.path());
    let _origin = bare_origin(root.path());
    trellis_github(&workspace, &api)
        .args(["release", "pr", "--pre", "rc"])
        .assert()
        .success();
    git(
        root.path(),
        &["merge", "--ff-only", "origin/release/pending"],
    );
    trellis_github(&workspace, &api)
        .args(["release", "pr", "--auto-pre", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"outcome\": \"unchanged\""));
    add_fragment(&workspace, "lat_core", "Fixed", "nested fix");
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "nested fix"]);
    trellis_github(&workspace, &api)
        .args(["release", "pr", "--auto-pre"])
        .assert()
        .success();
}

#[test]
fn old_metadata_does_not_move_the_candidate_content_baseline() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.merge();
    let state = f.path().join(".changes/release.toml");
    write(&state, "obsolete metadata\n");
    write(
        &f.path().join("packages/lat_core/src/feature.gleam"),
        "pub fn main() { 1 }\n",
    );
    f.commit();
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "prepared");
}

#[test]
fn ignored_metadata_does_not_block_preparation() {
    let f = ReleaseFixture::new();
    write(
        &f.path().join(".gitignore"),
        ".fake/\n.changes/release.toml\n",
    );
    f.commit();
    assert_eq!(f.prepare(&["--pre", "rc"])["outcome"], "prepared");
    assert_eq!(version_of(f.path(), "lat_core"), "1.2.0");
    assert_eq!(git_stdout(f.path(), &["status", "--porcelain"]), "");
}

#[test]
fn edited_and_deleted_fragments_refresh_a_pending_candidate() {
    let f = ReleaseFixture::new();
    add_fragment(f.path(), "lat_mid", "Fixed", "second fragment");
    f.commit();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    fs::remove_file(f.path().join(".changes/unreleased/lat_mid-1.toml")).unwrap();
    f.commit();
    assert_eq!(
        f.prepare(&["--resume", "--skip-unchanged"])["outcome"],
        "prepared"
    );
    f.pending();
    write(
        &f.path().join(".changes/unreleased/lat_core-1.toml"),
        "package = \"lat_core\"\nkind = \"Added\"\nbody = \"edited feature\"\n",
    );
    f.commit();
    assert_eq!(
        f.prepare(&["--resume", "--skip-unchanged"])["outcome"],
        "prepared"
    );
}

#[test]
fn legacy_candidate_baseline_does_not_repeat_retained_fragments() {
    let f = ReleaseFixture::new();
    // Simulate a pre-state-file candidate with retained source fragments.
    trellis_with_local_http(f.path())
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    add_fragment(f.path(), "lat_core", "Added", "first feature");
    f.commit();
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
    add_fragment(f.path(), "lat_mid", "Fixed", "new feature");
    f.commit();
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "prepared");
}
