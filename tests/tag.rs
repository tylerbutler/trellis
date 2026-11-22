//! End-to-end tests for `trellis tag`.

mod common;

use common::{
    bare_origin, commit_of, copy_fixture_to, git, git_stdout, init_repo, series_repo, set_version,
    trellis_github, trellis_with_local_http as trellis, write,
};
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;

// ---- tag ----------------------------------------------------------------

fn policy_repo(root: &std::path::Path, primary: &str) -> tempfile::TempDir {
    let remote = series_repo(
        root,
        &format!("primary_package = \"{primary}\"\npackage_tags = [\"exact\"]"),
    );
    write(&root.join(".fake/latest-policy"), "");
    remote
}

fn seed_release(root: &std::path::Path, id: u64, tag: &str, draft: bool, prerelease: bool) {
    write(
        &root.join(format!(".fake/release-{tag}")),
        &serde_json::json!({
            "id": id, "tag_name": tag, "draft": draft, "prerelease": prerelease,
            "target_commitish": "main", "body": "existing notes", "assets": [],
        })
        .to_string(),
    );
}

fn release_requests(root: &std::path::Path) -> Vec<(String, serde_json::Value)> {
    fs::read_to_string(root.join(".fake/github-log"))
        .unwrap()
        .split("---")
        .filter_map(|record| {
            let (request, body) = record.trim().split_once('\n')?;
            Some((
                request.to_string(),
                if body.trim().is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_str(body).unwrap()
                },
            ))
        })
        .collect()
}

#[test]
fn primary_package_latest_is_independent_of_batch_order_and_bootstrap_retries() {
    for primary in ["lat_core", "lat_cli", "lat_mid"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _remote = policy_repo(root, primary);
        let api = common::mock_github(root);
        for command in ["tag", "release"] {
            trellis_github(root, &api)
                .args([
                    command,
                    if command == "tag" {
                        "create"
                    } else {
                        "bootstrap"
                    },
                    "--github-release",
                ])
                .assert()
                .success();
            let version = common::version_of(root, primary);
            assert_eq!(
                fs::read_to_string(root.join(".fake/latest")).unwrap(),
                format!("{primary}-v{version}")
            );
            write(&root.join(".fake/latest"), "lat_cli-v0.3.1");
        }
        let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
        assert_eq!(
            log.matches("POST /repos/example/repo/releases\n").count(),
            3
        );
        let requests = release_requests(root);
        let creates: Vec<_> = requests
            .iter()
            .filter(|(request, _)| request == "POST /repos/example/repo/releases")
            .collect();
        assert_eq!(creates.len(), 3);
        assert!(
            creates
                .iter()
                .all(|(_, body)| body["make_latest"] == "false")
        );
    }
}

#[test]
fn secondary_only_release_restores_highest_stable_primary_from_all_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = policy_repo(root, "lat_core");
    let api = common::mock_github(root);
    for id in 1..=100 {
        seed_release(root, id, &format!("unrelated-{id}"), false, false);
    }
    seed_release(root, 101, "lat_core-v3.0.0", false, false);
    seed_release(root, 102, "lat_core-v4.0.0", true, false);
    seed_release(root, 103, "lat_core-v5.0.0", false, true);
    seed_release(root, 104, "lat_core-v6.0.0-rc.1", false, false);
    write(&root.join(".fake/latest"), "unrelated-1");
    trellis_github(root, &api)
        .args([
            "tag",
            "create",
            "--github-release",
            "--package",
            "lat_cli",
            "--finalize",
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(root.join(".fake/latest")).unwrap(),
        "lat_core-v3.0.0"
    );
    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(root.join(".fake/latest")).unwrap(),
        "lat_core-v3.0.0"
    );
}

#[test]
fn primary_latest_failure_is_visible_and_published_retry_repairs_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = policy_repo(root, "lat_core");
    let api = common::mock_github(root);
    write(&root.join(".fake/fail-latest"), "");
    let args = [
        "tag",
        "create",
        "--github-release",
        "--package",
        "lat_core",
        "--finalize",
    ];
    trellis_github(root, &api)
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains("retry"));
    let release: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".fake/release-lat_core-v1.2.0")).unwrap(),
    )
    .unwrap();
    assert_eq!(release["draft"], false);
    fs::remove_file(root.join(".fake/fail-latest")).unwrap();
    trellis_github(root, &api).args(args).assert().success();
    assert_eq!(
        fs::read_to_string(root.join(".fake/latest")).unwrap(),
        "lat_core-v1.2.0"
    );
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        log.matches("POST /repos/example/repo/releases\n").count(),
        1
    );
}

#[test]
fn primary_policy_drafts_dry_runs_and_no_primary_do_not_promote_secondary() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = policy_repo(root, "lat_core");
    let api = common::mock_github(root);
    let args = ["tag", "create", "--github-release", "--package", "lat_cli"];
    trellis_github(root, &api)
        .args(args)
        .args(["--finalize", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Latest"));
    assert!(!root.join(".fake/latest").exists());
    assert!(
        !fs::read_to_string(root.join(".fake/github-log"))
            .unwrap()
            .contains("POST ")
    );
    trellis_github(root, &api)
        .args(args)
        .arg("--draft")
        .assert()
        .success();
    assert!(!root.join(".fake/latest").exists());
    trellis_github(root, &api)
        .args(args)
        .arg("--finalize")
        .assert()
        .success()
        .stdout(predicate::str::contains("no published stable release"));
    assert!(!root.join(".fake/latest").exists());
}

#[test]
fn primary_policy_uses_custom_tags_and_semver_precedence_not_build_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "primary_package = \"lat_core\"\nexact_tag_format = \"release-{version}-{name}\"",
    );
    let api = common::mock_github(root);
    seed_release(root, 1, "release-3.0.0+alpha-lat_core", false, false);
    seed_release(root, 2, "release-3.0.0+zulu-lat_core", false, false);
    seed_release(root, 3, "release-99.0.0-lat_cli", false, false);
    write(&root.join(".fake/latest"), "release-3.0.0+zulu-lat_core");
    let args = [
        "tag",
        "create",
        "--github-release",
        "--package",
        "lat_cli",
        "--finalize",
    ];
    trellis_github(root, &api).args(args).assert().success();
    assert_eq!(
        fs::read_to_string(root.join(".fake/latest")).unwrap(),
        "release-3.0.0+zulu-lat_core"
    );
    write(&root.join(".fake/latest"), "release-99.0.0-lat_cli");
    trellis_github(root, &api).args(args).assert().success();
    assert_eq!(
        fs::read_to_string(root.join(".fake/latest")).unwrap(),
        "release-3.0.0+alpha-lat_core"
    );
}

