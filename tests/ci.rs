//! End-to-end tests for `trellis ci`.

mod common;

use common::{fixture, trellis};
use predicates::prelude::*;

// ---- ci --------------------------------------------------------------

#[test]
fn ci_matrix_emits_github_actions_shape() {
    let output = trellis(&fixture("basic"))
        .args(["ci", "matrix"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let matrix: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let include = matrix["include"].as_array().unwrap();
    assert_eq!(include.len(), 4);
    assert_eq!(include[0]["name"], "lat_core");
    assert_eq!(include[0]["path"], "packages/lat_core");
    assert_eq!(include[0]["version"], "1.2.0");
}

#[test]
fn ci_outputs_emits_key_value_lines() {
    trellis(&fixture("basic"))
        .args(["ci", "outputs"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "projects=[\"lat_core\",\"lat_mid\",\"lat_cli\",\"package_a\"]",
        ))
        .stdout(predicate::str::contains(
            "releasable=[\"lat_core\",\"lat_mid\",\"lat_cli\"]",
        ))
        .stdout(predicate::str::contains("lat_core-v1.2.0"));
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
    assert_eq!(info["tag_version"], "9.9.9");

    trellis(&fixture("basic"))
        .args(["ci", "tag-package", "unrelated-v1.0.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not match any releasable package",
        ));
}
