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
        .stdout("lat_core\nlat_mid\nlat_cli\npackage_a\n");
}

#[test]
fn list_works_from_inside_a_package() {
    trellis(&fixture("basic").join("packages/lat_mid"))
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("lat_core\n"));
}

#[test]
fn list_releasable_excludes_release_excluded_members() {
    trellis(&fixture("basic"))
        .args(["list", "--releasable"])
        .assert()
        .success()
        .stdout("lat_core\nlat_mid\nlat_cli\n");
}

#[test]
fn list_json_includes_graph_facts() {
    let output = trellis(&fixture("basic"))
        .args(["list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let items: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 4);
    let mid = items.iter().find(|i| i["name"] == "lat_mid").unwrap();
    assert_eq!(mid["version"], "0.5.0");
    assert_eq!(mid["path"], "packages/lat_mid");
    assert_eq!(mid["releasable"], true);
    assert_eq!(mid["dependencies"], serde_json::json!(["lat_core"]));
    assert_eq!(mid["dependents"], serde_json::json!(["lat_cli"]));
    let package_a = items.iter().find(|i| i["name"] == "package_a").unwrap();
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

// ---- doctor ----------------------------------------------------------

#[test]
fn doctor_passes_on_healthy_workspace() {
    trellis(&fixture("basic"))
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("ok: 4 member(s)"));
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
        .stdout(predicate::str::contains("will never exist on Hex"));
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
        .stdout(predicate::str::contains("ok: 1 member(s), 0 warning(s)"));
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
        .stdout("lat_mid\n");

    trellis(root)
        .args(["list", "--since", "main", "--with-dependents"])
        .assert()
        .success()
        .stdout("lat_mid\nlat_cli\npackage_a\n");

    // Uncommitted changes count too.
    write(&root.join("packages/lat_core/src/wip.gleam"), "// wip\n");
    trellis(root)
        .args(["list", "--since", "main"])
        .assert()
        .success()
        .stdout("lat_core\nlat_mid\n");
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
