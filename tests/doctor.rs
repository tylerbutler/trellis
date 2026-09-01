//! End-to-end tests for `trellis doctor`.

mod common;

use common::{copy_fixture_to, fixture, make_executable, trellis, write};
use predicates::prelude::*;
use std::fs;
use std::path::Path;

// ---- doctor ----------------------------------------------------------

#[test]
fn doctor_passes_on_healthy_workspace() {
    trellis(&fixture("basic"))
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("ok: 4 package(s)"));
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

    // The seeded file is the rendered header alone.
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

// ---- doctor .tool-versions advisory ----------------------------------------

#[test]
fn doctor_warns_on_tool_versions_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    write(&root.join(".tool-versions"), "erlang 27.0\ngleam 1.5.0\n");
    let gleam = root.join("fake-gleam.sh");
    write(&gleam, "#!/bin/sh\necho 'gleam 1.4.1'\n");
    make_executable(&gleam);

    // Mismatch is a warning, not an error: doctor still succeeds.
    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "gleam on PATH is 1.4.1 but .tool-versions pins 1.5.0",
        ));

    // Matching versions: no warning.
    write(&root.join(".tool-versions"), "gleam 1.4.1\n");
    trellis(root)
        .env("TRELLIS_GLEAM_BIN", &gleam)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("gleam on PATH is").not());
}
