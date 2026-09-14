//! End-to-end tests for `trellis changelog`.

mod common;

use common::{add_fragment, copy_fixture_to, git, trellis_with_stable_date as trellis, write};
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn workspace_with_one_missing_fragment(root: &Path) {
    copy_fixture_to(root);
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    write(&root.join("packages/lat_mid/src/new.gleam"), "// x\n");
    add_fragment(root, "lat_core", "Added", "something");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);
}

fn set_changelog_config(root: &Path, body: &str) {
    let path = root.join("gleam.toml");
    let existing = fs::read_to_string(&path).unwrap();
    write(
        &path,
        &format!("{existing}\n[tools.trellis.changelog]\n{body}\n"),
    );
}

fn parse_github_output(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some((key, delimiter)) = line.split_once("<<") {
            let mut value = String::new();
            for line in lines.by_ref() {
                if line == delimiter {
                    break;
                }
                value.push_str(line);
                value.push('\n');
            }
            out.insert(key.to_string(), value);
        } else {
            let (key, value) = line.split_once('=').expect("key=value line");
            out.insert(key.to_string(), value.to_string());
        }
    }
    out
}

// ---- changelog new ---------------------------------------------------------

/// `project` was the pre-1.0 spelling of a fragment's `package` key, inherited
/// from changie. Fragments written by an older trellis sit in `.changes/` of
/// real workspaces, so both spellings must parse until 1.0 removes the alias.
/// (`add_fragment` above still writes `project`, which exercises the alias
/// across the rest of this suite.)
#[test]
fn fragments_parse_under_either_package_spelling() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    let dir = root.join(".changes/unreleased");
    fs::create_dir_all(&dir).unwrap();
    write(
        &dir.join("old-spelling.toml"),
        "project = \"lat_core\"\nkind = \"Added\"\nbody = \"written by an older trellis\"\n",
    );
    write(
        &dir.join("new-spelling.toml"),
        "package = \"lat_core\"\nkind = \"Added\"\nbody = \"written by this one\"\n",
    );

    trellis(root)
        .args(["version", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core"))
        .stdout(predicate::str::contains("2 fragment(s)"));
}

#[test]
fn new_fragment_writes_toml_and_validates_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Added",
            "--body",
            "grow more vines",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ".changes/unreleased/lat_core-grow-more-vines.toml",
        ));
    let fragment =
        fs::read_to_string(root.join(".changes/unreleased/lat_core-grow-more-vines.toml")).unwrap();
    assert_eq!(
        fragment,
        "package = \"lat_core\"\nkind = \"Added\"\nbody = \"grow more vines\"\n"
    );

    // A different body earns a different name, no counter involved.
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Fixed",
            "--body",
            "x",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core-x.toml"));

    // The same body twice falls back to a counter.
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Fixed",
            "--body",
            "x",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core-x-2.toml"));

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "nope",
            "--kind",
            "Added",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown package"));
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "package_a",
            "--kind",
            "Added",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("release lifecycle `workspace`"));
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Invented",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown kind `Invented`"))
        .stderr(predicate::str::contains("Added"));
    trellis(root)
        .args(["changelog", "new", "--kind", "Added", "--body", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--package is required"));
    // The fixture configures no categories, so the error names the key that
    // turns the axis on.
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Added",
            "--category",
            "build",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no `categories` are configured"));
}

// ---- categories -------------------------------------------------------------

/// The fixture plus a `categories` list, which is all it takes to switch the
/// second grouping axis on.
fn with_categories(root: &Path) {
    let config = fs::read_to_string(root.join("gleam.toml")).unwrap();
    write(
        &root.join("gleam.toml"),
        &format!("{config}\n[tools.trellis.changelog]\ncategories = [\"build\", \"publish\"]\n"),
    );
}

#[test]
fn new_fragment_records_a_category_and_validates_it() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    with_categories(root);

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Added",
            "--category",
            "build",
            "--body",
            "grow more vines",
        ])
        .assert()
        .success();
    let fragment =
        fs::read_to_string(root.join(".changes/unreleased/lat_core-grow-more-vines.toml")).unwrap();
    assert_eq!(
        fragment,
        "package = \"lat_core\"\nkind = \"Added\"\ncategory = \"build\"\nbody = \"grow more vines\"\n"
    );

    // The category stays optional even once categories are configured.
    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Fixed",
            "--body",
            "x",
        ])
        .assert()
        .success();
    let fragment = fs::read_to_string(root.join(".changes/unreleased/lat_core-x.toml")).unwrap();
    assert_eq!(
        fragment,
        "package = \"lat_core\"\nkind = \"Fixed\"\nbody = \"x\"\n"
    );

    trellis(root)
        .args([
            "changelog",
            "new",
            "--package",
            "lat_core",
            "--kind",
            "Added",
            "--category",
            "Invented",
            "--body",
            "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown category `Invented`"))
        .stderr(predicate::str::contains("build, publish"));
}