#[test]
fn primary_prerelease_does_not_displace_stable_and_draft_staging_does_not_repair_latest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = policy_repo(root, "lat_core");
    let api = common::mock_github(root);
    seed_release(root, 1, "lat_core-v1.1.0", false, false);
    seed_release(root, 2, "unrelated", false, false);
    write(&root.join(".fake/latest"), "unrelated");
    set_version(root, "lat_core", "2.0.0-rc.1");
    git(root, &["commit", "-qam", "prepare candidate"]);
    let args = ["tag", "create", "--github-release", "--package", "lat_core"];
    trellis_github(root, &api)
        .args(args)
        .arg("--draft")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(root.join(".fake/latest")).unwrap(),
        "unrelated"
    );
    trellis_github(root, &api)
        .args(args)
        .arg("--finalize")
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(root.join(".fake/latest")).unwrap(),
        "lat_core-v1.1.0"
    );
    let release: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".fake/release-lat_core-v2.0.0-rc.1")).unwrap(),
    )
    .unwrap();
    assert_eq!(release["prerelease"], true);
    assert_eq!(release["make_latest"], "false");
}

#[test]
fn primary_policy_api_failures_stop_before_tag_writes() {
    for malformed in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _remote = policy_repo(root, "lat_core");
        let api = common::mock_github(root);
        if malformed {
            write(
                &root.join(".fake/release-broken"),
                r#"{"id":1,"tag_name":"broken","draft":false}"#,
            );
        } else {
            seed_release(root, 1, "lat_core-v1.1.0", false, false);
            write(&root.join(".fake/fail-list"), "");
        }
        trellis_github(root, &api)
            .args(["tag", "create", "--github-release"])
            .assert()
            .failure();
        assert!(git_stdout(root, &["tag", "--list"]).is_empty());
        assert!(git_stdout(root, &["ls-remote", "--tags", "origin"]).is_empty());
        assert!(
            !fs::read_to_string(root.join(".fake/github-log"))
                .unwrap()
                .contains("POST ")
        );
    }
}

#[test]
fn primary_policy_verifies_required_assets_before_promoting_existing_release() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = policy_repo(root, "lat_core");
    let api = common::mock_github(root);
    let intent = serde_json::json!({
        "revision": "old", "assets": { "sbom.json": format!("sha256:{}", "0".repeat(64)) },
    });
    write(
        &root.join(".fake/release-lat_core-v3.0.0"),
        &serde_json::json!({
            "id": 1, "tag_name": "lat_core-v3.0.0", "draft": false, "prerelease": false,
            "target_commitish": "main", "assets": [],
            "body": format!("notes\n<!-- trellis.release_assets/1\n{intent}\n-->"),
        })
        .to_string(),
    );
    trellis_github(root, &api)
        .args([
            "tag",
            "create",
            "--github-release",
            "--package",
            "lat_cli",
            "--finalize",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("sbom.json").and(predicate::str::contains("missing")));
    assert!(!root.join(".fake/latest").exists());
}

#[test]
fn unconfigured_releases_keep_native_defaults_and_plain_tags_do_not_query_latest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, "package_tags = [\"exact\"]");
    let api = common::mock_github(root);
    let args = [
        "tag",
        "create",
        "--github-release",
        "--package",
        "lat_core",
        "--finalize",
    ];
    trellis_github(root, &api).args(args).assert().success();
    for (request, payload) in release_requests(root) {
        assert!(!request.contains("/latest"));
        assert!(payload.get("make_latest").is_none());
    }
    fs::remove_file(root.join(".fake/github-log")).unwrap();
    trellis_github(root, &api)
        .args(["tag", "create"])
        .assert()
        .success();
    assert!(!root.join(".fake/github-log").exists());
}

fn legacy_alias_repo(root: &std::path::Path) -> tempfile::TempDir {
    write(&root.join("README.md"), "Existing actions\n");
    init_repo(root);
    git(root, &["tag", "-a", "v1", "-m", "legacy alias"]);
    let remote = bare_origin(root);
    git(root, &["push", "-q", "origin", "v1"]);
    write(
        &root.join("trellis.toml"),
        "[tools.trellis.adapter]\nmanifest = \"release.toml\"\n\
         [tools.trellis.publish]\nexact_tag_format = \"v{version}\"\n\
         package_tags = [\"exact\"]\nrepository_tag_package = \"actions\"\n\
         repository_tag_format = \"v{series}\"\nrepository_tags = [\"major\", \"minor\"]\n",
    );
    write(
        &root.join("release.toml"),
        "name = \"actions\"\nversion = \"1.0.0\"\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "adopt version manifest"]);
    remote
}

#[test]
fn legacy_alias_adoption_requires_explicit_mode_and_preserves_preview_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = legacy_alias_repo(root);
    let local = git_stdout(root, &["show-ref", "--tags"]);
    let remote = git_stdout(root, &["ls-remote", "--tags", "origin"]);
    trellis(root).args(["doctor"]).assert().success();
    for args in [
        vec!["tag", "plan", "--json"],
        vec!["tag", "create", "--push", "--dry-run"],
    ] {
        trellis(root)
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("--force=adopt"));
    }
    let output = trellis(root)
        .args(["tag", "plan", "--json", "--force=adopt"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let alias = document["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tag| tag["tag"] == "v1")
        .unwrap();
    assert_eq!(alias["kind"], "repository_series");
    assert_eq!(alias["action"], "move");
    trellis(root)
        .args(["tag", "create", "--push", "--dry-run", "--force=adopt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would force-push v1"));
    assert_eq!(git_stdout(root, &["show-ref", "--tags"]), local);
    assert_eq!(git_stdout(root, &["ls-remote", "--tags", "origin"]), remote);
}

#[test]
fn legacy_alias_adoption_pushes_once_and_keeps_exact_tags_immutable() {
    for command in [["tag", "create"], ["release", "bootstrap"]] {
        for remote_only in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            let _remote = legacy_alias_repo(root);
            let legacy = commit_of(root, "v1");
            git(root, &["tag", "unrelated", &legacy]);
            if remote_only {
                git(root, &["tag", "-d", "v1"]);
                git(
                    root,
                    &["config", "remote.origin.fetch", "+refs/tags/*:refs/tags/*"],
                );
            }
            let before = git_stdout(root, &["show-ref", "--tags"]);
            trellis(root)
                .args(command)
                .args(["--push", "--dry-run", "--force=adopt"])
                .assert()
                .success();
            assert_eq!(git_stdout(root, &["show-ref", "--tags"]), before);
            trellis(root)
                .args(command)
                .args(["--push", "--force=adopt"])
                .assert()
                .success();
            let release = commit_of(root, "HEAD");
            for tag in ["v1", "v1.0", "v1.0.0"] {
                assert_eq!(commit_of(root, tag), release);
                assert!(
                    git_stdout(
                        root,
                        &["ls-remote", "origin", &format!("refs/tags/{tag}^{{}}")]
                    )
                    .starts_with(&release)
                );
            }
            assert_eq!(commit_of(root, "unrelated"), legacy);
            let exact = git_stdout(root, &["rev-parse", "refs/tags/v1.0.0"]);
            let refs = git_stdout(root, &["ls-remote", "--tags", "origin"]);
            git(root, &["commit", "-q", "--allow-empty", "-m", "unrelated"]);
            trellis(root).args(command).arg("--push").assert().success();
            assert_eq!(git_stdout(root, &["ls-remote", "--tags", "origin"]), refs);
            assert_eq!(git_stdout(root, &["rev-parse", "refs/tags/v1.0.0"]), exact);
        }
    }
}

#[test]
fn legacy_alias_adoption_rejects_unrelated_or_deleted_manifest_history() {
    for deleted in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _remote = legacy_alias_repo(root);
        if deleted {
            git(root, &["rm", "release.toml"]);
            git(root, &["commit", "-qm", "delete manifest"]);
        } else {
            git(root, &["checkout", "--orphan", "unrelated"]);
            git(root, &["rm", "-rf", "."]);
            write(&root.join("README.md"), "Unrelated history\n");
            git(root, &["add", "."]);
            git(root, &["commit", "-qm", "unrelated history"]);
        }
        git(root, &["tag", "-f", "v1"]);
        git(root, &["push", "-q", "--force", "origin", "v1"]);
        if deleted {
            write(
                &root.join("release.toml"),
                "name = \"actions\"\nversion = \"1.0.0\"\n",
            );
            git(root, &["add", "."]);
            git(root, &["commit", "-qm", "restore manifest"]);
        }
        git(root, &["checkout", "-q", "main"]);
        let refs = git_stdout(root, &["ls-remote", "--tags", "origin"]);
        for args in [
            vec!["tag", "plan", "--force=adopt"],
            vec!["tag", "create", "--push", "--force=adopt"],
        ] {
            trellis(root)
                .args(args)
                .assert()
                .code(3)
                .stderr(predicate::str::contains(if deleted {
                    "history already contains the anchor manifest"
                } else {
                    "not an ancestor of HEAD"
                }));
        }
        assert_eq!(git_stdout(root, &["ls-remote", "--tags", "origin"]), refs);
        assert!(!git_stdout(root, &["tag", "--list"]).contains("v1.0.0"));
    }
}

