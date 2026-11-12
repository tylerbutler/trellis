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
    fn adapter() -> Self {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join(".gitignore"), ".fake/\n");
        write(
            &root.path().join("trellis.toml"),
            "[tools.trellis]\nmembers = [\"plugins/*\"]\n\
             [tools.trellis.adapter]\nmanifest = \"plugin.json\"\nversion = \"package.version\"\n",
        );
        write(
            &root.path().join("plugins/alpha/plugin.json"),
            "{\"name\":\"alpha\",\"package\":{\"version\":\"1.2.0\"}}\n",
        );
        write(
            &root.path().join("plugins/alpha/skills/task.md"),
            "first task\n",
        );
        let api = mock_github(root.path());
        add_fragment(root.path(), "alpha", "Added", "first feature");
        init_repo(root.path());
        let origin = bare_origin(root.path());
        Self {
            root,
            _origin: origin,
            api,
        }
    }

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
}

#[test]
fn adapter_pending_release_tracks_manifest_fields_content_and_configuration() {
    let f = ReleaseFixture::adapter();
    assert_eq!(f.prepare(&["--pre", "rc"])["outcome"], "prepared");
    f.pending();
    assert_eq!(
        f.prepare(&["--resume", "--skip-unchanged"])["outcome"],
        "unchanged"
    );

    write(
        &f.path().join("plugins/alpha/skills/task.md"),
        "changed task\n",
    );
    f.commit();
    assert_eq!(
        f.prepare(&["--resume", "--skip-unchanged"])["outcome"],
        "prepared"
    );
    f.pending();

    let config = f.path().join("trellis.toml");
    let text = fs::read_to_string(&config).unwrap();
    write(
        &config,
        &format!("{text}\n[tools.trellis.publish]\nexact_tag_format = \"{{name}}@{{version}}\"\n"),
    );
    f.commit();
    assert_eq!(
        f.prepare(&["--resume", "--skip-unchanged"])["outcome"],
        "prepared"
    );
}

#[test]
fn adapter_committed_candidates_continue_from_their_manifest_baseline() {
    let f = ReleaseFixture::adapter();
    f.prepare(&["--pre", "rc"]);
    f.merge();
    assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
    write(
        &f.path().join("plugins/alpha/skills/task.md"),
        "changed task\n",
    );
    f.commit();
    let result = f.prepare(&["--auto-pre"]);
    assert_eq!(result["outcome"], "prepared");
    assert_eq!(result["bumped"][0]["next"], "1.3.0-rc.1");
}

#[test]
fn yaml_and_toml_adapter_candidates_resume_and_continue_nested_versions() {
    for (manifest, contents) in [
        (
            "apm.yml",
            "name: alpha\npackage:\n  version: '1.2.0' # keep\n",
        ),
        (
            "package.toml",
            "name = \"alpha\"\n[package]\nversion = \"1.2.0\" # keep\n",
        ),
    ] {
        let f = ReleaseFixture::adapter();
        let config_path = f.path().join("trellis.toml");
        write(
            &config_path,
            &fs::read_to_string(&config_path)
                .unwrap()
                .replace("plugin.json", manifest),
        );
        write(&f.path().join("plugins/alpha").join(manifest), contents);
        f.commit();
        let result = f.prepare(&["--pre", "rc"]);
        assert_eq!(result["bumped"][0]["next"], "1.3.0-rc.0");
        let pending = git_stdout(
            f.path(),
            &[
                "show",
                &format!("origin/release/pending:plugins/alpha/{manifest}"),
            ],
        );
        assert_eq!(pending, contents.replace("1.2.0", "1.3.0-rc.0").trim_end());
        f.pending();
        assert_eq!(
            f.prepare(&["--resume", "--skip-unchanged"])["outcome"],
            "unchanged"
        );
        f.merge();
        assert_eq!(f.prepare(&["--auto-pre"])["outcome"], "unchanged");
        write(&f.path().join("plugins/alpha/skills/task.md"), "new task\n");
        f.commit();
        assert_eq!(
            f.prepare(&["--auto-pre"])["bumped"][0]["next"],
            "1.3.0-rc.1"
        );
    }
}