/// An unknown category is a fragment problem like an unknown kind: `doctor`
/// names the file, and `version` refuses outright rather than dropping the
/// entry on the floor at release time.
#[test]
fn an_unknown_category_is_reported_by_doctor_and_fatal_to_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    with_categories(root);
    write(
        &root.join(".changes/unreleased/lat_core-1.toml"),
        "package = \"lat_core\"\nkind = \"Added\"\ncategory = \"nope\"\nbody = \"x\"\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("category `nope` is not one of"))
        .stdout(predicate::str::contains("lat_core-1.toml"));
    trellis(root)
        .args(["version", "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("category `nope`"));
}

#[test]
fn version_apply_groups_a_section_by_category_then_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    with_categories(root);
    write(
        &root.join(".changes/unreleased/lat_core-1.toml"),
        "package = \"lat_core\"\nkind = \"Added\"\ncategory = \"publish\"\nbody = \"--dry-run\"\n",
    );
    write(
        &root.join(".changes/unreleased/lat_core-2.toml"),
        "package = \"lat_core\"\nkind = \"Added\"\ncategory = \"build\"\nbody = \"--watch\"\n",
    );
    // No category: lands under the trailing `Other` heading.
    add_fragment(root, "lat_core", "Fixed", "a general fix");

    trellis(root).args(["version", "apply"]).assert().success();

    let section = fs::read_to_string(root.join(".changes/lat_core/v1.3.0.md")).unwrap();
    assert_eq!(
        section,
        "## v1.3.0 - 2026-07-11\n\
         \n### build\n\
         \n#### Added\n\n- --watch\n\
         \n### publish\n\
         \n#### Added\n\n- --dry-run\n\
         \n### Other\n\
         \n#### Fixed\n\n- a general fix\n"
    );

    // Generated ripple entries carry no category, so lat_mid's whole section
    // is the `Other` block.
    let mid = fs::read_to_string(root.join(".changes/lat_mid/v0.5.1.md")).unwrap();
    assert_eq!(
        mid,
        "## v0.5.1 - 2026-07-11\n\
         \n### Other\n\
         \n#### Dependencies\n\n- Updated lat_core to 1.3.0\n"
    );

    // Nothing about the extra level disturbs the rest of the pipeline.
    trellis(root).arg("doctor").assert().success();
}

// ---- changelog check ---------------------------------------------------------

#[test]
fn changelog_check_maps_diff_to_missing_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    // Change two releasable packages and the example package; add a fragment for one.
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    write(&root.join("packages/lat_mid/src/new.gleam"), "// x\n");
    write(&root.join("examples/package-a/src/new.gleam"), "// x\n");
    add_fragment(root, "lat_core", "Added", "something");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "lat_mid lacks a fragment");
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["has_entries"], true);
    assert_eq!(payload["needs_entry"], true);
    let packages = payload["packages"].as_array().unwrap();
    // The example package changed too but is not releasable, so only two rows.
    assert_eq!(packages.len(), 2);
    let core = packages.iter().find(|p| p["name"] == "lat_core").unwrap();
    assert_eq!(core["has_entry"], true);
    let mid = packages.iter().find(|p| p["name"] == "lat_mid").unwrap();
    assert_eq!(mid["has_entry"], false);
    assert!(payload["preview"].as_str().unwrap().contains("lat_mid"));

    // Adding the missing fragment turns the check green.
    add_fragment(root, "lat_mid", "Fixed", "more");
    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_mid: 1 fragment(s)"));
}