#[test]
fn legacy_alias_adoption_rejects_invalid_existing_manifests() {
    for contents in ["not toml!", "name = \"actions\"\n", "version = \"bad\"\n"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let _remote = legacy_alias_repo(root);
        git(root, &["checkout", "-q", "-b", "invalid", "v1"]);
        write(&root.join("release.toml"), contents);
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "invalid historical manifest"]);
        git(root, &["tag", "-f", "v1"]);
        git(root, &["push", "-q", "--force", "origin", "v1"]);
        git(root, &["checkout", "-q", "main"]);
        for args in [
            vec!["tag", "plan", "--force=adopt"],
            vec!["tag", "create", "--push", "--force=adopt"],
        ] {
            trellis(root).args(args).assert().code(3).stderr(
                predicate::str::contains("cannot parse repository tag anchor manifest")
                    .or(predicate::str::contains("invalid anchor package version")),
            );
        }
        assert!(!git_stdout(root, &["tag", "--list"]).contains("v1.0.0"));
    }
}

#[test]
fn legacy_alias_adoption_rejects_shallow_history() {
    let source = tempfile::tempdir().unwrap();
    let _remote = legacy_alias_repo(source.path());
    let clone = tempfile::tempdir().unwrap();
    git(
        clone.path(),
        &[
            "clone",
            "-q",
            "--depth=1",
            "--no-local",
            source.path().to_str().unwrap(),
            ".",
        ],
    );
    git(
        clone.path(),
        &["fetch", "-q", "origin", "refs/tags/v1:refs/tags/v1"],
    );
    trellis(clone.path())
        .args(["tag", "plan", "--force=adopt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("shallow"));
    git(clone.path(), &["fetch", "-q", "--unshallow", "origin"]);
    trellis(clone.path())
        .args(["tag", "plan", "--force=adopt"])
        .assert()
        .success();
}

#[test]
fn legacy_alias_adoption_does_not_override_exact_tag_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = legacy_alias_repo(root);
    git(root, &["tag", "v1.0.0", "v1"]);
    git(root, &["push", "-q", "origin", "v1.0.0"]);
    git(root, &["tag", "-f", "v1.0.0", "HEAD"]);
    let refs = git_stdout(root, &["ls-remote", "--tags", "origin"]);
    trellis(root)
        .args(["tag", "create", "--push", "--force=adopt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("different objects"));
    assert_eq!(git_stdout(root, &["ls-remote", "--tags", "origin"]), refs);
}

#[test]
fn legacy_alias_adoption_never_moves_versioned_aliases_backward() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = legacy_alias_repo(root);
    let first = commit_of(root, "HEAD");
    write(
        &root.join("release.toml"),
        "name = \"actions\"\nversion = \"1.0.1\"\n",
    );
    git(root, &["commit", "-qam", "newer release"]);
    git(root, &["tag", "-f", "v1"]);
    git(root, &["push", "-q", "--force", "origin", "v1"]);
    git(root, &["checkout", "-q", &first]);
    for args in [
        vec!["tag", "plan", "--force=adopt"],
        vec!["tag", "create", "--push", "--force=adopt"],
    ] {
        trellis(root)
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("refusing to move it backward"));
    }
}

#[test]
fn legacy_alias_adoption_is_available_locally_and_requires_a_valid_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = legacy_alias_repo(root);
    for command in [["tag", "plan"], ["tag", "create"], ["release", "bootstrap"]] {
        for force in ["--force", "--force=anything"] {
            trellis(root).args(command).arg(force).assert().code(2);
        }
    }
    trellis(root)
        .args([
            "tag",
            "create",
            "--force=adopt",
            "--github-release",
            "--package",
            "actions",
        ])
        .assert()
        .code(2);
    let remote = git_stdout(root, &["ls-remote", "--tags", "origin"]);
    trellis(root)
        .args(["tag", "create", "--force=adopt"])
        .assert()
        .success();
    assert_eq!(commit_of(root, "v1"), commit_of(root, "HEAD"));
    assert_eq!(git_stdout(root, &["ls-remote", "--tags", "origin"]), remote);
}

#[test]
fn legacy_alias_adoption_does_not_hide_git_read_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = legacy_alias_repo(root);
    let tree = git_stdout(root, &["rev-parse", "v1^{tree}"]);
    fs::remove_file(root.join(".git/objects").join(&tree[..2]).join(&tree[2..])).unwrap();
    trellis(root)
        .args(["tag", "plan", "--force=adopt"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("cannot inspect anchor manifest"));
    assert!(!git_stdout(root, &["tag", "--list"]).contains("v1.0.0"));
}

