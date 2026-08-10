//! End-to-end tests running the trellis binary against fixture workspaces.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn trellis(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trellis").unwrap();
    cmd.current_dir(dir);
    cmd
}

// ---- list ------------------------------------------------------------

#[test]
fn list_prints_members_in_topological_order() {
    trellis(&fixture("basic"))
        .arg("list")
        .assert()
        .success()
        .stdout("lat_core   hex\nlat_mid    hex\nlat_cli    hex\npackage_a  workspace\n");
}

#[test]
fn list_treats_git_deps_with_subdirectory_paths_as_external() {
    // Gleam 1.18+ git deps may carry a `path` key selecting a subdirectory
    // of the remote repo. They must not join the workspace path-dep graph
    // (regression: they were misread as workspace path deps, making every
    // command fail with "workspace is invalid").
    trellis(&fixture("git-path-deps"))
        .arg("list")
        .assert()
        .success()
        .stdout("gp_core  hex\ngp_app   hex\n");
}

#[test]
fn list_works_from_inside_a_package() {
    trellis(&fixture("basic").join("packages/lat_mid"))
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("lat_core"));
}

#[test]
fn list_releasable_excludes_release_excluded_members() {
    trellis(&fixture("basic"))
        .args(["list", "--releasable"])
        .assert()
        .success()
        .stdout("lat_core  hex\nlat_mid   hex\nlat_cli   hex\n");
}

