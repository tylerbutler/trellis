//! End-to-end tests for the tag/publish layer, using a fake gleam binary
//! (TRELLIS_GLEAM_BIN), a mock GitHub API (TRELLIS_GITHUB_API_URL), a mock
//! Hex API served from a local thread (TRELLIS_HEX_API_URL), and real git
//! repos.

// ponytail: changelog/gleam-log assertions use contains(); convert to insta::assert_snapshot! when next touched

mod common;

use common::*;

use predicates::prelude::*;
use std::fs;
use std::io::{Read, Write as IoWrite};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A fake gleam that logs every invocation (cwd + args) to `.fake/gleam-log`
/// and snapshots gleam.toml at publish time so tests can observe the rewrite.
///
/// It also records whether `build/packages` existed at each invocation, in
/// `.fake/build-state` — a separate file so tests asserting on the exact
/// contents of `gleam-log` are unaffected.
fn install_fake_gleam(root: &Path) -> PathBuf {
    let script = root.join("fake-gleam.sh");
    write(
        &script,
        &format!(
            concat!(
                "#!/bin/sh\n",
                "set -eu\n",
                "root=\"{root}\"\n",
                "echo \"$(basename \"$PWD\") gleam $*\" >> \"$root/.fake/gleam-log\"\n",
                "if [ -d build/packages ]; then state=present; else state=absent; fi\n",
                "echo \"$(basename \"$PWD\") $1 $state\" >> \"$root/.fake/build-state\"\n",
                "if [ \"$1\" = publish ]; then\n",
                "  cp gleam.toml \"$root/.fake/published-$(basename \"$PWD\").toml\"\n",
                "fi\n",
            ),
            root = root.display()
        ),
    );
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(root.join(".fake")).unwrap();
    script
}

/// Serve a canned Hex API from a background thread: `versions` maps package
/// name → published versions; unknown packages get a 404, like Hex.
fn mock_hex(versions: Vec<(&'static str, Vec<&'static str>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/api", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let mut request = Vec::new();
            // Read until end of headers (requests have no body).
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let request = String::from_utf8_lossy(&request);
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let package = path.rsplit('/').next().unwrap_or("");
            let response = match versions.iter().find(|(name, _)| *name == package) {
                Some((_, released)) => {
                    let releases: Vec<String> = released
                        .iter()
                        .map(|v| format!("{{\"version\":\"{v}\"}}"))
                        .collect();
                    let body = format!("{{\"releases\":[{}]}}", releases.join(","));
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    base
}

// ---- tag ----------------------------------------------------------------

#[test]
fn tag_plan_lists_untagged_versions_and_create_tags_them() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    // lat_core 1.2.0 is already tagged; lat_mid and lat_cli are not.
    git(root, &["tag", "-a", "lat_core-v1.2.0", "-m", "existing"]);

    let document = json_output(root, &["tag", "plan", "--json"], true);
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
    // The notes body is the CHANGELOG section for 1.2.0.
    assert!(log.contains("- initial"), "github log:\n{log}");
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

// ---- ci tag-package -------------------------------------------------------

#[test]
fn ci_tag_package_resolves_tag_to_package() {
    trellis(&fixture("basic"))
        .args(["ci", "tag-package", "lat_core-v1.2.0"])
        .assert()
        .success()
        .stdout("lat_core\n");

    let info = json_output(
        &fixture("basic"),
        &["ci", "tag-package", "lat_mid-v9.9.9", "--json"],
        true,
    );
    assert_eq!(info["name"], "lat_mid");
    assert_eq!(info["version"], "0.5.0");
    assert_eq!(info["tag_version"], "9.9.9");

    trellis(&fixture("basic"))
        .args(["ci", "tag-package", "unrelated-v1.0.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not match any releasable package",
        ));
}

// ---- publish --------------------------------------------------------------

#[test]
fn publish_rewrites_path_deps_and_restores_the_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);
    let hex = mock_hex(vec![("lat_core", vec!["1.2.0"])]); // lat_mid: 404

    let original = fs::read_to_string(root.join("packages/lat_mid/gleam.toml")).unwrap();
    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "lat_mid"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "rewrote lat_core -> \">= 1.2.0 and < 2.0.0\"",
        ))
        .stdout(predicate::str::contains("[lat_mid] published 0.5.0"));

    // Validation and publish ran, in order, in the package directory.
    let log = fs::read_to_string(root.join(".fake/gleam-log")).unwrap();
    assert_eq!(
        log,
        concat!(
            "lat_mid gleam format --check\n",
            "lat_mid gleam build --warnings-as-errors\n",
            "lat_mid gleam test\n",
            "lat_mid gleam publish --yes\n",
        )
    );
    // gleam publish saw the rewritten manifest…
    let published = fs::read_to_string(root.join(".fake/published-lat_mid.toml")).unwrap();
    assert!(published.contains("lat_core = \">= 1.2.0 and < 2.0.0\""));
    assert!(!published.contains("path"));
    // …but the repo shows the original afterwards.
    assert_eq!(
        fs::read_to_string(root.join("packages/lat_mid/gleam.toml")).unwrap(),
        original
    );
}