#[test]
fn legacy_alias_adoption_leases_the_validated_remote_object_and_retries_safely() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = legacy_alias_repo(root);
    git(root, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    let release = commit_of(root, "HEAD");
    let hook = root.join(".git/hooks/pre-push");
    write(
        &hook,
        "#!/bin/sh\nset -eu\n\
         while read local_ref local_oid remote_ref remote_oid; do\n\
           if [ \"$local_ref\" = refs/tags/v1.0.0 ]; then\n\
             git -c safe.bareRepository=all -C \"$TEST_REMOTE\" update-ref refs/tags/v1 \"$TEST_COMMIT\"\n\
           fi\n\
         done\n",
    );
    common::make_executable(&hook);
    trellis(root)
        .env("TEST_REMOTE", remote.path())
        .env("TEST_COMMIT", &release)
        .args(["tag", "create", "--push", "--force=adopt"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to push tag v1"));
    assert_eq!(
        git_stdout(remote.path(), &["rev-parse", "refs/tags/v1"]),
        release
    );
    let exact = git_stdout(root, &["rev-parse", "refs/tags/v1.0.0"]);
    fs::remove_file(hook).unwrap();
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    assert_eq!(git_stdout(root, &["rev-parse", "refs/tags/v1"]), release);
    assert_eq!(git_stdout(root, &["rev-parse", "refs/tags/v1.0.0"]), exact);
    assert_eq!(commit_of(root, "v1.0"), release);
}

#[test]
fn release_assets_require_an_unambiguous_package_and_existing_files_before_tagging() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    trellis_github(root, &api)
        .args([
            "tag",
            "create",
            "--github-release",
            "--asset",
            "missing.json",
        ])
        .assert()
        .failure();
    trellis_github(root, &api)
        .args([
            "tag",
            "create",
            "--github-release",
            "--package",
            "lat_core",
            "--draft",
            "--asset",
            "missing.json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing.json"));
    assert!(git_stdout(root, &["tag", "--list"]).is_empty());
}

#[test]
fn release_assets_stage_then_finalize_idempotently_at_the_qualified_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    write(&root.join("sbom.json"), "{\"bomFormat\":\"CycloneDX\"}");
    let args = ["tag", "create", "--github-release", "--package", "lat_core"];
    trellis_github(root, &api)
        .args(args)
        .args(["--draft", "--asset", "sbom.json"])
        .assert()
        .success();
    let release_file = root.join(".fake/release-lat_core-v1.2.0");
    let release = || -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(&release_file).unwrap()).unwrap()
    };
    assert_eq!(release()["draft"], true);
    assert_eq!(release()["prerelease"], false);
    assert!(release()["body"].as_str().unwrap().contains("- initial"));
    assert_eq!(release()["assets"][0]["name"], "sbom.json");
    assert_eq!(git_stdout(root, &["tag", "--list"]), "lat_core-v1.2.0");
    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("draft"));
    for _ in 0..2 {
        trellis_github(root, &api)
            .args(args)
            .arg("--finalize")
            .assert()
            .success();
    }
    assert_eq!(release()["draft"], false);
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        log.matches("POST /repos/example/repo/releases\n").count(),
        1
    );
    assert_eq!(log.matches("/assets?name=sbom.json\n").count(), 1);

    write(&root.join("later.txt"), "main advanced\n");
    git(root, &["add", "later.txt"]);
    git(root, &["commit", "-qm", "later"]);
    trellis_github(root, &api)
        .args(args)
        .arg("--finalize")
        .assert()
        .failure()
        .stderr(predicate::str::contains("qualified revision"));
    git(root, &["switch", "--detach", "HEAD~1"]);
    trellis_github(root, &api)
        .args(args)
        .arg("--finalize")
        .assert()
        .success();
}

#[test]
fn failed_asset_upload_cannot_finalize_an_incomplete_release_and_retry_recovers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    write(&root.join("one.json"), "one");
    write(&root.join("two.json"), "two");
    write(&root.join(".fake/fail-asset"), "two.json");
    let args = ["tag", "create", "--github-release", "--package", "lat_core"];
    trellis_github(root, &api)
        .args(args)
        .args(["--finalize", "--asset", "one.json", "--asset", "two.json"])
        .assert()
        .failure();
    fs::remove_file(root.join(".fake/fail-asset")).unwrap();
    trellis_github(root, &api)
        .args(args)
        .arg("--finalize")
        .assert()
        .failure()
        .stderr(predicate::str::contains("two.json"));
    write(&root.join(".fake/fail-finalize"), "");
    trellis_github(root, &api)
        .args(args)
        .args(["--finalize", "--asset", "one.json", "--asset", "two.json"])
        .assert()
        .failure();
    fs::remove_file(root.join(".fake/fail-finalize")).unwrap();
    trellis_github(root, &api)
        .args(args)
        .arg("--finalize")
        .assert()
        .success();
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(log.matches("/assets?name=one.json\n").count(), 1);
}

#[test]
fn conflicting_assets_need_explicit_replacement_and_failed_upload_preserves_original() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    write(&root.join("sbom.json"), "original");
    let args = [
        "tag",
        "create",
        "--github-release",
        "--package",
        "lat_core",
        "--draft",
        "--asset",
        "sbom.json",
    ];
    trellis_github(root, &api).args(args).assert().success();
    let release_file = root.join(".fake/release-lat_core-v1.2.0");
    let original: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&release_file).unwrap()).unwrap();
    write(&root.join("sbom.json"), "replacement");
    trellis_github(root, &api).args(args).assert().failure();
    write(&root.join(".fake/fail-asset"), "sbom.json");
    trellis_github(root, &api)
        .args(args)
        .arg("--replace-assets")
        .assert()
        .failure();
    let failed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&release_file).unwrap()).unwrap();
    assert_eq!(failed["assets"], original["assets"]);
    fs::remove_file(root.join(".fake/fail-asset")).unwrap();
    trellis_github(root, &api)
        .args(args)
        .arg("--replace-assets")
        .assert()
        .success();
    let replaced: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&release_file).unwrap()).unwrap();
    assert_eq!(replaced["assets"].as_array().unwrap().len(), 1);
    assert_eq!(replaced["assets"][0]["name"], "sbom.json");
    assert_ne!(
        replaced["assets"][0]["digest"],
        original["assets"][0]["digest"]
    );
}

#[test]
fn replacement_recovers_a_verified_upload_after_rename_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    write(&root.join("sbom.json"), "original");
    let args = [
        "tag",
        "create",
        "--github-release",
        "--package",
        "lat_core",
        "--draft",
        "--asset",
        "sbom.json",
        "--replace-assets",
    ];
    trellis_github(root, &api).args(args).assert().success();
    write(&root.join("sbom.json"), "replacement");
    write(&root.join(".fake/fail-rename"), "");
    trellis_github(root, &api).args(args).assert().failure();
    fs::remove_file(root.join(".fake/fail-rename")).unwrap();
    trellis_github(root, &api).args(args).assert().success();
    let release: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".fake/release-lat_core-v1.2.0")).unwrap(),
    )
    .unwrap();
    assert_eq!(release["assets"].as_array().unwrap().len(), 1);
    assert_eq!(release["assets"][0]["name"], "sbom.json");
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        log.matches("POST /repos/example/repo/releases/100/assets?")
            .count(),
        2
    );
}

#[test]
fn release_asset_dry_run_does_not_create_tags_or_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    write(&root.join("sbom.json"), "sbom");
    trellis_github(root, &api)
        .args([
            "tag",
            "create",
            "--github-release",
            "--package",
            "lat_core",
            "--draft",
            "--asset",
            "sbom.json",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("would"))
        .stdout(predicate::str::contains("staged GitHub").not());
    assert!(git_stdout(root, &["tag", "--list"]).is_empty());
    assert!(!root.join(".fake/release-lat_core-v1.2.0").exists());
}