#[test]
fn list_json_includes_graph_facts() {
    let output = trellis(&fixture("basic"))
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema"], "trellis.list/1");
    let items = document["packages"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    let mid = items.iter().find(|i| i["name"] == "lat_mid").unwrap();
    assert_eq!(mid["version"], "0.5.0");
    assert_eq!(mid["path"], "packages/lat_mid");
    assert_eq!(mid["lifecycle"], "hex");
    assert_eq!(mid["releasable"], true);
    assert_eq!(mid["dependencies"], serde_json::json!(["lat_core"]));
    assert_eq!(mid["dependents"], serde_json::json!(["lat_cli"]));
    let package_a = items.iter().find(|i| i["name"] == "package_a").unwrap();
    assert_eq!(package_a["lifecycle"], "workspace");
    assert_eq!(package_a["releasable"], false);
}

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

// ---- global flags ----------------------------------------------------

#[test]
fn quiet_suppresses_the_package_stream_and_the_summary() {
    trellis(&fixture("basic"))
        .args(["run", "hello", "--quiet"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn quiet_leaves_the_exit_code_and_errors_alone() {
    trellis(&fixture("basic"))
        .args(["-q", "exec", "lat_core", "--", "sh", "-c", "exit 3"])
        .assert()
        .failure()
        .stdout("");
}

/// `-q` is not scoped to `run`/`exec` — it drops every command's normal-path
/// narration. `doctor`'s checked/warning/summary lines are as much "progress
/// chatter" as `run`'s per-package stream, and the exit code still carries
/// the verdict.
#[test]
fn quiet_suppresses_doctor_text_output_but_keeps_the_exit_code() {
    trellis(&fixture("basic"))
        .args(["doctor", "--quiet"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn quiet_leaves_doctor_json_and_github_formats_alone() {
    trellis(&fixture("basic"))
        .args(["doctor", "--quiet", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema\""));

    trellis(&fixture("basic"))
        .args(["list", "--quiet", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"schema\""));
}

#[test]
fn quiet_suppresses_lists_text_output() {
    trellis(&fixture("basic"))
        .args(["list", "--quiet"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn color_always_survives_a_pipe() {
    // Nothing here is a terminal, so auto-detection would say no; the flag is
    // the user overriding that.
    trellis(&fixture("basic"))
        .args(["run", "hello", "--color", "always"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\x1b["));
}

#[test]
fn color_flag_beats_no_color_in_the_environment() {
    trellis(&fixture("basic"))
        .env("NO_COLOR", "1")
        .args(["run", "hello", "--color", "always"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\x1b["));

    trellis(&fixture("basic"))
        .env_remove("NO_COLOR")
        .args(["run", "hello", "--color", "never"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\x1b[").not());
}

#[test]
fn verbose_traces_shelled_out_commands_to_stderr() {
    let output = trellis(&fixture("basic"))
        .args(["-v", "run", "hello", "lat_core"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("+ sh -c"),
        "expected a `+ ` command trace:\n{stderr}"
    );
    assert!(
        stderr.contains("packages/lat_core"),
        "the trace should say where the command ran:\n{stderr}"
    );
}

#[test]
fn command_traces_stay_off_unless_asked_for() {
    let output = trellis(&fixture("basic"))
        .args(["run", "hello", "lat_core"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("+ sh -c"), "unexpected trace:\n{stderr}");
}

#[test]
fn global_flags_are_accepted_after_the_subcommand() {
    // `global = true` is what makes this work; without it these would only
    // parse before the subcommand, which is not where anyone types them.
    trellis(&fixture("basic"))
        .args(["list", "--verbose", "--color", "never", "--no-update-check"])
        .assert()
        .success()
        .stdout("lat_core   hex\nlat_mid    hex\nlat_cli    hex\npackage_a  workspace\n");
}

#[test]
fn quiet_and_verbose_are_mutually_exclusive() {
    trellis(&fixture("basic"))
        .args(["-q", "-v", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// ---- doctor ----------------------------------------------------------

#[test]
fn doctor_passes_on_healthy_workspace() {
    trellis(&fixture("basic"))
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("ok: 4 package(s)"));
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn doctor_reports_all_problems_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\nexclude = { docs = [\"also-missing\"], \"@release\" = [\"nomatch\"] }\n",
    );
    // a: stale lockfile for b, and a path dep escaping the workspace
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n[dependencies]\nb = { path = \"../b\" }\nout = { path = \"../../../elsewhere\" }\n",
    );
    write(
        &root.join("packages/a/manifest.toml"),
        "packages = [ { name = \"b\", version = \"0.9.0\", source = \"local\", path = \"../b\" } ]\n",
    );
    write(
        &root.join("packages/b/gleam.toml"),
        "name = \"b\"\nversion = \"1.0.0\"\n",
    );
    // c: version behind its changelog
    write(
        &root.join("packages/c/gleam.toml"),
        "name = \"c\"\nversion = \"0.1.0\"\n",
    );
    write(&root.join("packages/c/CHANGELOG.md"), "# c\n\n## 0.2.0\n");

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("points outside the workspace"))
        .stdout(predicate::str::contains(
            "`@release` exclusion glob `nomatch` matches no member",
        ))
        .stdout(predicate::str::contains(
            "`docs` exclusion glob `also-missing` matches no member",
        ))
        .stdout(predicate::str::contains("locks `b` at 0.9.0"))
        .stdout(predicate::str::contains("behind its CHANGELOG"));
}

#[test]
fn doctor_accepts_working_at_members_exclusion_glob() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\nexclude = { \"@members\" = [\"packages/excluded\"] }\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );
    write(
        &root.join("packages/excluded/gleam.toml"),
        "name = \"excluded\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("matches no member").not());
}

#[test]
fn doctor_reports_at_members_exclusion_glob_typo() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\nexclude = { \"@members\" = [\"packages/nonexistent\"] }\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "`@members` exclusion glob `packages/nonexistent` matches no member",
        ));
}

#[test]
fn doctor_detects_dependency_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    write(
        &root.join("packages/b/gleam.toml"),
        "name = \"b\"\nversion = \"1.0.0\"\n[dependencies]\na = { path = \"../a\" }\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("dependency cycle"));
}

#[test]
fn doctor_flags_releasable_dep_on_unreleasable_member() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\", \"shared\"]\nexclude = { \"@release\" = [\"shared\"] }\n",
    );
    write(
        &root.join("packages/app/gleam.toml"),
        "name = \"app\"\nversion = \"1.0.0\"\n[dependencies]\nshared = { path = \"../../shared\" }\n",
    );
    write(
        &root.join("shared/gleam.toml"),
        "name = \"shared\"\nversion = \"0.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "which is unavailable in `app`'s distribution",
        ));
}

#[test]
fn doctor_flags_trellis_config_in_a_member_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    // A member with its own [tools.trellis] would hijack root discovery for
    // commands run inside it.
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n\n[tools.trellis]\nmembers = [\"nested/*\"]\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "member `packages/a` has a [tools.trellis] table",
        ));
}

#[test]
fn doctor_fix_seeds_missing_changelog() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .args(["doctor", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fixed: seed CHANGELOG.md for `a`"));

    // The seeded file matches the header `trellis new` scaffolds.
    let changelog = fs::read_to_string(root.join("packages/a/CHANGELOG.md")).unwrap();
    assert_eq!(changelog, "# a changelog\n");

    // A second run is clean: nothing left to fix.
    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: 1 package(s) (0 workspace, 0 git_only, 1 hex), 0 warning(s)",
        ));
}

/// A CHANGELOG.md that trellis never batched would be regenerated away on the
/// next release, so doctor surfaces it and `--fix` captures it up front.
#[test]
fn doctor_fix_adopts_unbatched_changelog_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );
    write(
        &root.join("packages/a/CHANGELOG.md"),
        "# a changelog\n\n## [1.0.0] - 2020-01-01\n\n- the beginning\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "package `a` has changelog history that trellis has not batched yet",
        ));

    trellis(root)
        .args(["doctor", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "fixed: adopt existing changelog history for `a`",
        ));

    // Captured verbatim, minus the header line the engine regenerates.
    let adopted = fs::read_to_string(root.join(".changes/a/v1.0.0.md")).unwrap();
    assert_eq!(adopted, "## [1.0.0] - 2020-01-01\n\n- the beginning\n");

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: 1 package(s) (0 workspace, 0 git_only, 1 hex), 0 warning(s)",
        ));
}

#[test]
fn doctor_fix_patches_stale_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    write(
        &root.join("packages/a/manifest.toml"),
        "packages = [ { name = \"b\", version = \"0.9.0\", source = \"local\", path = \"../b\" } ]\n",
    );
    // Give `a` a CHANGELOG so the only finding is the stale lockfile.
    write(&root.join("packages/a/CHANGELOG.md"), "# a changelog\n");
    write(
        &root.join("packages/b/gleam.toml"),
        "name = \"b\"\nversion = \"1.0.0\"\n",
    );
    write(&root.join("packages/b/CHANGELOG.md"), "# b changelog\n");

    trellis(root)
        .args(["doctor", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "fixed: patch locked versions in packages/a/manifest.toml",
        ));

    let manifest = fs::read_to_string(root.join("packages/a/manifest.toml")).unwrap();
    assert!(manifest.contains("version = \"1.0.0\""));
    assert!(!manifest.contains("0.9.0"));
}

#[test]
fn doctor_dry_run_lists_fixes_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .args(["doctor", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would fix: seed CHANGELOG.md for `a`",
        ));

    // Nothing was written.
    assert!(!root.join("packages/a/CHANGELOG.md").exists());
}

#[test]
fn doctor_fix_leaves_unfixable_findings_and_fails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    // a: fixable (missing CHANGELOG) plus an unfixable escaping path dep.
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n[dependencies]\nout = { path = \"../../../elsewhere\" }\n",
    );

    trellis(root)
        .args(["doctor", "--fix"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("fixed: seed CHANGELOG.md for `a`"))
        .stdout(predicate::str::contains("points outside the workspace"));

    // The fixable finding really was applied even though the run failed.
    assert!(root.join("packages/a/CHANGELOG.md").exists());
}

#[test]
fn doctor_github_format_emits_annotations_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    // a: a fixable warning (missing CHANGELOG) and an unfixable error, so both
    // annotation levels appear.
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n[dependencies]\nout = { path = \"../../../elsewhere\" }\n",
    );

    let assert = trellis(root)
        .args(["doctor", "--format", "github"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "::error title=path_dependency,file=packages/a/gleam.toml::",
        ))
        .stdout(predicate::str::contains(
            "::warning title=changelog_missing,file=packages/a/CHANGELOG.md::",
        ))
        // The prose surfaces belong to text mode alone.
        .stdout(predicate::str::contains("checked:").not())
        .stdout(predicate::str::contains("FAILED:").not())
        .stdout(predicate::str::contains("auto-fixable").not());

    // Every line is a workflow command; nothing else shares the stream.
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for line in stdout.lines() {
        assert!(line.starts_with("::"), "stray output: {line}");
    }
}

/// A run with nothing to annotate must say nothing at all — a summary line
/// would show up as an unexplained log entry on every green PR.
///
/// Built here rather than taken from a fixture: the `basic` fixture carries
/// unbatched changelog history, so it legitimately warns.
#[test]
fn doctor_github_format_is_silent_when_there_is_nothing_to_report() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\
         exclude = { \"@release\" = [\"packages/a\"] }\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .args(["doctor", "--format", "github"])
        .assert()
        .success()
        .stdout("");
}

/// A message spanning lines would otherwise truncate the annotation at the
/// break, or inject a second workflow command.
#[test]
fn doctor_github_format_escapes_multiline_messages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion =\n",
    );

    let assert = trellis(root)
        .args(["doctor", "--format", "github"])
        .assert()
        .failure();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("%0A"), "newline was not escaped: {stdout}");
    assert_eq!(stdout.lines().count(), 1, "annotation spilled: {stdout}");
}

/// `--json` owns stdout the same way, and still reports through the exit code.
#[test]
fn doctor_json_format_emits_only_the_payload() {
    let assert = trellis(&fixture("basic"))
        .args(["doctor", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("checked:").not());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(payload["schema"], "trellis.doctor/1");
    assert_eq!(payload["ok"], true);
}

/// `--fix` in a structured format reports what it wrote through `applied`
/// rather than through the `fixed:` prose it suppresses.
#[test]
fn doctor_json_format_reports_applied_fixes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    let assert = trellis(root)
        .args(["doctor", "--fix", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fixed:").not());

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(payload["applied"][0]["kind"], "seed_changelog");
    assert_eq!(payload["applied"][0]["file"], "packages/a/CHANGELOG.md");
    // The re-inspect ran, so the warning it fixed is gone from the payload.
    assert_eq!(payload["findings"].as_array().unwrap().len(), 0);
    assert_eq!(payload["fixes"].as_array().unwrap().len(), 0);
    assert!(root.join("packages/a/CHANGELOG.md").exists());
}

#[test]
fn strict_load_fails_on_broken_workspace_but_names_doctor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"pkgs/*\"]\n",
    );
    trellis(root)
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains("matches no packages"))
        .stderr(predicate::str::contains("trellis doctor"));
}

#[test]
fn member_glob_skips_directories_without_gleam_toml() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"pkgs/*\"]\n",
    );
    write(
        &root.join("pkgs/a/gleam.toml"),
        "name = \"a\"\nversion = \"0.1.0\"\n",
    );
    // Non-package clutter that a wildcard sweeps up (e.g. node_modules).
    std::fs::create_dir_all(root.join("pkgs/node_modules")).unwrap();
    let output = trellis(root).arg("list").assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout).to_string();
    assert!(stdout.contains("a"));
    assert!(!stdout.contains("node_modules"));
}

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

// ---- markdown reference ----------------------------------------------

#[test]
fn markdown_reference_page_is_up_to_date() {
    let output = Command::cargo_bin("trellis")
        .unwrap()
        .arg("markdown-help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let generated = String::from_utf8(output.stdout).unwrap();
    let checked_in = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("website/src/content/docs/docs/reference.md"),
    )
    .unwrap();
    assert_eq!(
        generated, checked_in,
        "CLI reference is stale — regenerate with \
         `trellis markdown-help > website/src/content/docs/docs/reference.md`"
    );
}

// ---- man pages -------------------------------------------------------

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn man_pages_are_up_to_date() {
    let generated = tempfile::tempdir().unwrap();
    Command::cargo_bin("trellis")
        .unwrap()
        .arg("man")
        .arg("--out")
        .arg(generated.path())
        .assert()
        .success();

    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/man");
    // Compare the file set first: a removed subcommand leaves a stale page that
    // matching contents alone would never catch.
    assert_eq!(
        file_names(generated.path()),
        file_names(&committed),
        "man page set is stale — regenerate with `just docs`"
    );
    for name in file_names(generated.path()) {
        assert_eq!(
            fs::read_to_string(generated.path().join(&name)).unwrap(),
            fs::read_to_string(committed.join(&name)).unwrap(),
            "assets/man/{name} is stale — regenerate with `just docs`"
        );
    }
}

#[test]
fn man_pages_carry_no_version() {
    // The pages are committed, so a version string in them would go stale on
    // every release — and since CI runs this suite on the release PR that bumps
    // Cargo.toml, that PR would fail every time.
    let page =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/man/trellis.1"))
            .unwrap();
    assert!(
        !page.contains(env!("CARGO_PKG_VERSION")),
        "man pages must not embed the crate version"
    );
    // All five .TH fields present and aligned; an empty date would otherwise
    // collapse and shift source/manual one field left.
    assert!(
        page.contains(r#".TH trellis 1 "" trellis "Trellis Manual""#),
        "unexpected .TH line in assets/man/trellis.1"
    );
}

// ---- completions -----------------------------------------------------

#[test]
fn completions_emit_a_registration_snippet_per_shell() {
    for (shell, needle) in [
        ("bash", "complete -o nospace"),
        ("zsh", "#compdef trellis"),
        ("fish", "complete"),
        ("elvish", "edit:completion:arg-completer[trellis]"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        Command::cargo_bin("trellis")
            .unwrap()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains(needle))
            // Every snippet hands the shell name back through $COMPLETE; the
            // spelling differs (`COMPLETE=zsh` vs PowerShell's `$env:COMPLETE`).
            .stdout(predicate::str::contains("COMPLETE"))
            // The snippet must invoke `trellis` from $PATH. Left to default,
            // clap_complete bakes in argv[0] — i.e. this checkout's
            // target/debug path — which would break on any other machine.
            .stdout(predicate::str::contains("target/debug").not());
    }
}

/// Drive the completion engine the way a shell would: `COMPLETE=<shell>` names
/// the shell, `_CLAP_COMPLETE_INDEX` is the cursor position, and the words
/// after `--` are the command line being completed.
fn complete(dir: &Path, index: usize, words: &[&str]) -> String {
    let output = trellis(dir)
        .env("COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", index.to_string())
        .env("_CLAP_IFS", "\n")
        .arg("--")
        .args(words)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn completion_offers_subcommands_and_hides_internal_ones() {
    let out = complete(&fixture("basic"), 1, &["trellis", ""]);
    assert!(out.contains("doctor"), "{out}");
    assert!(out.contains("completions"), "{out}");
    // The hidden maintainer commands must stay out of a user's shell.
    assert!(!out.contains("markdown-help"), "{out}");
    assert!(!out.contains("man\n"), "{out}");
}

#[test]
fn completion_offers_workspace_package_names() {
    let out = complete(&fixture("basic"), 2, &["trellis", "info", ""]);
    for package in ["lat_core", "lat_mid", "lat_cli", "package_a"] {
        assert!(out.contains(package), "missing {package} in: {out}");
    }
    // Prefix filtering is the engine's job, but verify it reaches our candidates.
    let filtered = complete(&fixture("basic"), 2, &["trellis", "info", "lat_"]);
    assert!(filtered.contains("lat_core"), "{filtered}");
    assert!(!filtered.contains("package_a"), "{filtered}");
}

#[test]
fn completion_offers_releasable_packages_only_where_it_should() {
    // `package_a` is excluded from releases by the fixture's `@release` config.
    let out = complete(&fixture("basic"), 2, &["trellis", "publish", ""]);
    assert!(out.contains("lat_core"), "{out}");
    assert!(!out.contains("package_a"), "{out}");
}

#[test]
fn completion_offers_builtin_and_configured_tasks() {
    let out = complete(&fixture("basic"), 2, &["trellis", "run", ""]);
    assert!(out.contains("build"), "{out}");
    // `hello` comes from the fixture's [tools.trellis.tasks].
    assert!(out.contains("hello"), "{out}");
}

#[test]
fn completion_outside_a_workspace_degrades_quietly() {
    // Completion fires wherever the cursor is. Failing to load a workspace must
    // yield no candidates rather than an error, or the shell becomes unusable.
    let dir = tempfile::tempdir().unwrap();
    let out = complete(dir.path(), 2, &["trellis", "info", ""]);
    assert!(!out.contains("lat_core"), "{out}");
    assert!(out.contains("--json"), "flags should still complete: {out}");
}

// ---- --since ---------------------------------------------------------

#[test]
fn since_selects_changed_packages_and_dependents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Copy the basic fixture into a real git repo.
    copy_dir(&fixture("basic"), root);

    let git = |args: &[&str]| {
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
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["checkout", "-q", "-b", "feature"]);
    write(&root.join("packages/lat_mid/src/new.gleam"), "// change\n");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "touch mid"]);

    trellis(root)
        .args(["list", "--since", "main"])
        .assert()
        .success()
        .stdout("lat_mid  hex\n");

    trellis(root)
        .args(["list", "--since", "main", "--with-dependents"])
        .assert()
        .success()
        .stdout("lat_mid    hex\nlat_cli    hex\npackage_a  workspace\n");

    // Uncommitted changes count too.
    write(&root.join("packages/lat_core/src/wip.gleam"), "// wip\n");
    trellis(root)
        .args(["list", "--since", "main"])
        .assert()
        .success()
        .stdout("lat_core  hex\nlat_mid   hex\n");
}