#[test]
fn publish_clears_the_stale_dependency_tree_before_publishing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);
    let hex = mock_hex(vec![("lat_core", vec!["1.2.0"])]); // lat_mid: 404

    // Stand in for what validation leaves behind: a dependency tree gleam
    // resolved while lat_core was still a path dep, alongside compiled output.
    let packages = root.join("packages/lat_mid/build/packages");
    fs::create_dir_all(&packages).unwrap();
    write(&packages.join("lat_core.config_fingerprint"), "stale");
    let compiled = root.join("packages/lat_mid/build/dev/erlang/lat_mid");
    fs::create_dir_all(&compiled).unwrap();
    write(&compiled.join("lat_mid.app"), "compiled");

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "lat_mid"])
        .assert()
        .success();

    // Validation runs against the tree as resolved; publish must not, or gleam
    // fails reading the local dependency it just dropped from the manifest.
    let state = fs::read_to_string(root.join(".fake/build-state")).unwrap();
    assert_eq!(
        state,
        concat!(
            "lat_mid format present\n",
            "lat_mid build present\n",
            "lat_mid test present\n",
            "lat_mid publish absent\n",
        )
    );
    // Only the resolved-dependency half is stale; compiled output survives.
    assert!(
        compiled.join("lat_mid.app").exists(),
        "clearing the dependency tree must not throw away compiled artifacts"
    );
}

#[test]
fn publish_keeps_the_dependency_tree_when_nothing_is_rewritten() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);
    let hex = mock_hex(vec![]); // nothing published yet

    // lat_core has no path deps, so its manifest is published as-is and its
    // dependency tree stays valid throughout — no reason to pay for a refetch.
    let packages = root.join("packages/lat_core/build/packages");
    fs::create_dir_all(&packages).unwrap();
    write(&packages.join("gleam.lock"), "");

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "lat_core"])
        .assert()
        .success();

    let state = fs::read_to_string(root.join(".fake/build-state")).unwrap();
    assert!(
        state.contains("lat_core publish present"),
        "unrewritten package should keep its dependency tree:\n{state}"
    );
    assert!(packages.join("gleam.lock").exists());
}

#[test]
fn publish_skips_versions_already_on_hex() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);
    let hex = mock_hex(vec![("lat_core", vec!["1.1.0", "1.2.0"])]);

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "lat_core"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1.2.0 is already on Hex; skipping",
        ));
    assert!(
        !root.join(".fake/gleam-log").exists(),
        "no gleam command should run"
    );
}

