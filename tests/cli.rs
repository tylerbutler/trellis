//! End-to-end tests for global CLI behavior.

mod common;

use assert_cmd::Command;
use common::{fixture, trellis};
use predicates::prelude::*;

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