// ---- version ---------------------------------------------------------

#[test]
fn version_appends_git_describe_on_dev_builds() {
    let output = Command::cargo_bin("trellis")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let base = env!("CARGO_PKG_VERSION");

    // Integration tests share the package's build-script env, so the same
    // VERGEN_GIT_DESCRIBE the binary embedded is visible here.
    match option_env!("VERGEN_GIT_DESCRIBE") {
        // "VERGEN_IDEMPOTENT_OUTPUT" is vergen's fallback when git info is
        // unavailable (e.g. building from a crates.io tarball).
        Some(describe)
            if describe != "VERGEN_IDEMPOTENT_OUTPUT" && describe != format!("v{base}") =>
        {
            assert_eq!(stdout.trim(), format!("trellis {base} ({describe})"));
        }
        _ => assert_eq!(stdout.trim(), format!("trellis {base}")),
    }
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in walk(from) {
        let rel = entry.strip_prefix(from).unwrap();
        let dest = to.join(rel);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&entry, &dest).unwrap();
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

// ---- doctor: unrecognized config keys ---------------------------------

/// A two-member workspace, so the shared_dependency check has something to
/// compare. `config` is spliced in under `[tools.trellis]`.
fn workspace_with(root: &Path, config: &str, a_deps: &str, b_deps: &str) {
    write(
        &root.join("gleam.toml"),
        &format!("[tools.trellis]\nmembers = [\"packages/*\"]\n{config}"),
    );
    write(
        &root.join("packages/a/gleam.toml"),
        &format!("name = \"a\"\nversion = \"1.0.0\"\n[dependencies]\n{a_deps}"),
    );
    write(
        &root.join("packages/b/gleam.toml"),
        &format!("name = \"b\"\nversion = \"1.0.0\"\n[dependencies]\n{b_deps}"),
    );
}

#[test]
fn a_pre_0_8_kebab_case_key_still_works_and_says_it_is_deprecated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // The spelling every release through v0.7.0 documented. `package_tags` is
    // snake-case-only: it postdates the rename, so it has no kebab spelling to
    // be deprecated for.
    workspace_with(
        root,
        "[tools.trellis.publish]\nseries-tag-format = \"{name}@{series}\"\n\
         package_tags = [\"minor\"]\n",
        "",
        "",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        // A warning, not an error: the key still configures what it always did,
        // so failing would break working repositories over a spelling.
        .success()
        .stdout(predicate::str::contains(
            "key `publish.series-tag-format` is deprecated; rename it to \
             `publish.series_tag_format`",
        ));

    // And it is still in effect — the old name is an alias, not a no-op. This
    // is the half that a silently-ignored key would fail: the tags would come
    // out in the default `{name}-v{series}` scheme instead.
    trellis(root)
        .args(["ci", "outputs"])
        .assert()
        .success()
        .stdout(predicate::str::contains(r#"series_tags=["a@1.0","b@1.0"]"#));
}

#[test]
fn an_unrecognized_config_key_warns_but_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // Not a real key in any spelling, so it may belong to a newer trellis.
    // Erroring would make a workspace unloadable under a pinned older trellis,
    // which is a bad failure for a tool CI pins.
    workspace_with(root, "future_thing = true\n", "", "");

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "key `future_thing` is not recognized and is being ignored",
        ));
    trellis(root)
        .arg("list")
        .assert()
        .success()
        .stdout("a  hex\nb  hex\n");
}