#[test]
fn publish_all_untagged_goes_in_topological_order_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);
    // lat_core already published at the current version; the others aren't.
    let hex = mock_hex(vec![("lat_core", vec!["1.2.0"])]);

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "--all-untagged"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[lat_core] 1.2.0 is already on Hex",
        ))
        .stdout(predicate::str::contains("[lat_mid] published 0.5.0"))
        .stdout(predicate::str::contains("[lat_cli] published 0.3.1"));

    let log = fs::read_to_string(root.join(".fake/gleam-log")).unwrap();
    let mid = log.find("lat_mid gleam publish").unwrap();
    let cli = log.find("lat_cli gleam publish").unwrap();
    assert!(
        mid < cli,
        "dependency must publish before dependent:\n{log}"
    );
    assert!(
        !log.contains("package_a"),
        "@release-excluded members never publish"
    );
}

#[test]
fn publish_dry_run_reports_without_running_gleam() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);
    let hex = mock_hex(vec![]);

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "lat_mid", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[lat_mid] would publish 0.5.0"))
        .stdout(predicate::str::contains(
            "lat_core -> \">= 1.2.0 and < 2.0.0\"",
        ));
    assert!(!root.join(".fake/gleam-log").exists());
}

#[test]
fn publish_by_tag_refuses_version_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);
    let hex = mock_hex(vec![]);

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "--tag", "lat_core-v9.9.9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to publish"));

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "--tag", "lat_core-v1.2.0"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[lat_core] published 1.2.0"));
}

#[test]
fn publish_restores_manifest_even_when_publish_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    // gleam fails only on `publish`, after validation passes.
    let gleam = root.join("fake-gleam.sh");
    write(
        &gleam,
        "#!/bin/sh\nif [ \"$1\" = publish ]; then echo 'rate limited' >&2; exit 1; fi\n",
    );
    fs::set_permissions(&gleam, fs::Permissions::from_mode(0o755)).unwrap();
    let hex = mock_hex(vec![]);

    let original = fs::read_to_string(root.join("packages/lat_mid/gleam.toml")).unwrap();
    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .env("TRELLIS_HEX_API_URL", &hex)
        .args(["publish", "lat_mid"])
        .assert()
        .failure();
    assert_eq!(
        fs::read_to_string(root.join("packages/lat_mid/gleam.toml")).unwrap(),
        original,
        "gleam.toml must be restored after a failed publish"
    );
}

// ---- series tags -----------------------------------------------------------

/// The repository-series config the tag tests share: `lat_cli` anchoring
/// `repo-v{series}`.
const REPO_SERIES: &str = "repository_tag_package = \"lat_cli\"\n\
     repository_tag_format = \"repo-v{series}\"\nrepository_tags = [\"minor\"]";

/// A bare repository added to `root`'s repo as `origin`. The returned remote
/// must outlive the test.
fn bare_origin(root: &Path) -> tempfile::TempDir {
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    remote
}

/// The basic fixture with `[tools.trellis.publish]` replaced by `publish`, in
/// a git repo with a bare origin. The returned remote must outlive the test.
fn series_repo(root: &Path, publish: &str) -> tempfile::TempDir {
    copy_fixture_to(root);
    write(
        &root.join("gleam.toml"),
        &format!(
            "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\n\
             exclude = {{ \"@release\" = [\"examples/*\"] }}\n\n\
             [tools.trellis.publish]\n{publish}\n"
        ),
    );
    init_repo(root);
    bare_origin(root)
}

