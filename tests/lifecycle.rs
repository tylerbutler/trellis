//! End-to-end tests for per-package release lifecycle states (issue #74):
//! `publish.lifecycle` configuration, resolution against the legacy
//! `exclude.@release` mapping, the dependency-availability rule, and the
//! lifecycle matrix across changelog, version, tag, publish, and CI commands.
//!
//! `tests/fixtures/lifecycle` fixes one workspace exercising all three
//! states: `core` is the implicit default (`hex`), `adapter` is explicit
//! `git_only`, `tooling` is explicit `workspace`, and `demo` reaches
//! `workspace` through the legacy `@release` mapping instead of an explicit
//! rule.

mod common;

use common::{fixture, trellis_with_stable_date as trellis, write};
use predicates::prelude::*;

// ---- introspection over the mixed-lifecycle fixture -------------------

#[test]
fn doctor_passes_the_mixed_lifecycle_workspace() {
    trellis(&fixture("lifecycle"))
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: 4 package(s) (2 workspace, 1 git_only, 1 hex)",
        ));
}

#[test]
fn list_json_reports_all_three_lifecycle_states() {
    let output = trellis(&fixture("lifecycle"))
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let items = document["packages"].as_array().unwrap();
    let lifecycle_of = |name: &str| {
        items
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("no package named {name}"))["lifecycle"]
            .clone()
    };
    assert_eq!(lifecycle_of("core"), "hex");
    assert_eq!(lifecycle_of("adapter"), "git_only");
    // Explicit rule and legacy `@release` mapping agree on the same state.
    assert_eq!(lifecycle_of("tooling"), "workspace");
    assert_eq!(lifecycle_of("demo"), "workspace");
    // `releasable` stays the git_only+hex compatibility boolean.
    let core = items.iter().find(|p| p["name"] == "core").unwrap();
    let adapter = items.iter().find(|p| p["name"] == "adapter").unwrap();
    let tooling = items.iter().find(|p| p["name"] == "tooling").unwrap();
    assert_eq!(core["releasable"], true);
    assert_eq!(adapter["releasable"], true);
    assert_eq!(tooling["releasable"], false);
}

#[test]
fn list_releasable_filters_to_git_only_and_hex() {
    trellis(&fixture("lifecycle"))
        .args(["list", "--releasable"])
        .assert()
        .success()
        .stdout("core     hex\nadapter  git_only\n");
}

#[test]
fn list_text_default_shows_a_lifecycle_column_for_every_state() {
    trellis(&fixture("lifecycle"))
        .arg("list")
        .assert()
        .success()
        .stdout("core     hex\nadapter  git_only\ndemo     workspace\ntooling  workspace\n");
}

#[test]
fn graph_json_reports_lifecycle_per_node() {
    let output = trellis(&fixture("lifecycle"))
        .args(["graph", "--format", "json"])
        .output()
        .unwrap();
    let graph: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let nodes = graph["nodes"].as_array().unwrap();
    let adapter = nodes.iter().find(|n| n["name"] == "adapter").unwrap();
    assert_eq!(adapter["lifecycle"], "git_only");
}

#[test]
fn info_reports_lifecycle_in_text_and_json() {
    trellis(&fixture("lifecycle"))
        .args(["info", "adapter"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lifecycle:  git_only"));

    let output = trellis(&fixture("lifecycle"))
        .args(["info", "adapter", "--json"])
        .output()
        .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["lifecycle"], "git_only");
}

#[test]
fn doctor_json_reports_package_lifecycles_alongside_the_numeric_count() {
    let output = trellis(&fixture("lifecycle"))
        .args(["doctor", "--format", "json"])
        .output()
        .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // The numeric field is retained...
    assert_eq!(document["packages"], 4);
    // ...and the additive per-package records sit alongside it.
    let records = document["package_lifecycles"].as_array().unwrap();
    assert_eq!(records.len(), 4);
    let lifecycle_of = |name: &str| {
        records
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("no record for {name}"))["lifecycle"]
            .clone()
    };
    assert_eq!(lifecycle_of("core"), "hex");
    assert_eq!(lifecycle_of("adapter"), "git_only");
    assert_eq!(lifecycle_of("tooling"), "workspace");
    assert_eq!(lifecycle_of("demo"), "workspace");
}

// ---- the lifecycle matrix across commands ------------------------------

#[test]
fn tag_plan_includes_git_only_and_hex_but_not_workspace() {
    trellis(&fixture("lifecycle"))
        .args(["tag", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "core: 1.0.0 needs tag core-v1.0.0",
        ))
        .stdout(predicate::str::contains(
            "adapter: 0.4.0 needs tag adapter-v0.4.0",
        ))
        .stdout(predicate::str::contains("tooling").not())
        .stdout(predicate::str::contains("demo").not());
}

