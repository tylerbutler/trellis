//! End-to-end tests for the native changelog/version engine: fragments,
//! check, plan, apply, and template rendering. No external changie binary —
//! trellis is the engine.

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
    // Deterministic dates in rendered changelogs: 2026-07-11.
    cmd.env("SOURCE_DATE_EPOCH", "1783728000");
    cmd
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn copy_fixture_to(root: &Path) {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let from = fixture("basic");
    let mut files = Vec::new();
    walk(&from, &mut files);
    for file in files {
        let dest = root.join(file.strip_prefix(&from).unwrap());
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&file, &dest).unwrap();
    }
}

fn add_fragment(root: &Path, project: &str, kind: &str, body: &str) {
    let dir = root.join(".changes/unreleased");
    fs::create_dir_all(&dir).unwrap();
    for n in 1u32.. {
        let path = dir.join(format!("{project}-{n}.toml"));
        if !path.exists() {
            write(
                &path,
                &format!("project = \"{project}\"\nkind = \"{kind}\"\nbody = \"{body}\"\n"),
            );
            return;
        }
    }
}

fn git(root: &Path, args: &[&str]) {
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
}

/// Commit the fixture on `main`, branch, then touch two releasable packages and
/// give only `lat_core` a fragment. Every strictness and `--format github` case
/// wants the same shape: one package satisfied, one (`lat_mid`) missing.
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

/// Append a `[tools.trellis.changelog]` table to the fixture's root manifest.
fn set_changelog_config(root: &Path, body: &str) {
    let path = root.join("gleam.toml");
    let existing = fs::read_to_string(&path).unwrap();
    write(
        &path,
        &format!("{existing}\n[tools.trellis.changelog]\n{body}\n"),
    );
}

/// Parse `key=value` / `key<<DELIM` heredoc lines the way GitHub Actions does,
/// so a test asserts on what the runner would actually put in `outputs`.
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
            ".changes/unreleased/lat_core-1.toml",
        ));
    let fragment = fs::read_to_string(root.join(".changes/unreleased/lat_core-1.toml")).unwrap();
    assert_eq!(
        fragment,
        "package = \"lat_core\"\nkind = \"Added\"\nbody = \"grow more vines\"\n"
    );

    // A second fragment gets the next free name.
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
        .stdout(predicate::str::contains("lat_core-2.toml"));

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
    let fragment = fs::read_to_string(root.join(".changes/unreleased/lat_core-1.toml")).unwrap();
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
    let fragment = fs::read_to_string(root.join(".changes/unreleased/lat_core-2.toml")).unwrap();
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

// ---- version plan / apply ------------------------------------------------

#[test]
fn version_plan_bumps_by_the_largest_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "patch-level change");
    add_fragment(root, "lat_core", "Added", "minor-level change");
    add_fragment(root, "lat_mid", "Breaking", "major-level change");

    let output = trellis(root)
        .args(["version", "plan", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // lat_cli owns no fragment. lat_core's minor bump is inside its requirement
    // and ripples; lat_mid's major bump is outside and does not.
    assert_eq!(
        plan,
        serde_json::json!({
            "schema": "trellis.version_plan/1",
            "bumped": [
                {"name": "lat_core", "current": "1.2.0", "next": "1.3.0", "fragments": 2,
                 "updated_dependencies": []},
                {"name": "lat_mid", "current": "0.5.0", "next": "1.0.0", "fragments": 1,
                 "updated_dependencies": [{"name": "lat_core", "version": "1.3.0"}]},
                {"name": "lat_cli", "current": "0.3.1", "next": "0.3.2", "fragments": 0,
                 "updated_dependencies": [
                     {"name": "lat_core", "version": "1.3.0"},
                 ]},
            ],
            // An ordinary release retires its fragments.
            "fragments_retained": false,
        })
    );
}

#[test]
fn major_bump_outside_path_dep_requirement_does_not_ripple() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Breaking", "major-level change");

    let output = trellis(root)
        .args(["version", "plan", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        plan["bumped"],
        serde_json::json!([
            {
                "name": "lat_core",
                "current": "1.2.0",
                "next": "2.0.0",
                "fragments": 1,
                "updated_dependencies": [],
            },
        ])
    );
}

/// A dependency's own bump wins over the patch a ripple would apply, and a
/// ripple never lowers it.
#[test]
fn own_fragment_bump_wins_over_ripple() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Breaking", "major-level change");
    add_fragment(root, "lat_mid", "Added", "minor-level change");

    let output = trellis(root)
        .args(["version", "plan", "--json"])
        .output()
        .unwrap();
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let bumped = &plan["bumped"];
    assert_eq!(bumped[0]["name"], "lat_core");
    assert_eq!(bumped[0]["next"], "2.0.0");
    // Minor from its own fragment, not the 0.5.1 a bare ripple would give.
    assert_eq!(bumped[1]["name"], "lat_mid");
    assert_eq!(bumped[1]["next"], "0.6.0");
    assert_eq!(bumped[2]["name"], "lat_cli");
    assert_eq!(bumped[2]["next"], "0.3.2");
}