#[test]
fn accepted_upload_with_failed_response_is_verified_on_retry_even_without_a_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    set_version(root, "lat_core", "1.3.0-rc.1");
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    write(&root.join("sbom.json"), "sbom");
    write(&root.join(".fake/fail-upload-response"), "");
    let args = [
        "tag",
        "create",
        "--github-release",
        "--package",
        "lat_core",
        "--finalize",
        "--asset",
        "sbom.json",
    ];
    trellis_github(root, &api).args(args).assert().failure();
    fs::remove_file(root.join(".fake/fail-upload-response")).unwrap();
    let file = root.join(".fake/release-lat_core-v1.3.0-rc.1");
    let mut release: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    release["assets"][0]
        .as_object_mut()
        .unwrap()
        .remove("digest");
    write(&file, &release.to_string());
    trellis_github(root, &api).args(args).assert().success();
    let release: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(release["assets"].as_array().unwrap().len(), 1);
    assert_eq!(release["draft"], false);
    assert_eq!(release["prerelease"], true);
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        log.matches("POST /repos/example/repo/releases/100/assets?")
            .count(),
        1
    );
    write(&root.join(".fake/asset-1"), "corrupted");
    trellis_github(root, &api)
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflicts"));
    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required asset"));
}

#[test]
fn scoped_release_rejects_a_conflicting_recorded_revision_and_ambiguous_exact_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);
    let args = [
        "tag",
        "create",
        "--github-release",
        "--package",
        "lat_core",
        "--draft",
    ];
    trellis_github(root, &api).args(args).assert().success();
    let file = root.join(".fake/release-lat_core-v1.2.0");
    let mut release: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    release["target_commitish"] = serde_json::json!("1111111111111111111111111111111111111111");
    write(&file, &release.to_string());
    trellis_github(root, &api)
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains("qualified revision"));

    let config_file = root.join("gleam.toml");
    let config = fs::read_to_string(&config_file).unwrap().replace(
        "exact_tag_format = \"{name}-v{version}\"",
        "exact_tag_format = \"v{version}\"",
    );
    write(&config_file, &config);
    set_version(root, "lat_mid", "1.2.0");
    git(root, &["add", "gleam.toml", "packages/lat_mid/gleam.toml"]);
    git(root, &["commit", "-qm", "ambiguous tag"]);
    trellis_github(root, &api)
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains("ambiguous"));
}

#[test]
fn tag_plan_lists_untagged_versions_and_create_tags_them() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    // lat_core 1.2.0 is already tagged; lat_mid and lat_cli are not.
    git(root, &["tag", "-a", "lat_core-v1.2.0", "-m", "existing"]);

    let output = trellis(root)
        .args(["tag", "plan", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "trellis.tag_plan/2");
    let plan = document["tags"].as_array().unwrap();
    let names: Vec<&str> = plan.iter().map(|p| p["name"].as_str().unwrap()).collect();
    // package_a is @release-excluded; lat_core already tagged.
    assert_eq!(names, vec!["lat_mid", "lat_cli"]);
    assert_eq!(plan[0]["tag"], "lat_mid-v0.5.0");

    trellis(root)
        .args(["tag", "create"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tagged lat_mid-v0.5.0"))
        .stdout(predicate::str::contains("tagged lat_cli-v0.3.1"));

    let tags = std::process::Command::new("git")
        .args(["tag", "--list"])
        .current_dir(root)
        .output()
        .unwrap();
    let tags = String::from_utf8_lossy(&tags.stdout);
    assert!(tags.contains("lat_mid-v0.5.0"));
    assert!(tags.contains("lat_cli-v0.3.1"));

    // Idempotent: nothing left to tag.
    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already tagged"));
}

#[test]
fn tag_create_github_release_uses_changelog_section() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    // A bare remote so the implied push has somewhere to go.
    let _remote = bare_origin(root);
    let api = common::mock_github(root);

    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pushed lat_core-v1.2.0"))
        .stdout(predicate::str::contains(
            "created GitHub release lat_core-v1.2.0",
        ));

    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert!(log.contains("POST /repos/example/repo/releases\n"), "{log}");
    assert!(log.contains(r#""tag_name": "lat_core-v1.2.0""#), "{log}");
    assert!(log.contains(r#""name": "lat_core-v1.2.0""#), "{log}");
    assert!(log.contains(r#""prerelease": false"#), "{log}");
    // The notes body is the CHANGELOG section for 1.2.0.
    assert!(log.contains("- initial"), "github log:\n{log}");
}

#[test]
fn github_release_classifies_candidates_from_semver_and_retries_existing_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = series_repo(
        root,
        "package_tags = [\"exact\", \"minor\"]\nexact_tag_format = \"release-{version}-{name}\"",
    );
    set_version(root, "lat_core", "1.3.0-rc.1");
    git(root, &["commit", "-qam", "candidate"]);
    let api = common::mock_github(root);
    // Publication may have succeeded and tag push failed before release creation.
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    let existing = commit_of(remote.path(), "release-1.3.0-rc.1-lat_core");
    for _ in 0..2 {
        trellis_github(root, &api)
            .args(["tag", "create", "--push", "--github-release"])
            .assert()
            .success();
    }
    assert_eq!(
        commit_of(remote.path(), "release-1.3.0-rc.1-lat_core"),
        existing
    );
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    let candidate = log
        .split("---")
        .find(|request| {
            request.contains("POST /repos/example/repo/releases\n")
                && request.contains(r#""tag_name": "release-1.3.0-rc.1-lat_core""#)
        })
        .unwrap();
    assert!(candidate.contains(r#""prerelease": true"#), "{candidate}");
    assert_eq!(
        log.matches("POST /repos/example/repo/releases\n").count(),
        3
    );
    assert!(!git_stdout(root, &["tag", "--list"]).contains("lat_core-v1.3\n"));
}

/// With no token in the environment, the client falls back to `gh auth
/// token` — the one job the gh CLI still has.
#[test]
fn github_release_token_falls_back_to_gh_auth_token() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    let api = common::mock_github(root);
    let gh = root.join("fake-gh.sh");
    write(
        &gh,
        "#!/bin/sh\nif [ \"$1 $2\" = 'auth token' ]; then echo fake-token; fi\n",
    );
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();

    trellis(root)
        .env("TRELLIS_GITHUB_API_URL", &api)
        .env("TRELLIS_GITHUB_REPO", "example/repo")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env("TRELLIS_GH_BIN", &gh)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "created GitHub release lat_core-v1.2.0",
        ));
}

#[test]
fn github_release_without_any_token_fails_with_guidance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);

    trellis(root)
        .env("TRELLIS_GITHUB_REPO", "example/repo")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env("TRELLIS_GH_BIN", "/nonexistent/gh")
        .args(["tag", "create", "--github-release"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("GITHUB_TOKEN"));
}

#[test]
fn tag_create_reconciles_local_tags_with_remote_and_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let remote = bare_origin(root);
    let api = common::mock_github(root);

    for (tag, message) in [
        ("lat_core-v1.2.0", "lat_core 1.2.0"),
        ("lat_mid-v0.5.0", "lat_mid 0.5.0"),
        ("lat_cli-v0.3.1", "lat_cli 0.3.1"),
    ] {
        git(root, &["tag", "-a", tag, "-m", message]);
    }

    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pushed lat_core-v1.2.0"))
        .stdout(predicate::str::contains(
            "created GitHub release lat_core-v1.2.0",
        ));

    let tags = git_stdout(remote.path(), &["tag", "--list"]);
    assert!(tags.contains("lat_core-v1.2.0"));
    assert!(tags.contains("lat_mid-v0.5.0"));
    assert!(tags.contains("lat_cli-v0.3.1"));

    let first_log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        first_log
            .matches("POST /repos/example/repo/releases\n")
            .count(),
        3
    );

    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success();
    let second_log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        second_log
            .matches("POST /repos/example/repo/releases\n")
            .count(),
        3,
        "the second run creates no new releases:\n{second_log}"
    );
}

#[test]
fn tag_create_rejects_divergent_local_and_remote_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);

    for (tag, message) in [
        ("lat_core-v1.2.0", "lat_core at initial commit"),
        ("lat_mid-v0.5.0", "lat_mid at initial commit"),
        ("lat_cli-v0.3.1", "lat_cli at initial commit"),
    ] {
        git(root, &["tag", "-a", tag, "-m", message]);
        git(root, &["push", "origin", tag]);
    }
    write(&root.join("later.txt"), "different commit\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "later"]);
    git(
        root,
        &[
            "tag",
            "-f",
            "-a",
            "lat_core-v1.2.0",
            "-m",
            "lat_core at later commit",
        ],
    );

    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("different objects"));
}
// ---- series tags -----------------------------------------------------------

