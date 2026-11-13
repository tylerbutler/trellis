//! End-to-end tests for `trellis run` and `trellis exec`.

mod common;

use common::{copy_fixture_to, fixture, init_repo, trellis, write};
use predicates::prelude::*;

fn selected_packages(dir: &std::path::Path, command: &str, selectors: &[&str]) -> Vec<String> {
    let mut cmd = trellis(dir);
    cmd.arg(command);
    if command == "run" {
        cmd.arg("hello");
    }
    cmd.args(selectors).arg("--json");
    if command == "exec" {
        cmd.args(["--", "echo", "selected"]);
    }
    let output = cmd.assert().success().get_output().clone();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    document["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["package"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn path_selectors_intersect_names_and_union_repeated_patterns() {
    for command in ["run", "exec"] {
        assert_eq!(
            selected_packages(
                &fixture("basic"),
                command,
                &["--path", "packages/lat_core", "--path", "packages/lat_mid",],
            ),
            ["lat_core", "lat_mid"],
        );
        assert_eq!(
            selected_packages(
                &fixture("basic"),
                command,
                &["lat_core", "lat_cli", "--path", "packages/lat_core"],
            ),
            ["lat_core"],
        );
        assert!(
            selected_packages(
                &fixture("basic"),
                command,
                &["lat_core", "--path", "packages/lat_mid"],
            )
            .is_empty()
        );
        assert_eq!(
            selected_packages(
                &fixture("basic"),
                command,
                &[
                    "--exclude-path",
                    "packages/lat_mid",
                    "--exclude-path",
                    "examples/*",
                ],
            ),
            ["lat_core", "lat_cli"],
        );
        assert!(
            selected_packages(
                &fixture("basic"),
                command,
                &["lat_core", "--exclude-path", "packages/lat_core"],
            )
            .is_empty()
        );
    }
}

#[test]
fn paths_select_members_not_names_and_preserve_task_exclusions() {
    assert_eq!(
        selected_packages(&fixture("basic"), "exec", &["--path", "examples/*"]),
        ["package_a"],
    );
    assert!(selected_packages(&fixture("basic"), "run", &["--path", "examples/*"]).is_empty());
    trellis(&fixture("basic"))
        .args([
            "exec",
            "--path",
            "examples/*",
            "--",
            "sh",
            "-c",
            "grep ^name gleam.toml",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("name = \"package_a\""))
        .stdout(predicate::str::contains("lat_core").not())
        .stdout(predicate::str::contains("lat_mid").not())
        .stdout(predicate::str::contains("lat_cli").not());
}

#[test]
fn paths_are_root_relative_and_include_new_members_and_root_dot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    copy_fixture_to(root);
    let config = std::fs::read_to_string(root.join("gleam.toml")).unwrap();
    write(
        &root.join("gleam.toml"),
        &format!(
            "name = \"workspace_root\"\nversion = \"1.0.0\"\n{}",
            config.replace(
                "members = [\"packages/*\", \"examples/*\"]",
                "members = [\".\", \"packages/*\", \"examples/*\"]",
            ),
        ),
    );
    write(
        &root.join("examples/another_directory/gleam.toml"),
        "name = \"new_example\"\nversion = \"1.0.0\"\n",
    );
    for command in ["run", "exec"] {
        let nested = root.join("packages/lat_mid");
        assert_eq!(
            selected_packages(&nested, command, &["--path", "."]),
            ["workspace_root"]
        );
        assert_eq!(
            selected_packages(&nested, command, &["--path", "packages/lat_core"]),
            ["lat_core"],
        );
    }
    assert_eq!(
        selected_packages(root, "exec", &["--path", "examples/*"]),
        ["new_example", "package_a"],
    );
}

#[test]
fn every_path_pattern_must_be_valid_and_match_a_member_before_execution() {
    for command in ["run", "exec"] {
        for flag in ["--path", "--exclude-path"] {
            for pattern in ["[", "missing/*", "lat_core", "packages/*/gleam.toml"] {
                let mut cmd = trellis(&fixture("basic"));
                cmd.arg(command);
                if command == "run" {
                    cmd.arg("hello");
                }
                cmd.args(["--path", "packages/*", flag, pattern]);
                if command == "exec" {
                    cmd.args(["--", "echo", "must-not-run"]);
                }
                cmd.assert()
                    .failure()
                    .stderr(predicate::str::contains(flag))
                    .stderr(predicate::str::contains(pattern))
                    .stdout("");
            }
        }
    }
}

#[test]
fn paths_intersect_since_and_bound_dependent_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    copy_fixture_to(root);
    init_repo(root);
    write(
        &root.join("packages/lat_core/src/change.gleam"),
        "// changed\n",
    );
    for command in ["run", "exec"] {
        assert_eq!(
            selected_packages(root, command, &["--since", "HEAD", "--path", "packages/*"]),
            ["lat_core"],
        );
        assert!(
            selected_packages(
                root,
                command,
                &["--since", "HEAD", "--path", "packages/lat_mid"]
            )
            .is_empty()
        );
    }
    assert_eq!(
        selected_packages(
            root,
            "run",
            &[
                "--since",
                "HEAD",
                "--with-dependents",
                "--path",
                "packages/*",
                "--exclude-path",
                "packages/lat_mid"
            ],
        ),
        ["lat_core", "lat_cli"],
    );
    assert_eq!(
        selected_packages(
            root,
            "run",
            &["--with-dependents", "--path", "packages/lat_core"],
        ),
        ["lat_core"],
    );
    assert!(
        selected_packages(
            root,
            "run",
            &[
                "--since",
                "HEAD",
                "--with-dependents",
                "--path",
                "packages/lat_mid"
            ],
        )
        .is_empty()
    );
}

#[test]
fn filtered_execution_preserves_order_limits_and_failure_scheduling() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    copy_fixture_to(root);
    let config = std::fs::read_to_string(root.join("gleam.toml")).unwrap();
    let script = format!(
        "mkdir '{}/lock' || exit 9; sleep 0.01; rmdir '{}/lock'; exit 3",
        root.display(),
        root.display(),
    );
    write(
        &root.join("gleam.toml"),
        &config.replace("echo hello-from-task", &script),
    );
    for command in ["run", "exec"] {
        for limit in [vec!["--serial"], vec!["--jobs", "1"]] {
            for keep_going in [false, true] {
                let mut cmd = trellis(root);
                cmd.arg(command);
                if command == "run" {
                    cmd.arg("hello");
                }
                cmd.args(["--path", "packages/*", "--json"]).args(&limit);
                if keep_going {
                    cmd.arg("--keep-going");
                }
                if command == "exec" {
                    cmd.args(["--", "sh", "-c", &script]);
                }
                let output = cmd.assert().failure().get_output().clone();
                let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                let results = document["results"].as_array().unwrap();
                let packages: Vec<_> = results
                    .iter()
                    .map(|r| r["package"].as_str().unwrap())
                    .collect();
                assert_eq!(packages, ["lat_core", "lat_mid", "lat_cli"]);
                for (idx, result) in results.iter().enumerate() {
                    if keep_going || idx == 0 {
                        assert_eq!(result["status"], "failed");
                        assert_eq!(result["exit_code"], 3);
                    } else {
                        assert_eq!(result["status"], "skipped");
                    }
                }
            }
        }
    }
}

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