#[test]
fn ci_outputs_releasable_is_git_only_and_hex_only() {
    let output = trellis(&fixture("lifecycle"))
        .args(["ci", "outputs"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let releasable = stdout
        .lines()
        .find_map(|line| line.strip_prefix("releasable="))
        .unwrap();
    let releasable: Vec<String> = serde_json::from_str(releasable).unwrap();
    assert_eq!(releasable, ["core", "adapter"]);
    let tags = stdout
        .lines()
        .find_map(|line| line.strip_prefix("tags="))
        .unwrap();
    let tags: Vec<String> = serde_json::from_str(tags).unwrap();
    assert_eq!(tags, ["core-v1.0.0", "adapter-v0.4.0"]);
}

#[test]
fn changelog_new_rejects_a_workspace_lifecycle_package() {
    // Runs against the shared fixture directly: the command fails validation
    // before it would write anything.
    trellis(&fixture("lifecycle"))
        .args([
            "changelog",
            "new",
            "--package",
            "tooling",
            "--kind",
            "Added",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "package `tooling` has release lifecycle `workspace`, so it never gets a \
             changelog entry",
        ));
}

#[test]
fn version_plan_rejects_a_workspace_lifecycle_package_by_name() {
    trellis(&fixture("lifecycle"))
        .args(["version", "plan", "--bump", "demo=major"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--bump/--set names `demo`, whose release lifecycle is `workspace`",
        ));
}

#[test]
fn publish_rejects_a_git_only_package_by_name() {
    trellis(&fixture("lifecycle"))
        .args(["publish", "adapter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "package `adapter` has release lifecycle `git_only`, not `hex`",
        ));
}

#[test]
fn publish_all_untagged_selects_hex_packages_only() {
    trellis(&fixture("lifecycle"))
        .args(["publish", "--all-untagged", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[core] would publish 1.0.0"))
        .stdout(predicate::str::contains("adapter").not())
        .stdout(predicate::str::contains("tooling").not())
        .stdout(predicate::str::contains("demo").not());
}

// ---- configuration: parsing, precedence, and conflicts -----------------

#[test]
fn explicit_rule_overrides_the_legacy_release_exclude_mapping() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\
         exclude = { \"@release\" = [\"packages/a\"] }\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/a\" = \"git_only\" }\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    let output = trellis(root).args(["list", "--json"]).output().unwrap();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = document["packages"].as_array().unwrap();
    // Legacy alone would resolve `a` to `workspace`; the explicit rule wins.
    assert_eq!(packages[0]["lifecycle"], "git_only");
}

#[test]
fn conflicting_explicit_lifecycle_rules_are_a_doctor_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/a\" = \"git_only\", \"packages/*\" = \"workspace\" }\n",
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
            "matches `publish.lifecycle.packages` globs for conflicting lifecycles",
        ))
        .stdout(predicate::str::contains("`git_only`"))
        .stdout(predicate::str::contains("`workspace`"));
}

#[test]
fn overlapping_rules_agreeing_on_the_same_lifecycle_do_not_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/a\" = \"git_only\", \"packages/*\" = \"git_only\" }\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: 1 package(s) (0 workspace, 1 git_only, 0 hex)",
        ));
}

#[test]
fn an_unknown_lifecycle_value_is_a_clear_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         default = \"published\"\n",
    );
    write(
        &root.join("packages/a/gleam.toml"),
        "name = \"a\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("published"));
}

#[test]
fn an_unmatched_lifecycle_glob_is_flagged_by_doctor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/nomatch\" = \"git_only\" }\n",
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
            "`publish.lifecycle.packages` glob `packages/nomatch` matches no member",
        ));
}

// ---- the dependency-availability rule -----------------------------------

#[test]
fn a_hex_package_depending_on_a_git_only_package_is_a_doctor_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/lib\" = \"git_only\" }\n",
    );
    write(
        &root.join("packages/app/gleam.toml"),
        "name = \"app\"\nversion = \"1.0.0\"\n[dependencies]\nlib = { path = \"../lib\" }\n",
    );
    write(
        &root.join("packages/lib/gleam.toml"),
        "name = \"lib\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "package `app` (lifecycle `hex`) path-depends on `lib` (lifecycle `git_only`), \
             which is unavailable in `app`'s distribution",
        ));
}

#[test]
fn a_git_only_package_depending_on_a_workspace_package_is_a_doctor_error() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/app\" = \"git_only\", \"packages/lib\" = \"workspace\" }\n",
    );
    write(
        &root.join("packages/app/gleam.toml"),
        "name = \"app\"\nversion = \"1.0.0\"\n[dependencies]\nlib = { path = \"../lib\" }\n",
    );
    write(
        &root.join("packages/lib/gleam.toml"),
        "name = \"lib\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "package `app` (lifecycle `git_only`) path-depends on `lib` (lifecycle `workspace`)",
        ));
}

#[test]
fn a_workspace_package_may_depend_on_a_hex_package() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/app\" = \"workspace\" }\n",
    );
    write(
        &root.join("packages/app/gleam.toml"),
        "name = \"app\"\nversion = \"1.0.0\"\n[dependencies]\nlib = { path = \"../lib\" }\n",
    );
    write(
        &root.join("packages/lib/gleam.toml"),
        "name = \"lib\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: 2 package(s) (1 workspace, 0 git_only, 1 hex)",
        ));
}

#[test]
fn a_dev_only_path_dep_is_exempt_from_the_availability_rule() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\"]\n\n\
         [tools.trellis.publish.lifecycle]\n\
         packages = { \"packages/lib\" = \"workspace\" }\n",
    );
    // `app` is `hex` (the default) and only *dev*-depends on the workspace-only
    // `lib` — a test helper, say. That never ships in `app`'s distribution.
    write(
        &root.join("packages/app/gleam.toml"),
        "name = \"app\"\nversion = \"1.0.0\"\n[dev-dependencies]\nlib = { path = \"../lib\" }\n",
    );
    write(
        &root.join("packages/lib/gleam.toml"),
        "name = \"lib\"\nversion = \"1.0.0\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: 2 package(s) (1 workspace, 0 git_only, 1 hex)",
        ));
}