/// `examples/package-a` is `@release`-excluded, so it never bumps even though
/// it path-depends on `lat_cli`, which does.
#[test]
fn ripple_skips_unreleasable_members() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "patch-level change");

    let output = trellis(root)
        .args(["version", "plan", "--json"])
        .output()
        .unwrap();
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let names: Vec<&str> = plan["bumped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["lat_core", "lat_mid", "lat_cli"]);
}

/// The graph does not distinguish dev-dependency edges, and neither does the
/// ripple: `lat_cli` dev-depends on `lat_core` and runtime-depends on `lat_mid`.
#[test]
fn ripple_reaches_dev_dependents() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "patch-level change");

    let output = trellis(root)
        .args(["version", "plan", "--json"])
        .output()
        .unwrap();
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let cli = plan["bumped"].as_array().unwrap().last().unwrap();
    assert_eq!(cli["name"], "lat_cli");
    assert_eq!(
        cli["updated_dependencies"],
        serde_json::json!([
            {"name": "lat_core", "version": "1.2.1"},
            {"name": "lat_mid", "version": "0.5.1"},
        ])
    );
}

/// The human-readable plan says *why* each package is being released.
#[test]
fn version_plan_text_explains_ripples() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "a feature");
    add_fragment(root, "lat_mid", "Fixed", "a bug");

    trellis(root)
        .args(["version", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "lat_core: 1.2.0 -> 1.3.0 (1 fragment(s))",
        ))
        .stdout(predicate::str::contains(
            "lat_mid: 0.5.0 -> 0.5.1 (1 fragment(s), dependencies: lat_core)",
        ))
        .stdout(predicate::str::contains(
            "lat_cli: 0.3.1 -> 0.3.2 (dependencies: lat_core, lat_mid)",
        ));
}