impl ReleaseFixture {
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

const PREPARE_ARTIFACTS: &str = "\
    sed -n 's/^version = \"\\(.*\\)\"/# lat_core \\1/p' packages/lat_core/gleam.toml > generated.md && \
    cp packages/lat_core/gleam.toml consumer.lock && \
    echo validated >> .fake/prepared && echo 'generator output'";

fn pr_mutations(f: &ReleaseFixture) -> usize {
    fs::read_to_string(f.path().join(".fake/github-log"))
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            line.starts_with("POST /repos/example/repo/pulls")
                || line.starts_with("PATCH /repos/example/repo/pulls/")
        })
        .count()
}

#[test]
fn prepare_command_includes_artifacts_in_first_push_and_validates_unchanged() {
    let f = ReleaseFixture::new();
    let first = f.prepare(&["--pre", "rc", "--prepare-command", PREPARE_ARTIFACTS]);
    assert_eq!(first["outcome"], "prepared");
    let before = commit_of(f.path(), "origin/release/pending");
    assert_eq!(
        git_stdout(f.path(), &["show", "origin/release/pending:generated.md"]),
        "# lat_core 1.3.0-rc.0"
    );
    assert!(
        git_stdout(f.path(), &["show", "origin/release/pending:consumer.lock"])
            .contains("version = \"1.3.0-rc.0\"")
    );
    f.pending();
    let mutations = pr_mutations(&f);
    for mode in ["--json", "--github-output"] {
        let output = trellis_github(f.path(), &f.api)
            .args([
                "release",
                "pr",
                "--resume",
                "--skip-unchanged",
                "--prepare-command",
                PREPARE_ARTIFACTS,
                mode,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("generator output"));
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("generator output"));
        if mode == "--json" {
            let result: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(result["outcome"], "unchanged");
            assert_eq!(result["pr_number"], 7);
        } else {
            assert!(stdout.contains("outcome=unchanged"));
        }
        assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
        assert_eq!(pr_mutations(&f), mutations);
    }
    assert_eq!(
        fs::read_to_string(f.path().join(".fake/prepared"))
            .unwrap()
            .lines()
            .count(),
        3
    );
}

#[test]
fn prepare_command_repairs_older_pending_preparation_once_without_losing_head_files() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    git(
        f.path(),
        &["checkout", "--detach", "origin/release/pending"],
    );
    write(&f.path().join("generated.md"), "# lat_core 1.2.0\n");
    write(&f.path().join("consumer.lock"), "stale consumer lock\n");
    write(&f.path().join("keep.txt"), "pending-only content\n");
    f.commit();
    git(
        f.path(),
        &["push", "origin", "HEAD:refs/heads/release/pending"],
    );
    git(f.path(), &["checkout", "main"]);
    f.pending();
    let before = commit_of(f.path(), "origin/release/pending");
    let repaired = f.prepare(&[
        "--resume",
        "--skip-unchanged",
        "--prepare-command",
        PREPARE_ARTIFACTS,
    ]);
    assert_eq!(repaired["outcome"], "prepared");
    assert_eq!(repaired["pr_number"], 7);
    assert_eq!(repaired["pr_action"], "updated");
    assert_eq!(repaired["bumped"][0]["next"], "1.3.0-rc.0");
    assert_eq!(
        git_stdout(f.path(), &["rev-parse", "origin/release/pending^"]),
        before
    );
    assert_eq!(
        git_stdout(f.path(), &["show", "origin/release/pending:keep.txt"]),
        "pending-only content"
    );
    assert_eq!(
        git_stdout(f.path(), &["show", "origin/release/pending:generated.md"]),
        "# lat_core 1.3.0-rc.0"
    );
    assert!(
        git_stdout(f.path(), &["show", "origin/release/pending:consumer.lock"])
            .contains("1.3.0-rc.0")
    );
    f.pending();
    let repaired_head = commit_of(f.path(), "origin/release/pending");
    assert_eq!(
        f.prepare(&[
            "--resume",
            "--skip-unchanged",
            "--prepare-command",
            PREPARE_ARTIFACTS,
        ])["outcome"],
        "unchanged"
    );
    assert_eq!(commit_of(f.path(), "origin/release/pending"), repaired_head);
}

