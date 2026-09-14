//! End-to-end tests for `trellis publish`.

mod common;

use common::{copy_fixture_to, install_fake_gleam, trellis_with_local_http as trellis, write};
use predicates::prelude::*;
use std::fs;
use std::io::{Read, Write as IoWrite};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;

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

#[test]
fn publish_rejects_unreleasable_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    trellis(root)
        .args(["publish", "package_a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "release lifecycle `workspace`, not `hex`",
        ));
}