#[test]
fn version_plan_is_empty_without_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    copy_fixture_to(tmp.path());
    trellis(tmp.path())
        .args(["version", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to bump"));
}

#[test]
fn version_apply_batches_renders_bumps_and_patches_lockfiles() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "grow more vines");
    add_fragment(root, "lat_core", "Fixed", "repair the trellis");

    let output = trellis(root)
        .args(["version", "apply", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["bumped"][0]["name"], "lat_core");
    assert_eq!(payload["bumped"][0]["next"], "1.3.0");
    assert_eq!(
        payload["lockfiles"],
        serde_json::json!(["packages/lat_mid/manifest.toml"])
    );

    // gleam.toml was bumped surgically: version changed, the rest untouched.
    let manifest = fs::read_to_string(root.join("packages/lat_core/gleam.toml")).unwrap();
    assert!(manifest.contains("version = \"1.3.0\""));
    assert!(manifest.contains("licences = [\"MIT\"]"));
    // The version section was batched…
    let section = fs::read_to_string(root.join(".changes/lat_core/v1.3.0.md")).unwrap();
    assert_eq!(
        section,
        "## v1.3.0 - 2026-07-11\n\n### Added\n\n- grow more vines\n\n### Fixed\n\n- repair the trellis\n"
    );
    // …the CHANGELOG was reassembled from header + sections…
    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert!(changelog.starts_with("# lat_core changelog\n"));
    assert!(changelog.contains("## v1.3.0 - 2026-07-11"));
    assert!(changelog.contains("- grow more vines"));
    // …fragments were consumed, and the dependent's lockfile patched.
    assert_eq!(
        fs::read_dir(root.join(".changes/unreleased"))
            .unwrap()
            .count(),
        0
    );
    let lock = fs::read_to_string(root.join("packages/lat_mid/manifest.toml")).unwrap();
    assert!(lock.contains("{ name = \"lat_core\", version = \"1.3.0\""));
    assert!(lock.contains("# This file was generated by Gleam"));

    // Everything is consistent afterwards…
    trellis(root).arg("doctor").assert().success();
    // …and re-running apply is a no-op.
    trellis(root)
        .args(["version", "apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to apply"));
}

/// The ripple's whole point: a dependent's version and the requirement it will
/// publish move together, and the changelog says why it moved.
#[test]
fn version_apply_writes_dependency_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "grow more vines");

    trellis(root).args(["version", "apply"]).assert().success();

    // The direct dependent bumped by a patch and records the ripple.
    assert!(
        fs::read_to_string(root.join("packages/lat_mid/gleam.toml"))
            .unwrap()
            .contains("version = \"0.5.1\"")
    );
    let section = fs::read_to_string(root.join(".changes/lat_mid/v0.5.1.md")).unwrap();
    assert_eq!(
        section,
        "## v0.5.1 - 2026-07-11\n\n### Dependencies\n\n- Updated lat_core to 1.3.0\n"
    );

    // …and so did the transitive one, naming both of its deps.
    assert!(
        fs::read_to_string(root.join("packages/lat_cli/gleam.toml"))
            .unwrap()
            .contains("version = \"0.3.2\"")
    );
    let section = fs::read_to_string(root.join(".changes/lat_cli/v0.3.2.md")).unwrap();
    assert_eq!(
        section,
        "## v0.3.2 - 2026-07-11\n\n### Dependencies\n\n- Updated lat_core to 1.3.0\n- Updated lat_mid to 0.5.1\n"
    );

    // Generated fragments are never written to disk, so nothing is left to
    // consume and a second apply is a no-op.
    assert_eq!(
        fs::read_dir(root.join(".changes/unreleased"))
            .unwrap()
            .count(),
        0
    );
    trellis(root).arg("doctor").assert().success();
    trellis(root)
        .args(["version", "apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to apply"));
}

/// A hand-written entry of the dependency kind shares one heading with the
/// generated ones rather than producing a second section.
#[test]
fn hand_written_dependency_entries_merge_with_generated_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "grow more vines");
    add_fragment(root, "lat_mid", "Dependencies", "dropped an unused hex dep");

    trellis(root).args(["version", "apply"]).assert().success();

    let section = fs::read_to_string(root.join(".changes/lat_mid/v0.5.1.md")).unwrap();
    assert_eq!(
        section,
        "## v0.5.1 - 2026-07-11\n\n### Dependencies\n\n- dropped an unused hex dep\n- Updated lat_core to 1.3.0\n"
    );
}

// ---- changelog adoption ---------------------------------------------------

