//! Snapshot tests over every `--json` payload trellis emits.
//!
//! These exist so that a breaking change to a documented shape fails *here*
//! rather than in a consumer's workflow. They assert the wire format and
//! nothing else — behavior is covered by `cli.rs`, `phase2.rs`, and
//! `phase3.rs`.
//!
//! A failing snapshot is not automatically a bug: adding a field is permitted
//! by the contract. Renaming, removing, or retyping one is not, and needs the
//! `schema` identifier bumped along with it. See
//! `website/src/content/docs/docs/json-output.mdx`.

use assert_cmd::Command;
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

/// Run a command and parse its stdout as JSON. Takes the expected exit status
/// because `changelog check` reports failure through it while still emitting a
/// well-formed payload.
fn json_output(dir: &Path, args: &[&str], expect_success: bool) -> serde_json::Value {
    let output = trellis(dir).args(args).output().unwrap();
    assert_eq!(
        output.status.success(),
        expect_success,
        "unexpected exit for {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("{args:?} did not emit JSON: {err}"))
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

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
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

// ---- introspection -------------------------------------------------------

#[test]
fn list_json_contract() {
    insta::assert_json_snapshot!(json_output(&fixture("basic"), &["list", "--json"], true));
}

#[test]
fn info_json_contract() {
    insta::assert_json_snapshot!(json_output(
        &fixture("basic"),
        &["info", "lat_mid", "--json"],
        true
    ));
}

#[test]
fn graph_json_contract() {
    insta::assert_json_snapshot!(json_output(
        &fixture("basic"),
        &["graph", "--format", "json"],
        true
    ));
}

// ---- changelog & versioning ---------------------------------------------

#[test]
fn changelog_check_json_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);
    git(root, &["checkout", "-q", "-b", "feature"]);
    // One releasable package with a fragment, one without, so the payload
    // exercises both `has_entry` states and a non-empty `preview`.
    write(&root.join("packages/lat_core/src/new.gleam"), "// x\n");
    write(&root.join("packages/lat_mid/src/new.gleam"), "// x\n");
    add_fragment(root, "lat_core", "Added", "something");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);

    let payload = json_output(
        root,
        &["changelog", "check", "--base", "main", "--json"],
        false,
    );
    // `preview` is contractual as a field, not as prose — it is Markdown for a
    // PR comment and free to change. Pin its presence and type, not its text.
    assert!(payload["preview"].is_string());
    insta::assert_json_snapshot!(payload, { ".preview" => "[markdown]" });
}

#[test]
fn version_plan_json_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "minor-level change");
    add_fragment(root, "lat_mid", "Breaking", "major-level change");

    insta::assert_json_snapshot!(json_output(root, &["version", "plan", "--json"], true));
}

#[test]
fn version_apply_json_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    add_fragment(root, "lat_core", "Added", "minor-level change");

    insta::assert_json_snapshot!(json_output(root, &["version", "apply", "--json"], true));
}

/// The empty plan returns early through a separate branch, so it gets its own
/// snapshot — it is the payload a no-op release job actually sees.
#[test]
fn version_apply_json_contract_when_nothing_to_do() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    insta::assert_json_snapshot!(json_output(root, &["version", "apply", "--json"], true));
}

// ---- tagging -------------------------------------------------------------

#[test]
fn tag_plan_json_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    let manifest = root.join("gleam.toml");
    let mut config = fs::read_to_string(&manifest).unwrap();
    // Bare keys, no header: `[tools.trellis.publish]` is the fixture's last
    // table, and repeating the header would be a TOML duplicate-table error.
    config.push_str(
        "repository_tag_package = \"lat_cli\"\n\
         repository_tag_format = \"repo-v{series}\"\n\
         repository_tags = [\"minor\"]\n",
    );
    fs::write(manifest, config).unwrap();
    init_repo(root);
    // lat_core is already tagged, so it drops out of the plan.
    git(root, &["tag", "-a", "lat_core-v1.2.0", "-m", "existing"]);

    insta::assert_json_snapshot!(json_output(root, &["tag", "plan", "--json"], true));
}

// ---- ci ------------------------------------------------------------------

#[test]
fn ci_matrix_contract() {
    let matrix = json_output(&fixture("basic"), &["ci", "matrix"], true);
    // `ci matrix` carries no `schema` key: GitHub Actions treats every
    // top-level key beside `include` as another matrix axis, so adding one
    // would multiply the job matrix.
    assert!(
        matrix.get("schema").is_none(),
        "ci matrix must stay a bare GitHub matrix object"
    );
    assert_eq!(matrix.as_object().unwrap().len(), 1);
    insta::assert_json_snapshot!(matrix);
}