fn commit_of(dir: &Path, revision: &str) -> String {
    git_stdout(dir, &["rev-parse", &format!("{revision}^{{commit}}")])
}

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
fn info_reports_the_tags_a_package_actually_gets() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }",
    );

    trellis(root)
        .args(["info", "lat_cli"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tag:        lat_cli-v0.3.1"))
        .stdout(predicate::str::contains("series tag: lat_cli-v0.3"));
    trellis(root)
        .args(["info", "lat_core"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tag:        lat_core-v1.2.0"))
        .stdout(predicate::str::contains("series tag:").not());
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
fn doctor_warns_about_a_repo_wide_series_tag_whatever_the_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags = [\"exact\", \"minor\"]\nseries_tag_format = \"v{series}\"",
    );

    // lat_core 1.2.0, lat_mid 0.5.0 and lat_cli 0.3.1 are three distinct
    // series, so no two members render the same tag — and it is ambiguous
    // anyway, because a `{name}`-less format matches every member.
    trellis(root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "`series_tag_format` `v{series}` has no {name}, so one repository-wide series tag \
             covers `lat_core`, `lat_mid`, `lat_cli`",
        ));
    trellis(root)
        .args(["ci", "tag-package", "v0.3"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("names no single package"));

    // Colliding versions are the same warning, not a new one.
    set_version(root, "lat_cli", "0.5.2");
    git(root, &["commit", "-qam", "lat_cli 0.5.2"]);
    trellis(root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("has no {name}"));
}

/// The format is deprecated for its shape, so one series-mode package — where
/// nothing is ambiguous yet — is warned too. Adding a second series package is
/// a one-line change that would otherwise silently break `ci tag-package`.
#[test]
fn doctor_deprecates_a_name_less_series_tag_format_even_when_unambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "series_tag_format = \"v{series}\"\n\
         package_tags_overrides = { \"packages/lat_cli\" = [\"minor\"] }",
    );

    // The named-format case — no warning at all — is `a_named_series_format
    // _stays_unambiguous`.
    trellis(root)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "is deprecated and will be removed at 1.0",
        ))
        .stdout(predicate::str::contains("repository_series"))
        // Only one series-mode package, so no ambiguity clause.
        .stdout(predicate::str::contains("cannot resolve it to one package").not());
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
    let document = json_output(root, &["tag", "plan", "--json"], true);
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

// ---- release bootstrap -----------------------------------------------------

#[test]
fn bootstrap_uses_current_versions_with_no_fragments_required() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    // The fixture ships no `.changes/unreleased` directory at all — bootstrap
    // must not need one.
    assert!(!root.join(".changes").exists());

    trellis(root)
        .args(["release", "bootstrap"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tagged lat_core-v1.2.0"))
        .stdout(predicate::str::contains("tagged lat_mid-v0.5.0"))
        .stdout(predicate::str::contains("tagged lat_cli-v0.3.1"));

    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec!["lat_cli-v0.3.1", "lat_core-v1.2.0", "lat_mid-v0.5.0"]
    );
    // No version bump: bootstrap never runs `version plan`/`version apply`.
    assert_eq!(version_of(root, "lat_core"), "1.2.0");
    assert_eq!(version_of(root, "lat_mid"), "0.5.0");
    assert_eq!(version_of(root, "lat_cli"), "0.3.1");
}

#[test]
fn bootstrap_dry_run_reports_every_action_and_mutates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let remote = bare_origin(root);
    let api = common::mock_github(root);

    trellis_github(root, &api)
        .args([
            "release",
            "bootstrap",
            "--dry-run",
            "--push",
            "--github-release",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("would tag lat_core-v1.2.0"))
        .stdout(predicate::str::contains("would push lat_core-v1.2.0"))
        .stdout(predicate::str::contains(
            "would create GitHub release lat_core-v1.2.0",
        ))
        .stdout(predicate::str::contains("would tag lat_mid-v0.5.0"))
        .stdout(predicate::str::contains(
            "would create GitHub release lat_cli-v0.3.1",
        ));

    // Nothing was actually created, locally, on origin, or via the API.
    assert_eq!(git_stdout(root, &["tag", "--list"]), "");
    assert_eq!(git_stdout(remote.path(), &["tag", "--list"]), "");
    let log = fs::read_to_string(root.join(".fake/github-log")).unwrap_or_default();
    assert!(
        !log.contains("POST /repos/example/repo/releases\n"),
        "{log}"
    );
}