/// The repository-series config the tag tests share: `lat_cli` anchoring
/// `repo-v{series}`.
const REPO_SERIES: &str = "repository_tag_package = \"lat_cli\"\n\
     repository_tag_format = \"repo-v{series}\"\nrepository_tags = [\"minor\"]";

/// A bare repository added to `root`'s repo as `origin`. The returned remote
/// must outlive the test.
#[test]
fn series_tag_moves_with_each_release_while_exact_tags_stay_put() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }",
    );
    let remote = remote.path();
    set_version(root, "lat_cli", "0.0.1");
    git(root, &["commit", "-qam", "lat_cli 0.0.1"]);

    // The first release of a series creates both tags at the same commit.
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tagged lat_cli-v0.0.1"))
        .stdout(predicate::str::contains("tagged lat_cli-v0.0"));
    let first = commit_of(root, "HEAD");
    assert_eq!(commit_of(root, "lat_cli-v0.0.1"), first);
    assert_eq!(commit_of(root, "lat_cli-v0.0"), first);
    // Other packages keep the default mode: exact tags only.
    let tags = git_stdout(root, &["tag", "--list"]);
    assert!(tags.contains("lat_core-v1.2.0"), "{tags}");
    assert!(!tags.contains("lat_core-v1\n"), "{tags}");

    set_version(root, "lat_cli", "0.0.2");
    git(root, &["commit", "-qam", "lat_cli 0.0.2"]);
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tagged lat_cli-v0.0.2"))
        .stdout(predicate::str::contains("moved lat_cli-v0.0"))
        .stdout(predicate::str::contains("force-pushed lat_cli-v0.0"));

    let second = commit_of(root, "HEAD");
    assert_ne!(first, second);
    assert_eq!(commit_of(root, "lat_cli-v0.0"), second, "series tag moved");
    assert_eq!(
        commit_of(root, "lat_cli-v0.0.1"),
        first,
        "exact tags are immutable"
    );
    assert_eq!(commit_of(root, "lat_cli-v0.0.2"), second);
    // Origin has the move too, not just the local repo.
    assert_eq!(commit_of(remote, "lat_cli-v0.0"), second);
    assert_eq!(commit_of(remote, "lat_cli-v0.0.1"), first);

    // Re-running moves nothing.
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved").not())
        .stdout(predicate::str::contains("force-pushed").not());
    assert_eq!(commit_of(root, "lat_cli-v0.0"), second);
    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already tagged"));
}

#[test]
fn tag_plan_reports_the_series_move() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }",
    );
    trellis(root).args(["tag", "create"]).assert().success();

    // Work that releases nothing moves nothing: the series tag follows its own
    // package's version, not HEAD.
    git(root, &["commit", "-q", "--allow-empty", "-m", "later work"]);
    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already tagged"));

    // A patch release inside the same series moves it.
    set_version(root, "lat_cli", "0.3.2");
    git(root, &["commit", "-qam", "lat_cli 0.3.2"]);
    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "lat_cli: 0.3.2 moves tag lat_cli-v0.3",
        ));

    let output = trellis(root)
        .args(["tag", "plan", "--json"])
        .output()
        .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let plan = document["tags"].as_array().unwrap();
    let series = plan
        .iter()
        .find(|tag| tag["tag"] == "lat_cli-v0.3")
        .unwrap_or_else(|| panic!("no series tag planned: {plan:?}"));
    assert_eq!(series["kind"], "series");
    assert_eq!(series["action"], "move");
}

/// One package's release must not drag every other package's series tags to
/// the release commit — the reason the signal is the manifest version at the
/// tag rather than the tag's position.
#[test]
fn a_release_moves_only_the_releasing_packages_series_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, "package_tags = [\"major\", \"minor\"]");
    trellis(root).args(["tag", "create"]).assert().success();
    let before = commit_of(root, "HEAD");

    set_version(root, "lat_cli", "0.4.0");
    git(root, &["commit", "-qam", "lat_cli 0.4.0"]);
    trellis(root)
        .args(["tag", "create"])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved lat_cli-v0"))
        .stdout(predicate::str::contains("tagged lat_cli-v0.4"))
        .stdout(predicate::str::contains("lat_core").not())
        .stdout(predicate::str::contains("lat_mid").not());

    // Untouched packages keep both levels on their own release commit.
    for tag in ["lat_core-v1", "lat_core-v1.2", "lat_mid-v0", "lat_mid-v0.5"] {
        assert_eq!(commit_of(root, tag), before, "{tag} was dragged forward");
    }
    assert_eq!(commit_of(root, "lat_cli-v0"), commit_of(root, "HEAD"));
}