#[test]
fn ci_outputs_contract() {
    let output = trellis(&fixture("basic"))
        .args(["ci", "outputs"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Not a JSON document — `key=<json>` lines for `$GITHUB_OUTPUT`. The
    // contract is the set of keys and each value's element type.
    let pairs: Vec<(&str, serde_json::Value)> = stdout
        .lines()
        .map(|line| {
            let (key, value) = line.split_once('=').expect("every line is key=value");
            (key, serde_json::from_str(value).expect("value is JSON"))
        })
        .collect();
    let keys: Vec<&str> = pairs.iter().map(|(key, _)| *key).collect();
    // `projects` is the deprecated alias of `packages`, emitted with an
    // identical value until 1.0 so existing workflows keep resolving.
    assert_eq!(
        keys,
        [
            "packages",
            "projects",
            "releasable",
            "version_files",
            "tags",
            "series_tags"
        ]
    );
    let by_key: std::collections::HashMap<_, _> = pairs.iter().cloned().collect();
    assert_eq!(by_key["packages"], by_key["projects"]);
    insta::assert_json_snapshot!(serde_json::Value::Object(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    ));
}

#[test]
fn ci_tag_package_json_contract_for_an_exact_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    init_repo(root);

    insta::assert_json_snapshot!(json_output(
        root,
        &["ci", "tag-package", "lat_mid-v0.5.0", "--json"],
        true,
    ));
}

/// A series tag resolves to `tag_series` instead of `tag_version`, so the two
/// variants serialize different key sets and both need pinning.
#[test]
fn ci_tag_package_json_contract_for_a_series_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\n\
         exclude = { \"@release\" = [\"examples/*\"] }\n\n\
         [tools.trellis.publish]\n\
         package_tags_overrides = { \"packages/lat_cli\" = [\"exact\", \"minor\"] }\n",
    );
    init_repo(root);

    insta::assert_json_snapshot!(json_output(
        root,
        &["ci", "tag-package", "lat_cli-v0.3", "--json"],
        true,
    ));
}

// ---- doctor --------------------------------------------------------------

/// The passing shape: `ok` is true because warnings are advisory, so this also
/// pins that a warning does not fail the run.
#[test]
fn doctor_json_contract_when_passing() {
    insta::assert_json_snapshot!(json_output(
        &fixture("basic"),
        &["doctor", "--format", "json"],
        true
    ));
}

/// The interesting payload is the failing one: it is what a PR workflow reads.
/// The fixture is broken four ways, to pin an error with a file and no package,
/// a fixable error, a fixable warning, and a finding attributed to a file
/// outside any member.
#[test]
fn doctor_json_contract_with_findings() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_fixture_to(root);

    // An exclusion glob that matches nothing: an error with no package.
    write(
        &root.join("gleam.toml"),
        "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\n\
         exclude = { \"@release\" = [\"examples/*\"], build = [\"nope/*\"] }\n",
    );
    // Stale locked version: a fixable error naming a manifest.
    write(
        &root.join("packages/lat_mid/manifest.toml"),
        "packages = [\n  \
         { name = \"lat_core\", version = \"1.1.0\", source = \"local\", path = \"../lat_core\" },\n]\n",
    );
    // Missing changelog: a fixable warning.
    fs::remove_file(root.join("packages/lat_core/CHANGELOG.md")).unwrap();
    // A fragment naming no member: an error pointing at the fragment file.
    add_fragment(root, "ghost", "Added", "from nowhere");

    let payload = json_output(root, &["doctor", "--format", "json"], false);
    insta::assert_json_snapshot!(payload);
}

// ---- run / exec ----------------------------------------------------------

/// `duration_ms` is contractual as a field, not as a value — it is wall-clock
/// and differs on every run. Pin its presence and type, not its number.
const DURATION: &str = "[ms]";

#[test]
fn run_json_contract() {
    let payload = json_output(&fixture("basic"), &["run", "hello", "--json"], true);
    for result in payload["results"].as_array().unwrap() {
        assert!(result["duration_ms"].is_u64());
    }
    insta::assert_json_snapshot!(payload, { r#".results[]["duration_ms"]"# => DURATION });
}

/// The failing shape is the one a workflow reads: it carries the exit code and
/// the command, and pins that a skipped package reports neither.
#[test]
fn exec_json_contract_with_a_failure() {
    let payload = json_output(
        &fixture("basic"),
        &["exec", "--serial", "--json", "--", "sh", "-c", "exit 3"],
        false,
    );
    insta::assert_json_snapshot!(payload, { r#".results[]["duration_ms"]"# => DURATION });
}
