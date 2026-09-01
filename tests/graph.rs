//! End-to-end tests for `trellis graph`.

mod common;

use common::{fixture, trellis};
use predicates::prelude::*;

// ---- graph -----------------------------------------------------------

#[test]
fn graph_mermaid_shows_edges() {
    trellis(&fixture("basic"))
        .args(["graph", "--format", "mermaid"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_mid --> lat_core"))
        .stdout(predicate::str::contains("lat_cli --> lat_mid"));
}

#[test]
fn graph_json_lists_nodes_and_edges() {
    let output = trellis(&fixture("basic"))
        .args(["graph", "--format", "json"])
        .output()
        .unwrap();
    let graph: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 4);
    // lat_mid->lat_core, lat_cli->lat_mid, lat_cli->lat_core (dev),
    // package_a->lat_cli
    assert_eq!(graph["edges"].as_array().unwrap().len(), 4);
}