#[test]
fn github_releases_skip_series_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }",
    );
    let api = common::mock_github(root);

    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "created GitHub release lat_cli-v0.3.1",
        ));

    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert!(log.contains(r#""tag_name": "lat_cli-v0.3.1""#), "{log}");
    // A release bound to a moving tag would silently retarget on the next move.
    assert!(!log.contains(r#""tag_name": "lat_cli-v0.3""#), "{log}");
    assert!(!log.contains("/releases/tags/lat_cli-v0.3\n"), "{log}");
    // The series tag itself is still created and pushed.
    assert_eq!(commit_of(root, "lat_cli-v0.3"), commit_of(root, "HEAD"));
}

#[test]
fn series_only_mode_creates_no_exact_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, "package_tags = [\"minor\"]");
    // A prerelease belongs to no series, so it moves nothing.
    set_version(root, "lat_mid", "0.6.0-rc.1");
    git(root, &["commit", "-qam", "lat_mid rc"]);

    trellis(root).args(["tag", "create"]).assert().success();
    // The default level is `minor`, so a 1.x package gets `v1.2`, not `v1`.
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec!["lat_cli-v0.3", "lat_core-v1.2"]
    );

    trellis(root)
        .args(["ci", "outputs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tags=[]\n"))
        .stdout(predicate::str::contains(
            "series_tags=[\"lat_core-v1.2\",\"lat_cli-v0.3\"]",
        ));
}

#[test]
fn the_major_level_gives_a_one_part_series_at_every_major() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, "package_tags = [\"major\"]");

    trellis(root).args(["tag", "create"]).assert().success();
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec!["lat_cli-v0", "lat_core-v1", "lat_mid-v0"]
    );
}

#[test]
fn listing_both_levels_moves_a_major_and_a_minor_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = series_repo(root, "package_tags = [\"major\", \"minor\"]");
    let remote = remote.path();

    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    let first = commit_of(root, "HEAD");
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec![
            "lat_cli-v0",
            "lat_cli-v0.3",
            "lat_core-v1",
            "lat_core-v1.2",
            "lat_mid-v0",
            "lat_mid-v0.5",
        ]
    );
    assert_eq!(commit_of(root, "lat_cli-v0"), first);

    // A minor bump starts a new minor tag and moves the major one — the whole
    // point of the coarser level.
    set_version(root, "lat_cli", "0.4.0");
    git(root, &["commit", "-qam", "lat_cli 0.4.0"]);
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tagged lat_cli-v0.4"))
        .stdout(predicate::str::contains("moved lat_cli-v0"))
        .stdout(predicate::str::contains("force-pushed lat_cli-v0"));

    let second = commit_of(root, "HEAD");
    assert_ne!(first, second);
    assert_eq!(commit_of(root, "lat_cli-v0"), second);
    assert_eq!(
        commit_of(remote, "lat_cli-v0"),
        second,
        "origin has the move"
    );
    assert_eq!(
        commit_of(root, "lat_cli-v0.3"),
        first,
        "the superseded minor series stays put"
    );

    // The bare major tag still resolves back to its package.
    trellis(root)
        .args(["ci", "tag-package", "lat_cli-v0"])
        .assert()
        .success()
        .stdout("lat_cli\n");
    trellis(root)
        .args(["ci", "outputs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_cli-v0\""));
}

#[test]
fn the_repository_tag_moves_every_level_it_declares() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // `repository_tags` is stated, not inherited from `package_tags` — the
    // repository tag tracks the anchor regardless of what packages publish.
    let remote = series_repo(
        root,
        "package_tags = [\"exact\"]\n\
         repository_tag_package = \"lat_cli\"\n\
         repository_tag_format = \"repo-v{series}\"\n\
         repository_tags = [\"major\", \"minor\"]",
    );
    let remote = remote.path();

    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    let first = commit_of(root, "HEAD");
    assert_eq!(commit_of(root, "repo-v0.3"), first);
    assert_eq!(commit_of(root, "repo-v0"), first);

    set_version(root, "lat_cli", "0.4.0");
    git(root, &["commit", "-qam", "anchor 0.4.0"]);
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    let second = commit_of(root, "HEAD");
    assert_eq!(commit_of(root, "repo-v0.4"), second);
    assert_eq!(commit_of(root, "repo-v0"), second);
    assert_eq!(commit_of(remote, "repo-v0"), second);
    assert_eq!(commit_of(root, "repo-v0.3"), first);
}

#[test]
fn default_config_creates_no_series_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);

    trellis(root).args(["tag", "create"]).assert().success();
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec!["lat_cli-v0.3.1", "lat_core-v1.2.0", "lat_mid-v0.5.0"]
    );
    trellis(root)
        .args(["ci", "outputs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("series_tags=[]"));
}

#[test]
fn a_series_tag_names_a_package_but_never_a_release() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }",
    );

    // CI can still route on it…
    trellis(root)
        .args(["ci", "tag-package", "lat_cli-v0.3"])
        .assert()
        .success()
        .stdout("lat_cli\n");
    let output = trellis(root)
        .args(["ci", "tag-package", "lat_cli-v0.3", "--json"])
        .output()
        .unwrap();
    let resolved: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(resolved["name"], "lat_cli");
    assert_eq!(resolved["tag_kind"], "series");
    assert_eq!(resolved["tag_series"], "0.3");
    assert!(resolved.get("tag_version").is_none());

    // …but it names no version, so it cannot trigger a publish.
    trellis(root)
        .args(["publish", "--tag", "lat_cli-v0.3"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("moving v0.3 series tag"))
        .stderr(predicate::str::contains("--all-untagged"));

    // The exact tag still resolves as before.
    let output = trellis(root)
        .args(["ci", "tag-package", "lat_cli-v0.3.1", "--json"])
        .output()
        .unwrap();
    let resolved: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(resolved["tag_kind"], "exact");
    assert_eq!(resolved["tag_version"], "0.3.1");
}

#[test]
fn a_name_less_series_tag_format_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags = [\"exact\", \"minor\"]\nseries_tag_format = \"v{series}\"",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "`series_tag_format` `v{series}` has no {name} placeholder",
        ));
}

#[test]
fn repository_series_moves_only_when_the_anchor_manifest_version_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, &format!("package_tags = [\"exact\"]\n{REPO_SERIES}"));

    trellis(root)
        .args(["tag", "create"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tagged repo-v0.3"));
    let first = commit_of(root, "repo-v0.3");

    set_version(root, "lat_mid", "0.5.1");
    git(root, &["commit", "-qam", "release only lat_mid"]);
    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("repo-v0.3").not());
    trellis(root).args(["tag", "create"]).assert().success();
    assert_eq!(commit_of(root, "repo-v0.3"), first);

    set_version(root, "lat_cli", "0.3.2");
    git(root, &["commit", "-qam", "release anchor"]);
    let output = trellis(root)
        .args(["tag", "plan", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let repository = document["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tag| tag["tag"] == "repo-v0.3")
        .unwrap();
    assert_eq!(repository["name"], "lat_cli");
    assert_eq!(repository["version"], "0.3.2");
    assert_eq!(repository["kind"], "repository_series");
    assert_eq!(repository["action"], "move");
}

#[test]
fn repository_series_transition_keeps_the_old_tag_and_skips_prereleases() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, REPO_SERIES);
    trellis(root).args(["tag", "create"]).assert().success();
    let old = commit_of(root, "repo-v0.3");

    set_version(root, "lat_cli", "0.4.0-rc.1");
    git(root, &["commit", "-qam", "anchor prerelease"]);
    trellis(root).args(["tag", "create"]).assert().success();
    assert!(!git_stdout(root, &["tag", "--list"]).contains("repo-v0.4"));

    set_version(root, "lat_cli", "0.4.0");
    git(root, &["commit", "-qam", "anchor stable"]);
    trellis(root).args(["tag", "create"]).assert().success();
    assert_eq!(commit_of(root, "repo-v0.3"), old);
    assert_eq!(commit_of(root, "repo-v0.4"), commit_of(root, "HEAD"));
}

#[test]
fn repository_series_uses_committed_anchor_version_not_worktree_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, REPO_SERIES);
    trellis(root).args(["tag", "create"]).assert().success();
    let tagged = commit_of(root, "repo-v0.3");

    set_version(root, "lat_cli", "0.4.0");
    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("repo-v0.4").not());
    assert_eq!(commit_of(root, "repo-v0.3"), tagged);
}

