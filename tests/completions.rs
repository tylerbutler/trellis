//! End-to-end tests for `trellis completions`.

mod common;

use assert_cmd::Command;
use common::{fixture, trellis};
use predicates::prelude::*;
use std::path::Path;

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