/// CHANGELOG.md is regenerated from `.changes/<pkg>/`, so a package's first
/// release under trellis has to capture whatever history it already had.
#[test]
fn version_apply_adopts_existing_changelog_history() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "grow more vines");

    let output = trellis(root)
        .args(["version", "apply", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // Every package this release touches keeps its history, in topological
    // order — including the two that only bumped because lat_core did.
    assert_eq!(
        payload["adopted"],
        serde_json::json!([
            ".changes/lat_core/v1.2.0.md",
            ".changes/lat_mid/v0.5.0.md",
            ".changes/lat_cli/v0.3.1.md",
        ])
    );

    // The old body is preserved byte-for-byte, minus the header line.
    let adopted = fs::read_to_string(root.join(".changes/lat_core/v1.2.0.md")).unwrap();
    assert_eq!(adopted, "## lat_core-v1.2.0 - 2026-06-01\n\n- initial\n");

    // …and the regenerated changelog carries the new section above it.
    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert_eq!(
        changelog,
        "# lat_core changelog\n\
         \n## v1.3.0 - 2026-07-11\n\n### Added\n\n- grow more vines\n\
         \n## lat_core-v1.2.0 - 2026-06-01\n\n- initial\n"
    );
}

/// Adoption happens once. After the first release CHANGELOG.md is fully
/// generated, so a second apply must not capture it again.
#[test]
fn changelog_adoption_happens_only_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    add_fragment(root, "lat_core", "Fixed", "first release");
    trellis(root).args(["version", "apply"]).assert().success();
    add_fragment(root, "lat_core", "Added", "second release");
    let output = trellis(root)
        .args(["version", "apply", "--json"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["adopted"], serde_json::json!([]));

    let sections: Vec<String> = fs::read_dir(root.join(".changes/lat_core"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        sections.len(),
        3,
        "one adopted + two released: {sections:?}"
    );
    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert_eq!(changelog.matches("- initial").count(), 1, "{changelog}");
}

/// The `trellis new` / `doctor --fix` stub is a header and nothing else, so
/// there is no history to adopt.
#[test]
fn header_only_changelog_is_not_adopted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    write(
        &root.join("packages/lat_mid/CHANGELOG.md"),
        "# lat_mid changelog\n",
    );
    add_fragment(root, "lat_mid", "Fixed", "a bug");

    let output = trellis(root)
        .args(["version", "apply", "--json"])
        .output()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let adopted = payload["adopted"].as_array().unwrap();
    assert!(
        !adopted
            .iter()
            .any(|f| f.as_str().unwrap().contains("lat_mid")),
        "{adopted:?}"
    );
}

/// With no parseable `## ` heading there is nothing to date the history by, so
/// it is filed under the version the package is being released *from*.
#[test]
fn changelog_without_a_parseable_heading_is_dated_by_current_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    write(
        &root.join("packages/lat_mid/CHANGELOG.md"),
        "# lat_mid changelog\n\nSee the git log for changes before 0.5.0.\n",
    );
    add_fragment(root, "lat_mid", "Fixed", "a bug");

    trellis(root).args(["version", "apply"]).assert().success();

    let adopted = fs::read_to_string(root.join(".changes/lat_mid/v0.5.0.md")).unwrap();
    assert_eq!(adopted, "See the git log for changes before 0.5.0.\n");
    let changelog = fs::read_to_string(root.join("packages/lat_mid/CHANGELOG.md")).unwrap();
    assert!(changelog.contains("## v0.5.1 - 2026-07-11"), "{changelog}");
    assert!(changelog.contains("See the git log"), "{changelog}");
}

/// The ripple makes adoption matter for packages that never had a fragment of
/// their own — before this, releasing lat_core would wipe lat_cli's history.
#[test]
fn rippled_packages_keep_their_changelog_history() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "grow more vines");

    trellis(root).args(["version", "apply"]).assert().success();

    let changelog = fs::read_to_string(root.join("packages/lat_cli/CHANGELOG.md")).unwrap();
    assert!(changelog.contains("## v0.3.2 - 2026-07-11"), "{changelog}");
    assert!(
        changelog.contains("## [0.3.1]"),
        "history kept:\n{changelog}"
    );
}