#[test]
fn repository_series_fetches_a_remote_only_tag_before_planning_a_push() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let remote = series_repo(root, REPO_SERIES);
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    let original = git_stdout(root, &["ls-remote", "origin", "refs/tags/repo-v0.3^{}"]);
    git(root, &["tag", "-d", "repo-v0.3"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "unrelated"]);

    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("force-pushed repo-v0.3").not());
    assert!(
        original.starts_with(&commit_of(root, "repo-v0.3")),
        "{original}"
    );
    assert_eq!(
        git_stdout(root, &["ls-remote", "origin", "refs/tags/repo-v0.3^{}"]),
        original
    );
    drop(remote);
}

#[test]
fn repository_series_refuses_to_move_a_newer_remote_tag_backward() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, REPO_SERIES);
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    let old_commit = commit_of(root, "HEAD");

    set_version(root, "lat_cli", "0.3.2");
    git(root, &["commit", "-qam", "anchor 0.3.2"]);
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .success();
    let remote_new = git_stdout(root, &["ls-remote", "origin", "refs/tags/repo-v0.3^{}"]);

    git(root, &["checkout", "-q", &old_commit]);
    git(root, &["tag", "-f", "repo-v0.3", &old_commit]);
    trellis(root)
        .args(["tag", "create", "--push"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "origin is anchored at newer lat_cli version `0.3.2`",
        ))
        .stderr(predicate::str::contains(
            "refusing to move it backward to `0.3.1`",
        ));
    assert_eq!(
        git_stdout(root, &["ls-remote", "origin", "refs/tags/repo-v0.3^{}"]),
        remote_new
    );
}

#[test]
fn repository_series_is_independent_of_tag_mode_and_never_resolves_as_a_package_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags = [\"exact\"]\n\
         repository_tag_package = \"lat_core\"\n\
         repository_tag_format = \"repo-v{series}\"\nrepository_tags = [\"minor\"]",
    );
    trellis(root).args(["tag", "create"]).assert().success();
    assert!(git_stdout(root, &["tag", "--list"]).contains("repo-v1"));

    trellis(root)
        .args(["ci", "tag-package", "repo-v1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("repository tag"));
    trellis(root)
        .args(["publish", "--tag", "repo-v1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("repository tag"));
}

#[test]
fn repository_series_anchor_and_namespace_are_validated_by_doctor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "repository_tag_package = \"missing\"\n\
         repository_tag_format = \"repo-v{series}\"\nrepository_tags = [\"minor\"]",
    );
    trellis(root)
        .args(["doctor"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "`repository_tag_package` `missing` is not a workspace member",
        ));

    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\n\
         exclude = { \"@release\" = [\"examples/*\"] }\n\
         [tools.trellis.publish]\n\
         repository_tag_package = \"package_a\"\n\
         repository_tag_format = \"repo-v{series}\"\n\
         repository_tags = [\"minor\"]\n",
    );
    trellis(root)
        .args(["doctor"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "`repository_tag_package` `package_a` is excluded from release",
        ));

    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\n\
         exclude = { \"@release\" = [\"examples/*\"] }\n\
         [tools.trellis.publish]\nexact_tag_format = \"repo-v{version}\"\n\
         package_tags = [\"exact\", \"major\"]\n\
         repository_tag_package = \"lat_core\"\n\
         repository_tag_format = \"repo-v{series}.0.0\"\n\
         repository_tags = [\"major\"]\n",
    );
    set_version(root, "lat_core", "1.0.0");
    trellis(root)
        .args(["doctor"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("tag collision"))
        .stdout(predicate::str::contains("repo-v1.0.0"));
    trellis(root)
        .args(["tag", "create"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "collides with a package tag namespace",
        ));
}

#[test]
fn repository_series_is_force_pushed_and_gets_no_github_release() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, REPO_SERIES);
    let api = common::mock_github(root);
    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pushed repo-v0.3"));

    set_version(root, "lat_cli", "0.3.2");
    git(root, &["commit", "-qam", "anchor release"]);
    trellis_github(root, &api)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("force-pushed repo-v0.3"));
    let remote_peeled = git_stdout(root, &["ls-remote", "origin", "refs/tags/repo-v0.3^{}"]);
    assert!(
        remote_peeled.starts_with(&commit_of(root, "HEAD")),
        "{remote_peeled}"
    );
    // A repository tag is a moving tag, so it is never checked for or
    // given a GitHub release, which would silently retarget on the next move.
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert!(!log.contains(r#""tag_name": "repo-v0.3""#), "{log}");
    assert!(!log.contains("/releases/tags/repo-v0.3"), "{log}");
}

#[test]
fn repository_series_reports_an_unreadable_historical_anchor_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, REPO_SERIES);
    git(root, &["checkout", "-q", "-b", "missing-anchor"]);
    git(root, &["rm", "-q", "packages/lat_cli/gleam.toml"]);
    git(root, &["commit", "-qm", "remove anchor manifest"]);
    git(root, &["tag", "-a", "repo-v0.3", "-m", "broken history"]);
    git(root, &["checkout", "-q", "main"]);

    trellis(root)
        .args(["tag", "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot read repository tag anchor manifest \
             `packages/lat_cli/gleam.toml` at revision `repo-v0.3`",
        ));
}

#[test]
fn a_named_series_format_stays_unambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(root, "package_tags = [\"exact\", \"minor\"]");
    // Two packages in the same series: with `{name}` in the format their tags
    // stay distinct, so there is nothing to warn about and each still resolves.
    set_version(root, "lat_cli", "0.5.2");
    git(root, &["commit", "-qam", "lat_cli 0.5.2"]);

    trellis(root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("series tag").not());
    trellis(root)
        .args(["ci", "tag-package", "lat_cli-v0.5"])
        .assert()
        .success()
        .stdout("lat_cli\n");
    trellis(root)
        .args(["ci", "tag-package", "lat_mid-v0.5"])
        .assert()
        .success()
        .stdout("lat_mid\n");
}

#[test]
fn doctor_rejects_a_package_tags_override_that_matches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/nope\" = [\"minor\"] }",
    );

    trellis(root)
        .args(["doctor"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "`package_tags_overrides` glob `packages/nope` matches no member",
        ));
}
