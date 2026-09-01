//! End-to-end tests for `trellis info`.

mod common;

use common::{fixture, series_repo, trellis};
use predicates::prelude::*;

// ---- info ------------------------------------------------------------

#[test]
fn info_shows_package_details() {
    trellis(&fixture("basic"))
        .args(["info", "lat_core"])
        .assert()
        .success()
        .stdout(predicate::str::contains("version:    1.2.0"))
        .stdout(predicate::str::contains("tag:        lat_core-v1.2.0"))
        .stdout(predicate::str::contains("lat_mid"));
}

#[test]
fn info_rejects_unknown_package() {
    trellis(&fixture("basic"))
        .args(["info", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown package"));
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
