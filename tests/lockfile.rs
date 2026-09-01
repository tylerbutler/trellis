//! End-to-end tests for `trellis lockfile`.

mod common;

use common::{copy_fixture_to, install_fake_gleam, trellis_with_local_http as trellis};
use std::fs;

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