#[test]
fn version_apply_preflights_all_manifests_before_consuming_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "core change");
    add_fragment(root, "lat_mid", "Fixed", "mid change");

    let core_manifest_path = root.join("packages/lat_core/gleam.toml");
    let core_manifest = fs::read_to_string(&core_manifest_path).unwrap();
    let mid_manifest_path = root.join("packages/lat_mid/gleam.toml");
    let mid_manifest = fs::read_to_string(&mid_manifest_path).unwrap();
    let mid_without_version = mid_manifest
        .lines()
        .filter(|line| !line.starts_with("version = "))
        .collect::<Vec<_>>()
        .join("\n");
    write(&mid_manifest_path, &format!("{mid_without_version}\n"));

    trellis(root)
        .args(["version", "apply"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no version field"));

    assert!(root.join(".changes/unreleased/lat_core-1.toml").is_file());
    assert!(root.join(".changes/unreleased/lat_mid-1.toml").is_file());
    assert_eq!(
        fs::read_to_string(core_manifest_path).unwrap(),
        core_manifest
    );
}

#[test]
fn version_apply_preflights_all_changelog_merges_before_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "core change");
    add_fragment(root, "lat_mid", "Fixed", "mid change");
    write(
        &root.join(".changes/lat_mid/not-semver.md"),
        "invalid stored section\n",
    );

    let core_manifest_path = root.join("packages/lat_core/gleam.toml");
    let core_manifest = fs::read_to_string(&core_manifest_path).unwrap();

    trellis(root)
        .args(["version", "apply"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not named v<semver>.md"));

    assert!(root.join(".changes/unreleased/lat_core-1.toml").is_file());
    assert!(root.join(".changes/unreleased/lat_mid-1.toml").is_file());
    assert_eq!(
        fs::read_to_string(core_manifest_path).unwrap(),
        core_manifest
    );
}

#[test]
fn version_apply_accumulates_sections_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    add_fragment(root, "lat_core", "Fixed", "first release");
    trellis(root).args(["version", "apply"]).assert().success();
    add_fragment(root, "lat_core", "Added", "second release");
    trellis(root).args(["version", "apply"]).assert().success();

    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    let newer = changelog.find("## v1.3.0").expect("second release section");
    let older = changelog.find("## v1.2.1").expect("first release section");
    assert!(newer < older, "newest section first:\n{changelog}");
}

#[test]
fn custom_minijinja_templates_shape_the_output() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let config = fs::read_to_string(root.join("gleam.toml")).unwrap();
    write(
        &root.join("gleam.toml"),
        &format!(
            concat!(
                "{config}\n",
                "[tools.trellis.changelog]\n",
                "header_format = \"# Changes to {{{{ name }}}}\"\n",
                "version_format = \"## {{{{ tag }}}} ({{{{ date }}}})\"\n",
                "kind_format = \"**{{{{ kind | upper }}}}**\"\n",
                "change_format = \"* {{{{ body }}}}\"\n",
                // Ripple entries need a kind too; point them at the only one.
                "dependency_kind = \"Tweaked\"\n",
                "dependency_body = \"bumped {{{{ dependency }}}} to {{{{ dependency_version }}}}\"\n",
                "kinds = [{{ label = \"Tweaked\", bump = \"patch\" }}]\n",
            ),
            config = config
        ),
    );
    add_fragment(root, "lat_core", "Tweaked", "polished the finish");

    trellis(root).args(["version", "apply"]).assert().success();

    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert!(changelog.starts_with("# Changes to lat_core\n"));
    assert!(changelog.contains("## lat_core-v1.2.1 (2026-07-11)"));
    assert!(changelog.contains("**TWEAKED**"));
    assert!(changelog.contains("* polished the finish"));

    // Generated ripple entries go through the same templates.
    let mid = fs::read_to_string(root.join("packages/lat_mid/CHANGELOG.md")).unwrap();
    assert!(mid.contains("**TWEAKED**"), "{mid}");
    assert!(mid.contains("* bumped lat_core to 1.2.1"), "{mid}");
}

// ---- version overrides (--bump, --set) -------------------------------------