#[test]
fn unknown_key_detection_reaches_nested_tables_and_leaves_free_form_ones_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    workspace_with(
        root,
        // `tasks` and `exclude` take arbitrary user-chosen keys, hyphens and
        // all; only the typed structs have a fixed key set.
        "exclude = { \"my-task\" = [\"packages/a\"] }\n\
         [tools.trellis.tasks.my-task]\ncommand = \"true\"\n\
         [tools.trellis.changelog]\nnonsense = 1\n",
        "",
        "",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("`changelog.nonsense`"))
        // Neither unrecognized nor deprecated: the user named this table.
        .stdout(predicate::str::contains("my-task").not());
}

// ---- doctor: shared dependency agreement ------------------------------

#[test]
fn divergent_shared_dependencies_warn_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    workspace_with(
        root,
        "",
        "gleam_stdlib = \">= 0.44.0\"\n",
        "gleam_stdlib = \">= 0.60.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        // A warning: divergence is sometimes intended, so it does not fail CI.
        .success()
        .stdout(predicate::str::contains(
            "packages disagree on `gleam_stdlib`",
        ))
        .stdout(predicate::str::contains("`>= 0.44.0` (a)"))
        .stdout(predicate::str::contains("`>= 0.60.0` (b)"));
}

#[test]
fn agreeing_members_and_path_deps_produce_no_finding() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    workspace_with(
        root,
        "",
        "gleam_stdlib = \">= 0.44.0\"\n",
        "gleam_stdlib = \">= 0.44.0\"\na = { path = \"../a\" }\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("disagree").not());
}