#[test]
fn prepare_command_failure_never_pushes_or_mutates_a_pr() {
    for pending in [false, true] {
        let f = ReleaseFixture::new();
        if pending {
            f.prepare(&["--pre", "rc"]);
            f.pending();
        }
        let before = git_stdout(f._origin.path(), &["for-each-ref", "refs/heads/"]);
        let mutations = pr_mutations(&f);
        let output = trellis_github(f.path(), &f.api)
            .args([
                "release",
                "pr",
                "--pre",
                "rc",
                "--resume",
                "--skip-unchanged",
                "--json",
                "--prepare-command",
                "echo incomplete > generated.md; echo 'generation failed' >&2; exit 23",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("generation failed"), "{stderr}");
        assert!(stderr.contains("23"), "{stderr}");
        assert_eq!(
            fs::read_to_string(f.path().join("generated.md")).unwrap(),
            "incomplete\n"
        );
        assert_eq!(
            git_stdout(f._origin.path(), &["for-each-ref", "refs/heads/"]),
            before
        );
        assert_eq!(pr_mutations(&f), mutations);
    }
}

#[test]
fn prepare_command_keeps_initial_lease_in_first_preparation_and_unchanged_validation() {
    for pending in [false, true] {
        let f = ReleaseFixture::new();
        if pending {
            f.prepare(&["--pre", "rc"]);
            f.pending();
        }
        git(f.path(), &["push", "origin", "main:refs/heads/concurrent"]);
        let concurrent = commit_of(f.path(), "main");
        let command = format!(
            "git --git-dir='{}' update-ref refs/heads/release/pending {concurrent}",
            f._origin.path().display()
        );
        let mutations = pr_mutations(&f);
        let output = trellis_github(f.path(), &f.api)
            .args([
                "release",
                "pr",
                "--pre",
                "rc",
                "--resume",
                "--skip-unchanged",
                "--json",
                "--prepare-command",
                &command,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert_eq!(commit_of(f._origin.path(), "release/pending"), concurrent);
        assert_eq!(pr_mutations(&f), mutations);
    }
}

#[test]
fn prepare_command_rechecks_selected_pr_even_when_validation_makes_no_changes() {
    for change in ["closed", "retargeted", "head"] {
        let f = ReleaseFixture::new();
        f.prepare(&["--pre", "rc"]);
        f.pending();
        let before = commit_of(f.path(), "origin/release/pending");
        let mut pr: Value = serde_json::from_str::<Value>(
            &fs::read_to_string(f.path().join(".fake/pr-list")).unwrap(),
        )
        .unwrap()[0]
            .clone();
        match change {
            "closed" => pr["state"] = json!("closed"),
            "retargeted" => pr["base"]["ref"] = json!("other"),
            _ => pr["head"]["sha"] = json!(commit_of(f.path(), "main")),
        }
        let command = format!("printf '%s' '{}' > .fake/pr-detail", pr);
        let mutations = pr_mutations(&f);
        let output = trellis_github(f.path(), &f.api)
            .args([
                "release",
                "pr",
                "--resume",
                "--skip-unchanged",
                "--json",
                "--prepare-command",
                &command,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(f.path().join(".fake/pr-detail").exists());
        assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
        assert_eq!(pr_mutations(&f), mutations);
    }
}

#[test]
fn prepare_command_validates_merged_candidate_but_requires_force_for_repairs() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc", "--prepare-command", PREPARE_ARTIFACTS]);
    f.merge();
    let before = commit_of(f.path(), "origin/release/pending");
    let mutations = pr_mutations(&f);
    let unchanged = f.prepare(&["--auto-pre", "--prepare-command", PREPARE_ARTIFACTS]);
    assert_eq!(unchanged["outcome"], "unchanged");
    let output = trellis_github(f.path(), &f.api)
        .args([
            "release",
            "pr",
            "--auto-pre",
            "--json",
            "--prepare-command",
            "echo repair > generated.md",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--force"));
    assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
    assert_eq!(pr_mutations(&f), mutations);
}

#[test]
fn prepare_command_does_not_run_without_a_release_plan() {
    let f = ReleaseFixture::new();
    fs::remove_file(f.path().join(".changes/unreleased/lat_core-1.toml")).unwrap();
    f.commit();
    assert_eq!(
        f.prepare(&["--prepare-command", "echo unexpected > .fake/prepared"])["outcome"],
        "no_changes"
    );
    assert!(!f.path().join(".fake/prepared").exists());
    assert_eq!(pr_mutations(&f), 0);
}

#[test]
fn prepare_command_cannot_replace_the_selected_versions_or_commit() {
    for command in [
        "git commit --allow-empty -qm 'hook commit'",
        "git checkout -q main",
        "sed 's/1.3.0-rc.0/9.9.9/' packages/lat_core/gleam.toml > changed && mv changed packages/lat_core/gleam.toml",
    ] {
        let f = ReleaseFixture::new();
        f.prepare(&["--pre", "rc"]);
        f.pending();
        let before = commit_of(f.path(), "origin/release/pending");
        let mutations = pr_mutations(&f);
        let output = trellis_github(f.path(), &f.api)
            .args([
                "release",
                "pr",
                "--resume",
                "--skip-unchanged",
                "--json",
                "--prepare-command",
                command,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("prepare command"));
        assert_eq!(commit_of(f.path(), "origin/release/pending"), before);
        assert_eq!(pr_mutations(&f), mutations);
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
fn post_push_stale_head_converges_before_pr_update_and_success_outputs() {
    for output_flag in ["--json", "--github-output"] {
        let f = ReleaseFixture::new();
        f.prepare(&["--pre", "rc"]);
        f.pending();
        let prs: Value =
            serde_json::from_str(&fs::read_to_string(f.path().join(".fake/pr-list")).unwrap())
                .unwrap();
        let previous = &prs[0];
        let before = commit_of(f._origin.path(), "refs/heads/release/pending");
        add_fragment(f.path(), "lat_core", "Fixed", "second feature");
        f.commit();
        let original = commit_of(f.path(), "HEAD");
        write(
            &f.path().join(".fake/pr-detail-sequence"),
            &json!([previous, previous, previous, null]).to_string(),
        );
        fs::remove_file(f.path().join(".fake/github-log")).unwrap();

        let output = trellis_github(f.path(), &f.api)
            .args(["release", "pr", "--resume", output_flag])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        if output_flag == "--json" {
            let document: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(document["outcome"], "prepared");
            assert_eq!(document["pr_action"], "updated");
            assert_eq!(document["pr_number"], 7);
        } else {
            assert!(stdout.contains("outcome=prepared\n"), "{stdout}");
            assert!(stdout.contains("pr_action=updated\n"), "{stdout}");
            assert!(stdout.contains("pr_number=7\n"), "{stdout}");
        }
        assert_ne!(
            commit_of(f._origin.path(), "refs/heads/release/pending"),
            before
        );
        assert_eq!(commit_of(f.path(), "HEAD"), original);
        assert_eq!(git_stdout(f.path(), &["branch", "--show-current"]), "main");
        let log = fs::read_to_string(f.path().join(".fake/github-log")).unwrap();
        assert_eq!(log.matches("GET /repos/example/repo/pulls/7\n").count(), 4);
        assert_eq!(
            log.matches("PATCH /repos/example/repo/pulls/7\n").count(),
            1
        );
        assert!(log.contains("second feature"), "{log}");
    }
}

#[test]
fn post_push_persistent_stale_head_reports_incomplete_finalization() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    let prs: Value =
        serde_json::from_str(&fs::read_to_string(f.path().join(".fake/pr-list")).unwrap()).unwrap();
    write(&f.path().join(".fake/pr-detail"), &prs[0].to_string());
    let before = commit_of(f._origin.path(), "refs/heads/release/pending");
    add_fragment(f.path(), "lat_core", "Fixed", "second feature");
    f.commit();
    fs::remove_file(f.path().join(".fake/github-log")).unwrap();

    let output = trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume", "--github-output"])
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("branch `release/pending` was pushed successfully"),
        "{stderr}"
    );
    assert!(
        stderr.contains("PR finalization remains incomplete"),
        "{stderr}"
    );
    assert!(
        stderr.contains("still reports the previous head"),
        "{stderr}"
    );
    assert_ne!(
        commit_of(f._origin.path(), "refs/heads/release/pending"),
        before
    );
    assert_eq!(git_stdout(f.path(), &["branch", "--show-current"]), "main");
    let log = fs::read_to_string(f.path().join(".fake/github-log")).unwrap();
    let reads = log.matches("GET /repos/example/repo/pulls/7\n").count();
    assert!((3..=12).contains(&reads), "{log}");
    assert!(!log.contains("PATCH "), "{log}");
}

#[test]
fn post_push_stale_head_does_not_hide_concurrent_pr_changes() {
    for (pointer, value, message) in [
        (
            "/head/sha",
            json!("1".repeat(40)),
            "changed during preparation",
        ),
        ("/state", json!("closed"), "closed, merged, or retargeted"),
        ("/base/ref", json!("other"), "targeting `other`"),
        ("/head/ref", json!("other"), "closed, merged, or retargeted"),
        (
            "/head/repo/full_name",
            json!("fork/repo"),
            "closed, merged, or retargeted",
        ),
        (
            "/base/repo/full_name",
            json!("other/repo"),
            "closed, merged, or retargeted",
        ),
        ("/number", json!(8), "changed during preparation"),
        (
            "/html_url",
            json!("https://github.com/example/repo/pull/8"),
            "changed during preparation",
        ),
        ("/head/sha", json!("invalid"), "invalid head SHA"),
    ] {
        let f = ReleaseFixture::new();
        f.prepare(&["--pre", "rc"]);
        f.pending();
        let prs: Value =
            serde_json::from_str(&fs::read_to_string(f.path().join(".fake/pr-list")).unwrap())
                .unwrap();
        let previous = &prs[0];
        let mut changed = previous.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        write(
            &f.path().join(".fake/pr-detail-sequence"),
            &json!([previous, previous, changed]).to_string(),
        );
        let before = commit_of(f._origin.path(), "refs/heads/release/pending");
        add_fragment(f.path(), "lat_core", "Fixed", "second feature");
        f.commit();
        fs::remove_file(f.path().join(".fake/github-log")).unwrap();

        let output = trellis_github(f.path(), &f.api)
            .args(["release", "pr", "--resume", "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{pointer}");
        assert!(output.stdout.is_empty(), "{pointer}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{pointer}: {stderr}");
        assert!(
            stderr.contains("PR finalization remains incomplete"),
            "{stderr}"
        );
        assert_ne!(
            commit_of(f._origin.path(), "refs/heads/release/pending"),
            before
        );
        assert_eq!(git_stdout(f.path(), &["branch", "--show-current"]), "main");
        let log = fs::read_to_string(f.path().join(".fake/github-log")).unwrap();
        assert_eq!(
            log.matches("GET /repos/example/repo/pulls/7\n").count(),
            3,
            "{pointer}"
        );
        assert!(!log.contains("PATCH "), "{pointer}: {log}");
    }
}

#[test]
fn post_push_pr_update_failure_reports_incomplete_finalization() {
    let f = ReleaseFixture::new();
    f.prepare(&["--pre", "rc"]);
    f.pending();
    let before = commit_of(f._origin.path(), "refs/heads/release/pending");
    add_fragment(f.path(), "lat_core", "Fixed", "second feature");
    f.commit();
    write(&f.path().join(".fake/fail-pr-update"), "");

    let output = trellis_github(f.path(), &f.api)
        .args(["release", "pr", "--resume", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("branch `release/pending` was pushed successfully"),
        "{stderr}"
    );
    assert!(
        stderr.contains("PR finalization remains incomplete"),
        "{stderr}"
    );
    assert!(
        stderr.contains("HTTP 500: PR update unavailable"),
        "{stderr}"
    );
    assert_ne!(
        commit_of(f._origin.path(), "refs/heads/release/pending"),
        before
    );
    assert_eq!(git_stdout(f.path(), &["branch", "--show-current"]), "main");
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