/// A package's version straight from its gleam.toml, so a test asserts on what
/// actually landed on disk rather than on what `apply` said it did.
fn version_of(root: &Path, package: &str) -> String {
    let manifest = fs::read_to_string(root.join("packages").join(package).join("gleam.toml"))
        .unwrap_or_else(|err| panic!("no gleam.toml for {package}: {err}"));
    manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = "))
        .unwrap_or_else(|| panic!("no version in {package}'s gleam.toml"))
        .trim_matches('"')
        .to_string()
}

/// A committed repository, for the commands that read git state.
fn init_repo(root: &Path) {
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "."],
        &["commit", "-q", "-m", "init"],
    ] {
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
    }
}

fn unreleased_fragments(root: &Path) -> usize {
    fs::read_dir(root.join(".changes/unreleased"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "toml"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn bump_overrides_the_level_the_fragments_derived() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    // Filed as Fixed — a patch — but actually breaking, and already merged.
    add_fragment(root, "lat_core", "Fixed", "actually breaking");

    trellis(root)
        .args(["version", "apply", "--bump", "major"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core: 1.2.0 -> 2.0.0"));
    assert_eq!(version_of(root, "lat_core"), "2.0.0");
}

#[test]
fn per_package_bump_wins_over_the_workspace_wide_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "x");
    add_fragment(root, "lat_mid", "Fixed", "y");

    trellis(root)
        .args([
            "version",
            "apply",
            "--bump",
            "minor",
            "--bump",
            "lat_core=major",
        ])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_core"), "2.0.0");
    assert_eq!(version_of(root, "lat_mid"), "0.6.0");
}

#[test]
fn set_pins_an_exact_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "the 1.0 feature");

    // `1.0.0-does-not-parse` would be *valid* semver — a prerelease. This is
    // not.
    trellis(root)
        .args(["version", "apply", "--set", "lat_core=1.x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not semver"));

    trellis(root)
        .args(["version", "apply", "--set", "lat_core=3.1.4"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_core"), "3.1.4");
}

#[test]
fn plan_previews_the_same_override_apply_would_use() {
    // If these could diverge, `plan` would stop being a dry run.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "x");

    trellis(root)
        .args(["version", "plan", "--bump", "major"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core: 1.2.0 -> 2.0.0"));
    // Still a dry run: nothing was written.
    assert_eq!(version_of(root, "lat_core"), "1.2.0");
}

#[test]
fn overrides_reject_names_that_are_not_releasable_members() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "x");

    trellis(root)
        .args(["version", "plan", "--set", "nope=1.0.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown package `nope`"));
    // package_a is excluded from releases by the fixture's `@release` key.
    trellis(root)
        .args(["version", "plan", "--bump", "package_a=major"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("release lifecycle is `workspace`"));
    trellis(root)
        .args([
            "version",
            "plan",
            "--bump",
            "lat_core=major",
            "--set",
            "lat_core=9.9.9",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("both --bump and --set"));
}

#[test]
fn a_backwards_override_is_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Fixed", "x");

    trellis(root)
        .args(["version", "apply", "--set", "lat_core=1.0.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not forward"));
    assert_eq!(version_of(root, "lat_core"), "1.2.0");
    assert_eq!(unreleased_fragments(root), 1);
}

// ---- prereleases (--pre) ---------------------------------------------------

#[test]
fn pre_cuts_a_release_candidate_and_keeps_the_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "the 1.0 feature");

    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core: 1.2.0 -> 1.3.0-rc.1"))
        .stdout(predicate::str::contains("kept fragments unreleased"));
    assert_eq!(version_of(root, "lat_core"), "1.3.0-rc.1");
    // The section rendered, but the fragments behind it are still pending.
    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert!(changelog.contains("1.3.0-rc.1"), "{changelog}");
    assert!(changelog.contains("the 1.0 feature"), "{changelog}");
    assert_eq!(unreleased_fragments(root), 1);
}

#[test]
fn pre_combines_with_an_exact_version() {
    // The motivating case for both flags at once: a package approaching its
    // own 1.0 wants 1.0.0-rc.1, and neither flag alone reaches it.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_mid", "Added", "the 1.0 feature");

    trellis(root)
        .args(["version", "apply", "--set", "lat_mid=1.0.0", "--pre", "rc"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_mid"), "1.0.0-rc.1");

    // The base is settled now, so repeating only advances the counter.
    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_mid"), "1.0.0-rc.2");
}

#[test]
fn repeating_pre_increments_the_candidate_within_one_base() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "the 1.0 feature");

    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_core"), "1.3.0-rc.1");

    // Same fragments, second candidate: the base must not bump again.
    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_core"), "1.3.0-rc.2");
    assert_eq!(unreleased_fragments(root), 1);
}

#[test]
fn promote_finalizes_the_candidate_and_consumes_the_fragments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "the 1.0 feature");

    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_core"), "1.3.0-rc.2");
    assert_eq!(unreleased_fragments(root), 1);

    trellis(root)
        .args(["version", "apply", "--pre", "none"])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat_core: 1.3.0-rc.2 -> 1.3.0"));
    assert_eq!(version_of(root, "lat_core"), "1.3.0");
    // Only the final release retires them.
    assert_eq!(unreleased_fragments(root), 0);
    let changelog = fs::read_to_string(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    assert!(changelog.contains("the 1.0 feature"), "{changelog}");
}