#[test]
fn changelog_check_rows_a_package_the_branch_wrote_a_fragment_for() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    // Only `lat_core` has changed files. `lat_mid` is documented by a fragment
    // this branch wrote — a break that propagates to it without touching its
    // source — so it releases, and the check must say so rather than leaving it
    // to be discovered in the release preview alone.
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    add_fragment(root, "lat_core", "Added", "something");
    add_fragment(root, "lat_mid", "Fixed", "propagated");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "both packages are documented");
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = payload["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 2);
    let core = packages.iter().find(|p| p["name"] == "lat_core").unwrap();
    assert_eq!(core["changed"], true);
    assert_eq!(core["fragments"], 1);
    let mid = packages.iter().find(|p| p["name"] == "lat_mid").unwrap();
    // Rowed on the strength of its fragment, and honestly marked as untouched.
    assert_eq!(mid["changed"], false);
    assert_eq!(mid["has_entry"], true);
    assert_eq!(mid["fragments"], 1);
    // The comment's table and its release preview now agree on the package set.
    let preview = payload["preview"].as_str().unwrap();
    assert!(preview.contains("| lat_mid | ✅ 1 |"), "preview: {preview}");

    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_mid: 1 fragment(s)"));
}

// ---- changelog check: strictness ------------------------------------------

#[test]
fn strictness_warn_reports_a_missing_entry_without_failing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);
    set_changelog_config(root, "strictness = \"warn\"");

    // Reported, but advisory: the gate does not fail the job.
    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_mid: needs a changelog entry"));

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // `needs_entry` still states the fact; `ok` carries the verdict.
    assert_eq!(payload["needs_entry"], true);
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["strictness"], "warn");
}

#[test]
fn strictness_off_drops_the_verdict_but_still_reports_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);
    set_changelog_config(root, "strictness = \"off\"");

    // Nothing is being asked of the contributor, so the prose must not ask.
    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_mid: no entries"))
        .stdout(predicate::str::contains("needs a changelog entry").not());

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["needs_entry"], false);
    assert_eq!(payload["ok"], true);
    // The rows survive — `off` means "don't gate", not "don't report".
    let packages = payload["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 2);
    let mid = packages.iter().find(|p| p["name"] == "lat_mid").unwrap();
    assert_eq!(mid["has_entry"], false);
    // No call to action, since nothing is being asked of the contributor.
    assert!(
        !payload["preview"]
            .as_str()
            .unwrap()
            .contains("changelog new")
    );
}

#[test]
fn the_strictness_flag_overrides_the_configured_value() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);
    set_changelog_config(root, "strictness = \"warn\"");

    // A workflow can gate harder than the workspace's own default.
    trellis(root)
        .args([
            "changelog",
            "check",
            "--base",
            "main",
            "--strictness",
            "error",
        ])
        .assert()
        .failure();

    // ...and the other way, over a configured `error`.
    let strict = tempfile::tempdir().unwrap();
    let strict_root = strict.path();
    workspace_with_one_missing_fragment(strict_root);
    set_changelog_config(strict_root, "strictness = \"error\"");
    trellis(strict_root)
        .args([
            "changelog",
            "check",
            "--base",
            "main",
            "--strictness",
            "off",
        ])
        .assert()
        .success();
}

#[test]
fn invalid_fragments_fail_at_every_strictness() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);
    set_changelog_config(root, "strictness = \"off\"");
    write(
        &root.join(".changes/unreleased/broken-1.toml"),
        "not toml at all {{{\n",
    );

    // Strictness is a policy about missing entries. A fragment that does not
    // parse is malformed input, and no policy setting excuses it.
    trellis(root)
        .args(["changelog", "check", "--base", "main"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("broken-1.toml"));
}

