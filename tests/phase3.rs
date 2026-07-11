//! End-to-end tests for the tag/publish layer, using a fake gleam binary
//! (TRELLIS_GLEAM_BIN), a fake gh (TRELLIS_GH_BIN), a mock Hex API served
//! from a local thread (TRELLIS_HEX_API_URL), and real git repos.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::io::{Read, Write as IoWrite};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The mock Hex server binds localhost, so the agent proxy configured in
/// some environments must not intercept requests.
fn trellis(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trellis").unwrap();
    cmd.current_dir(dir);
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        cmd.env_remove(var);
    }
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

fn git(root: &Path, args: &[&str]) {
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
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
}

/// A fake gleam that logs every invocation (cwd + args) to `.fake/gleam-log`
/// and snapshots gleam.toml at publish time so tests can observe the rewrite.
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

/// A fake gh that logs `release create` calls, notes included.
fn install_fake_gh(root: &Path) -> PathBuf {
    let script = root.join("fake-gh.sh");
    write(
        &script,
        &format!(
            concat!(
                "#!/bin/sh\n",
                "set -eu\n",
                "printf 'gh %s\\n---\\n' \"$*\" >> \"{root}/.fake/gh-log\"\n",
                "case \"$1 $2\" in\n",
                "  'release view')\n",
                "    if [ -f \"{root}/.fake/release-$3\" ]; then\n",
                "      printf '{{\"tagName\":\"%s\"}}\\n' \"$3\"\n",
                "    else\n",
                "      echo 'release not found' >&2\n",
                "      exit 1\n",
                "    fi\n",
                "    ;;\n",
                "  'release create') touch \"{root}/.fake/release-$3\" ;;\n",
                "esac\n",
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

    let output = trellis(root)
        .args(["tag", "plan", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let plan = plan.as_array().unwrap();
    let names: Vec<&str> = plan.iter().map(|p| p["name"].as_str().unwrap()).collect();
    // examples is ignore-release; lat_core already tagged.
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
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    let gh = install_fake_gh(root);

    trellis(root)
        .env("TRELLIS_GH_BIN", &gh)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pushed lat_core-v1.2.0"))
        .stdout(predicate::str::contains(
            "created GitHub release lat_core-v1.2.0",
        ));

    let log = fs::read_to_string(root.join(".fake/gh-log")).unwrap();
    assert!(log.contains("release create lat_core-v1.2.0 --title lat_core-v1.2.0 --notes"));
    // The notes body is the CHANGELOG section for 1.2.0.
    assert!(log.contains("- initial"), "gh log:\n{log}");
}

#[test]
fn tag_create_reconciles_local_tags_with_remote_and_releases() {
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
    let gh = install_fake_gh(root);

    for (tag, message) in [
        ("lat_core-v1.2.0", "lat_core 1.2.0"),
        ("lat_mid-v0.5.0", "lat_mid 0.5.0"),
        ("lat_cli-v0.3.1", "lat_cli 0.3.1"),
    ] {
        git(root, &["tag", "-a", tag, "-m", message]);
    }

    trellis(root)
        .env("TRELLIS_GH_BIN", &gh)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pushed lat_core-v1.2.0"))
        .stdout(predicate::str::contains(
            "created GitHub release lat_core-v1.2.0",
        ));

    let tags = std::process::Command::new("git")
        .args([
            "--git-dir",
            remote.path().to_str().unwrap(),
            "tag",
            "--list",
        ])
        .output()
        .unwrap();
    let tags = String::from_utf8_lossy(&tags.stdout);
    assert!(tags.contains("lat_core-v1.2.0"));
    assert!(tags.contains("lat_mid-v0.5.0"));
    assert!(tags.contains("lat_cli-v0.3.1"));

    let first_log = fs::read_to_string(root.join(".fake/gh-log")).unwrap();
    assert_eq!(first_log.matches("gh release create").count(), 3);

    trellis(root)
        .env("TRELLIS_GH_BIN", &gh)
        .args(["tag", "create", "--github-release"])
        .assert()
        .success();
    let second_log = fs::read_to_string(root.join(".fake/gh-log")).unwrap();
    assert_eq!(second_log.matches("gh release create").count(), 3);
}

#[test]
fn tag_create_rejects_divergent_local_and_remote_tags() {
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

    let output = trellis(&fixture("basic"))
        .args(["ci", "tag-package", "lat_mid-v9.9.9", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["name"], "lat_mid");
    assert_eq!(info["version"], "0.5.0");
    assert_eq!(info["tag-version"], "9.9.9");

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
        !log.contains("examples"),
        "ignore-release members never publish"
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

#[test]
fn publish_rejects_unreleasable_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    trellis(root)
        .args(["publish", "examples"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("excluded from release"));
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