#[test]
fn bootstrap_creates_both_exact_and_series_tags() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let _remote = series_repo(
        root,
        "package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }",
    );

    trellis(root)
        .args(["release", "bootstrap", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would tag lat_cli-v0.3.1"))
        .stdout(predicate::str::contains("would tag lat_cli-v0.3\n"));

    trellis(root)
        .args(["release", "bootstrap"])
        .assert()
        .success();
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(
        tags.lines().collect::<Vec<_>>(),
        vec![
            "lat_cli-v0.3",
            "lat_cli-v0.3.1",
            "lat_core-v1.2.0",
            "lat_mid-v0.5.0"
        ]
    );
}

#[test]
fn bootstrap_leaves_release_excluded_packages_untagged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);

    trellis(root)
        .args(["release", "bootstrap", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("package_a").not());

    trellis(root)
        .args(["release", "bootstrap"])
        .assert()
        .success();
    let tags = git_stdout(root, &["tag", "--list"]);
    assert!(!tags.contains("package_a"), "{tags}");
}

#[test]
fn bootstrap_fetches_a_remote_only_tag_instead_of_recreating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    // lat_core is already tagged and pushed by some earlier process; the
    // local clone bootstrapping now has never seen the tag.
    git(root, &["tag", "-a", "lat_core-v1.2.0", "-m", "existing"]);
    git(root, &["push", "origin", "lat_core-v1.2.0"]);
    let original = commit_of(root, "lat_core-v1.2.0");
    git(root, &["tag", "-d", "lat_core-v1.2.0"]);

    trellis(root)
        .args(["release", "bootstrap", "--dry-run", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("would fetch lat_core-v1.2.0"));

    trellis(root)
        .args(["release", "bootstrap", "--push"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fetched lat_core-v1.2.0"));
    assert_eq!(commit_of(root, "lat_core-v1.2.0"), original);
}

#[test]
fn bootstrap_reports_existing_releases_and_reruns_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    let api = common::mock_github(root);

    trellis_github(root, &api)
        .args(["release", "bootstrap", "--push", "--github-release"])
        .assert()
        .success();
    let first_log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        first_log
            .matches("POST /repos/example/repo/releases\n")
            .count(),
        3
    );

    trellis_github(root, &api)
        .args([
            "release",
            "bootstrap",
            "--dry-run",
            "--push",
            "--github-release",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "GitHub release lat_core-v1.2.0 already exists; skipping",
        ))
        .stdout(predicate::str::contains("would").not());

    trellis_github(root, &api)
        .args(["release", "bootstrap", "--push", "--github-release"])
        .assert()
        .success();
    let second_log = fs::read_to_string(root.join(".fake/github-log")).unwrap();
    assert_eq!(
        second_log
            .matches("POST /repos/example/repo/releases\n")
            .count(),
        3
    );
}

#[test]
fn bootstrap_preflights_conflicts_before_mutating_any_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    let _remote = bare_origin(root);
    // lat_core is tagged and pushed, then the local tag is force-moved to a
    // later commit — an immutable tag disagreeing with origin.
    git(root, &["tag", "-a", "lat_core-v1.2.0", "-m", "first"]);
    git(root, &["push", "origin", "lat_core-v1.2.0"]);
    write(&root.join("later.txt"), "later\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "later"]);
    git(root, &["tag", "-f", "-a", "lat_core-v1.2.0", "-m", "moved"]);
    // lat_mid and lat_cli have no tag yet, and would otherwise be created.

    trellis(root)
        .args(["release", "bootstrap", "--dry-run", "--push"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("different objects"));

    trellis(root)
        .args(["release", "bootstrap", "--push"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("different objects"));

    // Neither the dry-run nor the failed real run tagged anything else —
    // the conflict on lat_core blocks the whole batch.
    let tags = git_stdout(root, &["tag", "--list"]);
    assert_eq!(tags.lines().collect::<Vec<_>>(), vec!["lat_core-v1.2.0"]);
}

// ---- lockfile refresh ------------------------------------------------------

#[test]
fn lockfile_refresh_scopes_to_one_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let gleam = install_fake_gleam(root);

    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .args(["lockfile", "refresh", "--package", "lat_mid"])
        .assert()
        .success();
    let log = fs::read_to_string(root.join(".fake/gleam-log")).unwrap();
    assert_eq!(log, "lat_mid gleam deps download\n");
}