// ---- changelog check: --format github --------------------------------------

#[test]
fn github_format_emits_github_output_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "github"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "lat_mid lacks a fragment");
    let text = String::from_utf8(output.stdout).unwrap();
    let outputs = parse_github_output(&text);

    assert_eq!(outputs["has_entries"], "true");
    assert_eq!(outputs["needs_entry"], "true");
    assert_eq!(outputs["ok"], "false");
    assert_eq!(outputs["strictness"], "error");
    assert_eq!(outputs["needs_entry_packages"], "[\"lat_mid\"]");
    assert_eq!(outputs["invalid_fragments"], "[]");

    // The comment body is the same markdown `--format json` reports, so the
    // two surfaces can never drift.
    let json = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        outputs["preview"].trim_end(),
        payload["preview"].as_str().unwrap().trim_end()
    );

    // Machine surface: no prose leaks into the file the runner parses.
    assert!(!text.contains("needs a changelog entry"));
}

#[test]
fn github_format_reports_a_clean_check() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);
    add_fragment(root, "lat_mid", "Fixed", "more");

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "github"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let outputs = parse_github_output(&String::from_utf8(output.stdout).unwrap());
    // The workflow's third state: entries exist, nothing is missing.
    assert_eq!(outputs["has_entries"], "true");
    assert_eq!(outputs["needs_entry"], "false");
    assert_eq!(outputs["needs_entry_packages"], "[]");
    assert_eq!(outputs["ok"], "true");
}

#[test]
fn the_preview_reports_next_versions_and_fragment_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);
    add_fragment(root, "lat_mid", "Fixed", "more");

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let preview = payload["preview"].as_str().unwrap();
    // The table carries each package's planned bump alongside its counts.
    assert!(
        preview.contains("| lat_core | ✅ 1 | 1.2.0 → 1.3.0 |"),
        "{preview}"
    );
    assert!(
        preview.contains("| lat_mid | ✅ 1 | 0.5.0 → 0.5.1 |"),
        "{preview}"
    );
    // The release preview renders the exact sections `version apply` would
    // write: fragment bodies, and generated ripple entries for dependents —
    // including lat_cli, which did not change in the diff but bumps anyway.
    assert!(preview.contains("### Release preview"), "{preview}");
    assert!(preview.contains("- something"), "{preview}");
    assert!(preview.contains("- more"), "{preview}");
    assert!(preview.contains("Updated lat_core to 1.3.0"), "{preview}");
    assert!(preview.contains("lat_cli"), "{preview}");
    // A ripple is never a demand: lat_cli did not change in the diff, so it
    // gets no table row and cannot be asked for a fragment.
    assert!(!preview.contains("| lat_cli"), "{preview}");
    assert_eq!(payload["needs_entry"], false);
}

#[test]
fn the_release_preview_ignores_fragments_already_on_the_base_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    // Unreleased on `main` already: an earlier PR's fragment, which this PR
    // neither added nor is answerable for.
    add_fragment(root, "lat_mid", "Fixed", "from an earlier pr");
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    add_fragment(root, "lat_core", "Added", "from this pr");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let preview = payload["preview"].as_str().unwrap();
    assert!(preview.contains("- from this pr"), "{preview}");
    assert!(!preview.contains("from an earlier pr"), "{preview}");
    // lat_mid still appears — this PR ripples it — but only with the entry
    // this PR is responsible for.
    assert!(preview.contains("Updated lat_core to 1.3.0"), "{preview}");
    assert!(
        preview.contains("| lat_core | ✅ 1 | 1.2.0 → 1.3.0 |"),
        "{preview}"
    );
}

/// The base branch carries an unreleased `lat_core` fragment; this PR changes
/// `lat_core` and adds nothing. The earlier PR's entry documents the earlier
/// PR, so it cannot answer for this one.
fn workspace_with_a_base_branch_fragment(root: &Path) {
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "from an earlier pr");
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);
}