#[test]
fn shared_dependency_strictness_is_configurable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let diverging = |config: &str| {
        workspace_with(
            root,
            config,
            "gleam_stdlib = \">= 0.44.0\"\n",
            "gleam_stdlib = \">= 0.60.0\"\n",
        );
    };

    diverging("[tools.trellis.doctor]\nshared_dependencies = \"error\"\n");
    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "packages disagree on `gleam_stdlib`",
        ));

    diverging("[tools.trellis.doctor]\nshared_dependencies = \"off\"\n");
    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("disagree").not());
}

#[test]
fn the_new_doctor_table_is_itself_a_recognized_key() {
    // The two halves of this change meet here: [tools.trellis.doctor] must not
    // trip the unknown-key detection it shipped alongside.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    workspace_with(
        root,
        "[tools.trellis.doctor]\nshared_dependencies = \"warn\"\n",
        "",
        "",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("not recognized").not())
        .stdout(predicate::str::contains("is deprecated").not());

    // ...and its own keys are checked like any other. This table arrived after
    // the kebab-case era, so the hyphenated spelling gets no alias.
    workspace_with(
        root,
        "[tools.trellis.doctor]\nshared-dependencies = \"warn\"\n",
        "",
        "",
    );
    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "key `doctor.shared-dependencies` is not recognized",
        ));
}
