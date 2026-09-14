//! End-to-end tests for `trellis run` and `trellis exec`.

mod common;

use common::{fixture, trellis};
use predicates::prelude::*;

// ---- run / exec ------------------------------------------------------

#[test]
fn run_custom_task_fans_out_with_prefixes() {
    trellis(&fixture("basic"))
        .args(["run", "hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core"))
        // once for the echoed `$ ...` command line and once for its output
        .stdout(predicate::str::contains("hello-from-task").count(6))
        .stdout(predicate::str::contains("package_a").not())
        .stdout(predicate::str::contains("\x1b[").not())
        .stdout(predicate::str::contains("ok"));
}

#[test]
fn task_exclusions_apply_to_explicit_package_selection() {
    trellis(&fixture("basic"))
        .args(["run", "hello", "package_a"])
        .assert()
        .success()
        .stdout("no packages selected\n");
}

#[test]
fn built_in_task_can_be_excluded_without_overriding_its_command() {
    trellis(&fixture("basic"))
        .env("TRELLIS_GLEAM_BIN", "echo")
        .args(["run", "docs", "--serial"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core"))
        .stdout(predicate::str::contains("lat_mid"))
        .stdout(predicate::str::contains("lat_cli"))
        .stdout(predicate::str::contains("package_a").not());
}

#[test]
fn run_unknown_task_names_the_alternatives() {
    trellis(&fixture("basic"))
        .args(["run", "bogus"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown task `bogus`"))
        .stderr(predicate::str::contains("build, test, check"))
        .stderr(predicate::str::contains("hello"));
}

#[test]
fn exec_runs_command_in_each_selected_package() {
    trellis(&fixture("basic"))
        .args(["exec", "lat_core", "lat_mid", "--", "cat", "gleam.toml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("name = \"lat_core\""))
        .stdout(predicate::str::contains("name = \"lat_mid\""))
        .stdout(predicate::str::contains("lat_cli").not());
}

#[test]
fn exec_serial_respects_dependency_order() {
    let output = trellis(&fixture("basic"))
        .args([
            "exec",
            "--serial",
            "--",
            "sh",
            "-c",
            "grep ^name gleam.toml",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let core = stdout.find("name = \"lat_core\"").unwrap();
    let mid = stdout.find("name = \"lat_mid\"").unwrap();
    let cli = stdout.find("name = \"lat_cli\"").unwrap();
    assert!(
        core < mid && mid < cli,
        "expected dependency order:\n{stdout}"
    );
}

#[test]
fn exec_failure_sets_exit_code_and_summary() {
    trellis(&fixture("basic"))
        .args(["exec", "lat_core", "--", "sh", "-c", "exit 3"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAILED"));
}

#[test]
fn exec_failure_stops_scheduling_without_keep_going() {
    // The first package (lat_core) fails, so the remaining three are skipped.
    trellis(&fixture("basic"))
        .args(["exec", "--serial", "--", "sh", "-c", "exit 1"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAILED").count(1))
        .stdout(predicate::str::contains("skipped").count(3));
}

#[test]
fn exec_keep_going_runs_everything_despite_failures() {
    trellis(&fixture("basic"))
        .args([
            "exec",
            "--serial",
            "--keep-going",
            "--",
            "sh",
            "-c",
            "exit 1",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAILED").count(4));
}

// ---- run / exec --json -----------------------------------------------

#[test]
fn run_json_reports_one_record_per_package() {
    let output = trellis(&fixture("basic"))
        .args(["run", "hello", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "trellis.run/1");
    assert_eq!(document["ok"], true);
    assert_eq!(document["task"], "hello");
    // `--target` was not given, so the field is absent rather than null.
    assert!(document.get("target").is_none());
    let results = document["results"].as_array().unwrap();
    // package_a is excluded from `hello` by the fixture's [tools.trellis.exclude].
    assert_eq!(results.len(), 3);
    let core = results.iter().find(|r| r["package"] == "lat_core").unwrap();
    assert_eq!(core["path"], "packages/lat_core");
    assert_eq!(core["status"], "success");
    assert!(core["duration_ms"].is_u64());
    // Nothing failed, so neither failure field is present.
    assert!(core.get("exit_code").is_none());
    assert!(core.get("command").is_none());
}

#[test]
fn run_json_carries_the_target_flag_as_given() {
    let output = trellis(&fixture("basic"))
        .env("TRELLIS_GLEAM_BIN", "echo")
        .args(["run", "docs", "--target", "all", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["target"], "all");
}

#[test]
fn exec_json_records_the_exit_code_of_the_failing_command() {
    let output = trellis(&fixture("basic"))
        .args(["exec", "lat_core", "--json", "--", "sh", "-c", "exit 3"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "trellis.exec/1");
    assert_eq!(document["ok"], false);
    // argv, not a re-splittable string.
    assert_eq!(
        document["command"],
        serde_json::json!(["sh", "-c", "exit 3"])
    );
    let results = document["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["status"], "failed");
    assert_eq!(results[0]["exit_code"], 3);
    // `sh -c` is unwrapped back to the script, matching what the summary table
    // and the `$ ...` echo have always shown.
    assert_eq!(results[0]["command"], "exit 3");
}

#[test]
fn exec_json_distinguishes_skipped_from_failed() {
    // lat_core fails first, so the other three never run. Skipped is not a
    // pass: it carries no exit code and still fails the command.
    let output = trellis(&fixture("basic"))
        .args(["exec", "--serial", "--json", "--", "sh", "-c", "exit 1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let results = document["results"].as_array().unwrap();
    let statuses: Vec<&str> = results
        .iter()
        .map(|r| r["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["failed", "skipped", "skipped", "skipped"]);
    let skipped = results.iter().find(|r| r["status"] == "skipped").unwrap();
    assert!(skipped.get("exit_code").is_none());
    assert!(skipped.get("command").is_none());
}

#[test]
fn json_keeps_stdout_clean_and_moves_package_output_to_stderr() {
    let output = trellis(&fixture("basic"))
        .args(["run", "hello", "--json"])
        .output()
        .unwrap();
    // The whole of stdout parses, so no stray progress or summary line leaked
    // into it — that is the property a consumer depends on.
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("hello-from-task"),
        "package output should stay observable on stderr:\n{stderr}"
    );
    assert!(stderr.contains('▏'), "expected the pkg ▏ prefix:\n{stderr}");
}

#[test]
fn json_emits_a_document_even_when_nothing_is_selected() {
    // The "no packages selected" notice would otherwise be the one thing on
    // stdout that is not JSON.
    let output = trellis(&fixture("basic"))
        .args(["run", "hello", "package_a", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], true);
    assert_eq!(document["results"], serde_json::json!([]));
}