#[test]
fn a_prerelease_labels_the_whole_plan_including_ripples() {
    // One coherent release candidate of the workspace: a dependent bumped only
    // because lat_core moved carries the same label.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "the 1.0 feature");

    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_core"), "1.3.0-rc.1");
    assert_eq!(version_of(root, "lat_mid"), "0.5.1-rc.1");
    assert_eq!(version_of(root, "lat_cli"), "0.3.2-rc.1");

    trellis(root)
        .args(["version", "apply", "--pre", "none"])
        .assert()
        .success();
    assert_eq!(version_of(root, "lat_core"), "1.3.0");
    assert_eq!(version_of(root, "lat_mid"), "0.5.1");
    assert_eq!(version_of(root, "lat_cli"), "0.3.2");
}

#[test]
fn a_pending_prerelease_must_be_resolved_explicitly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "the 1.0 feature");
    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();

    // A plain apply would otherwise derive 1.4.0 and drop the cycle silently.
    trellis(root)
        .args(["version", "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--pre none"));
}

#[test]
fn promote_needs_something_to_promote() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "x");

    trellis(root)
        .args(["version", "plan", "--pre", "none"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no package in the plan is at one"));
}

#[test]
fn a_prerelease_moves_no_series_tag() {
    // `series_of` already returns None for a prerelease; this pins that the
    // tag layer agrees now that prereleases are reachable.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\n\
         exclude = { \"@release\" = [\"examples/*\"] }\n\
         [tools.trellis.publish]\ntag_mode = \"both\"\n",
    );
    add_fragment(root, "lat_core", "Added", "x");
    init_repo(root);
    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();

    let output = trellis(root)
        .args(["tag", "plan", "--json"])
        .output()
        .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let kinds: Vec<&str> = document["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tag| tag["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.iter().all(|kind| *kind == "exact"),
        "a prerelease belongs to no series: {document}"
    );
}

#[test]
fn doctor_accepts_a_prerelease_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "x");
    trellis(root)
        .args(["version", "apply", "--pre", "rc"])
        .assert()
        .success();

    // The changelog check compares gleam.toml against the newest changelog
    // heading; a prerelease must not read as "behind".
    trellis(root).arg("doctor").assert().success();
}

#[test]
fn version_apply_json_reports_whether_fragments_survived() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "x");

    let output = trellis(root)
        .args(["version", "apply", "--pre", "rc", "--json"])
        .output()
        .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["fragments_retained"], true);

    let output = trellis(root)
        .args(["version", "apply", "--pre", "none", "--json"])
        .output()
        .unwrap();
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["fragments_retained"], false);
}