#[test]
fn a_base_branch_fragment_does_not_satisfy_a_later_pr() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_a_base_branch_fragment(root);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["needs_entry"], true);
    assert_eq!(payload["ok"], false);
    // `has_entries` follows the counts: this PR wrote nothing, so the CI recipe
    // deletes the comment rather than posting an empty preview.
    assert_eq!(payload["has_entries"], false);
    assert_eq!(payload["packages"][0]["fragments"], 0);
    assert_eq!(payload["packages"][0]["has_entry"], false);
    let preview = payload["preview"].as_str().unwrap();
    assert!(
        preview.contains("| lat_core | ❌ needs an entry | — |"),
        "{preview}"
    );
    assert!(!preview.contains("### Release preview"), "{preview}");
}

#[test]
fn editing_a_base_branch_fragment_counts_as_this_prs_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_a_base_branch_fragment(root);
    // A PR that rewrites an existing entry has documented its change; asking
    // it for a second fragment would be a false alarm.
    write(
        &root.join(".changes/unreleased/lat_core-1.toml"),
        "project = \"lat_core\"\nkind = \"Added\"\nbody = \"reworded by this pr\"\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "reword"]);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["needs_entry"], false);
    let preview = payload["preview"].as_str().unwrap();
    assert!(preview.contains("- reworded by this pr"), "{preview}");
    assert!(
        preview.contains("| lat_core | ✅ 1 | 1.2.0 → 1.3.0 |"),
        "{preview}"
    );
}

#[test]
fn an_uncommitted_fragment_satisfies_the_check_before_it_is_committed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_a_base_branch_fragment(root);
    // Written but not committed: running the check locally has to give the
    // answer CI will give once it is.
    add_fragment(root, "lat_core", "Fixed", "not committed yet");

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["needs_entry"], false);
    let preview = payload["preview"].as_str().unwrap();
    assert!(preview.contains("- not committed yet"), "{preview}");
    assert!(!preview.contains("from an earlier pr"), "{preview}");
}

#[test]
fn an_invalid_base_branch_fragment_still_fails_the_check() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    // Unlike the counts, problems are not scoped: this one blocks the next
    // release whoever committed it, so every check reports it.
    write(
        &root.join(".changes/unreleased/broken-1.toml"),
        "not toml at all {{{\n",
    );
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    add_fragment(root, "lat_core", "Added", "from this pr");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);

    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["needs_entry"], false);
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["invalid_fragments"].as_array().unwrap().len(), 1);
}

#[test]
fn invalid_fragments_leave_the_preview_without_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);
    add_fragment(root, "lat_mid", "Fixed", "more");
    write(
        &root.join(".changes/unreleased/broken-1.toml"),
        "not toml at all {{{\n",
    );

    // A plan cannot be computed over a fragment that does not parse, so the
    // preview falls back to counts alone — the problem itself is reported.
    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--format", "json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let preview = payload["preview"].as_str().unwrap();
    assert!(!preview.contains("### Release preview"), "{preview}");
    assert!(preview.contains("| lat_core | ✅ 1 | — |"), "{preview}");
    assert!(preview.contains("broken-1.toml"), "{preview}");
}

#[test]
fn json_stays_an_alias_for_format_json() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace_with_one_missing_fragment(root);

    // Deprecated, but workflows in the wild pass it — it must keep working.
    let output = trellis(root)
        .args(["changelog", "check", "--base", "main", "--json"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["schema"], "trellis.changelog_check/2");
}

#[test]
fn invalid_fragments_fail_check_and_doctor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_typo", "Added", "x"); // unknown project
    add_fragment(root, "lat_core", "Invented", "x"); // unknown kind
    write(
        &root.join(".changes/unreleased/broken-1.toml"),
        "not toml at all {{{\n",
    );

    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "`lat_typo` is not a workspace member",
        ))
        .stdout(predicate::str::contains("kind `Invented` is not one of"))
        .stdout(predicate::str::contains("broken-1.toml"));

    // plan/apply refuse loudly instead of dropping fragments.
    trellis(root)
        .args(["version", "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid changelog fragment(s)"));
}
