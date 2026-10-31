//! Publish-time source bundling, including checks on the exported artifact.

mod common;

use common::{
    add_fragment, git, init_repo, make_executable, trellis_with_local_http as trellis, write,
};
use predicates::prelude::*;
use std::fs;
use std::path::Path;

fn workspace(root: &Path) {
    write(
        &root.join("gleam.toml"),
        r#"[tools.trellis]
members = ["packages/*"]
[tools.trellis.publish.bundle]
lib_a = ["lib_b"]
[tools.trellis.publish.lifecycle.packages]
"packages/lib_b" = "workspace"
"packages/lib_c" = "workspace"
"#,
    );
    for name in ["lib_a", "lib_b", "lib_c"] {
        write(
            &root.join(format!("packages/{name}/gleam.toml")),
            &format!(
                "name = \"{name}\"\nversion = \"1.0.0\"\n\
                 description = \"Bundling test\"\nlicences = [\"MIT\"]\n\
                 [repository]\ntype = \"github\"\nuser = \"example\"\nrepo = \"bundle\"\n"
            ),
        );
        write(
            &root.join(format!("packages/{name}/LICENSE")),
            &format!("{name} test licence\n"),
        );
    }
    let a = root.join("packages/lib_a");
    let b = root.join("packages/lib_b");
    let manifest = fs::read_to_string(a.join("gleam.toml")).unwrap();
    write(
        &a.join("gleam.toml"),
        &format!("{manifest}\n[dependencies]\nlib_b = {{ path = \"../lib_b\" }}\n"),
    );
    let manifest = fs::read_to_string(b.join("gleam.toml")).unwrap();
    write(
        &b.join("gleam.toml"),
        &format!("{manifest}\n[dependencies]\nlib_c = {{ path = \"../lib_c\" }}\n"),
    );
    write(
        &a.join("src/lib_a.gleam"),
        "import lib_b\n\npub fn answer() -> Int {\n  lib_b.answer()\n}\n",
    );
    write(
        &a.join("test/lib_a_test.gleam"),
        "import lib_a\n\npub fn main() {\n  case lib_a.answer() {\n    42 -> Nil\n    _ -> panic as \"wrong answer\"\n  }\n}\n",
    );
    write(
        &b.join("src/lib_b.gleam"),
        "import lib_c\n\npub fn answer() -> Int {\n  native_answer() + lib_c.zero()\n}\n\n@external(erlang, \"native_helper\", \"answer\")\n@external(javascript, \"./native_helper.mjs\", \"answer\")\nfn native_answer() -> Int\n",
    );
    write(
        &b.join("src/native_helper.erl"),
        "-module(native_helper).\n-export([answer/0]).\nanswer() -> 42.\n",
    );
    write(
        &b.join("src/native_helper.mjs"),
        "import { value } from './helper.mjs';\nexport function answer() { return value; }\n",
    );
    write(&b.join("src/helper.mjs"), "export const value = 42;\n");
    write(
        &root.join("packages/lib_c/src/lib_c.gleam"),
        "pub fn zero() -> Int {\n  0\n}\n",
    );
}

fn fake_gleam(root: &Path, fail: &str) -> std::path::PathBuf {
    let script = root.join("fake-gleam.sh");
    write(
        &script,
        &format!(
            "#!/bin/sh\nset -eu\nroot='{root}'\n\
             printf '%s\\n' \"$PWD\" >> \"$root/cwd-log\"\n\
             printf '%s\\n' \"$*\" >> \"$root/gleam-log\"\n\
             if [ \"$1\" = '{fail}' ]; then exit 1; fi\n\
             if [ \"$1\" = publish ]; then\n\
               cp gleam.toml \"$root/published.toml\"\n\
               cp -R src \"$root/published-src\"\n\
             fi\n",
            root = root.display(),
        ),
    );
    make_executable(&script);
    script
}

fn publish(root: &Path, fake: &Path) -> assert_cmd::Command {
    let mut cmd = trellis(root);
    // The test Hex server already used by the shared test helpers returns 404.
    let hex = common::mock_github(root);
    cmd.env("TRELLIS_HEX_API_URL", format!("{hex}/api"))
        .env("TRELLIS_GLEAM_BIN", fake)
        .args(["publish", "lib_a"]);
    cmd
}

#[test]
fn bundle_publishes_transitive_and_native_sources_without_changing_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    trellis(root).arg("doctor").assert().success();
    let original = fs::read_to_string(root.join("packages/lib_a/gleam.toml")).unwrap();
    let fake = fake_gleam(root, "never");
    publish(root, &fake)
        .assert()
        .success()
        .stdout(predicate::str::contains("lib_b"))
        .stdout(predicate::str::contains("lib_c"));
    let published = fs::read_to_string(root.join("published.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&published).unwrap();
    for name in ["lib_b", "lib_c"] {
        assert!(doc["dependencies"].get(name).is_none());
        assert!(root.join(format!("published-src/{name}.gleam")).exists());
    }
    for name in ["native_helper.erl", "native_helper.mjs", "helper.mjs"] {
        assert!(root.join("published-src").join(name).exists());
    }
    assert_eq!(
        fs::read_to_string(root.join("packages/lib_a/gleam.toml")).unwrap(),
        original
    );
    assert!(!root.join("packages/lib_a/manifest.toml").exists());
    assert!(!root.join("packages/lib_a/src/lib_b.gleam").exists());
    let working_dirs = fs::read_to_string(root.join("cwd-log")).unwrap();
    for dir in working_dirs.lines() {
        assert!(!Path::new(dir).starts_with(root));
        assert!(
            !Path::new(dir).exists(),
            "temporary package was not removed"
        );
    }
}

#[test]
fn bundle_dry_run_checks_conflicts_without_running_gleam() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    let fake = fake_gleam(root, "never");
    publish(root, &fake)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("lib_b"));
    assert!(!root.join("gleam-log").exists());
    write(
        &root.join("packages/lib_c/src/lib_b.gleam"),
        "pub fn duplicate() { Nil }\n",
    );
    publish(root, &fake)
        .arg("--dry-run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflict"));
}

#[test]
fn bundle_rejects_differing_external_requirements_and_doctor_reports_them() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    for (name, version) in [("lib_a", "1.0.0"), ("lib_b", "2.0.0")] {
        let path = root.join(format!("packages/{name}/gleam.toml"));
        let text = fs::read_to_string(&path).unwrap();
        write(&path, &format!("{text}external = \"== {version}\"\n"));
    }
    trellis(root)
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("external"))
        .stdout(predicate::str::contains("conflict"));
    let fake = fake_gleam(root, "never");
    publish(root, &fake)
        .assert()
        .failure()
        .stderr(predicate::str::contains("external"));
    assert!(!root.join("gleam-log").exists());
}

#[test]
fn bundle_configuration_rejects_non_runtime_and_unknown_selections() {
    for selection in ["lib_c", "missing", "lib_a", "lib_b\", \"lib_b"] {
        let tmp = tempfile::tempdir().unwrap();
        workspace(tmp.path());
        let config = tmp.path().join("gleam.toml");
        let text = fs::read_to_string(&config).unwrap();
        write(
            &config,
            &text.replace("[\"lib_b\"]", &format!("[\"{selection}\"]")),
        );
        trellis(tmp.path())
            .arg("doctor")
            .assert()
            .failure()
            .stdout(predicate::str::contains("bundle"));
    }
}

#[test]
fn bundle_validation_and_publish_failures_remove_staging_directory() {
    for step in ["build", "publish"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        workspace(root);
        let config = root.join("gleam.toml");
        let text = fs::read_to_string(&config).unwrap();
        write(
            &config,
            &text.replace(
                "[tools.trellis.publish.bundle]",
                "[tools.trellis.publish.retry]\nattempts = 1\n[tools.trellis.publish.bundle]",
            ),
        );
        let original = fs::read_to_string(root.join("packages/lib_a/gleam.toml")).unwrap();
        let fake = fake_gleam(root, step);
        publish(root, &fake).assert().failure();
        assert!(!root.join("published.toml").exists());
        assert_eq!(
            fs::read_to_string(root.join("packages/lib_a/gleam.toml")).unwrap(),
            original
        );
        for dir in fs::read_to_string(root.join("cwd-log")).unwrap().lines() {
            assert!(!Path::new(dir).exists());
        }
    }
}

#[test]
fn bundle_changes_require_a_publisher_fragment() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    let config = root.join("gleam.toml");
    let text = fs::read_to_string(&config).unwrap();
    write(
        &config,
        &text.replace(
            "lib_a = [\"lib_b\"]",
            "lib_a = [\"lib_b\"]\nlib_d = [\"lib_b\"]",
        ),
    );
    write(
        &root.join("packages/lib_d/gleam.toml"),
        "name = \"lib_d\"\nversion = \"1.0.0\"\n[dependencies]\nlib_b = { path = \"../lib_b\" }\n",
    );
    write(
        &root.join("packages/lib_d/src/lib_d.gleam"),
        "import lib_b\n\npub fn answer() -> Int {\n  lib_b.answer()\n}\n",
    );
    init_repo(root);
    git(root, &["checkout", "-q", "-b", "feature"]);
    write(
        &root.join("packages/lib_c/src/lib_c.gleam"),
        "pub fn zero() -> Int { 0 }\n",
    );
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change private dependency"]);
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
        .failure()
        .stdout(predicate::str::contains("lib_a"));
    add_fragment(root, "lib_a", "Fixed", "Correct bundled code.");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "document publisher change"]);
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
        .failure()
        .stdout(predicate::str::contains("lib_d"));
    add_fragment(root, "lib_d", "Fixed", "Correct bundled code.");
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "-q", "-m", "document second publisher change"],
    );
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
        .success();
}

#[test]
#[ignore = "requires Gleam 1.18+, Erlang, Node.js, and tar; never publishes to Hex"]
fn bundle_real_hex_archive_builds_outside_the_workspace_on_both_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    let wrapper = root.join("export-instead-of-publish.sh");
    write(
        &wrapper,
        &format!(
            "#!/bin/sh\nset -eu\n\
             if [ \"$1\" = publish ]; then\n\
               gleam test --target javascript\n\
               gleam export hex-tarball\n\
               cp build/lib_a-1.0.0.tar '{}/package.tar'\n\
             else\n\
               gleam \"$@\"\n\
             fi\n",
            root.display()
        ),
    );
    make_executable(&wrapper);
    publish(root, &wrapper).assert().success();

    let archive = tempfile::tempdir().unwrap();
    run(
        archive.path(),
        "tar",
        &["xf", root.join("package.tar").to_str().unwrap()],
    );
    let extracted = tempfile::tempdir().unwrap();
    run(
        extracted.path(),
        "tar",
        &[
            "xzf",
            archive.path().join("contents.tar.gz").to_str().unwrap(),
        ],
    );
    for name in [
        "src/lib_b.gleam",
        "src/lib_c.gleam",
        "src/native_helper.erl",
        "src/native_helper.mjs",
        "src/helper.mjs",
        "LICENCE",
    ] {
        assert!(
            extracted.path().join(name).exists(),
            "archive missing {name}"
        );
    }
    let licences = fs::read_to_string(extracted.path().join("LICENCE")).unwrap();
    assert!(licences.contains("Bundled package: lib_b"));
    assert!(licences.contains("lib_b test licence"));
    assert!(licences.contains("Bundled package: lib_c"));
    let metadata = fs::read_to_string(archive.path().join("metadata.config")).unwrap();
    assert!(!metadata.contains("<<\"lib_b\">>"));
    assert!(!metadata.contains("<<\"lib_c\">>"));
    assert!(!extracted.path().join("test").exists());
    assert!(!extracted.path().join("build").exists());

    let consumer = tempfile::tempdir().unwrap();
    write(
        &consumer.path().join("gleam.toml"),
        &format!(
            "name = \"consumer\"\nversion = \"1.0.0\"\n\
             [dependencies]\nlib_a = {{ path = \"{}\" }}\n",
            extracted.path().display()
        ),
    );
    write(
        &consumer.path().join("test/consumer_test.gleam"),
        "import lib_a\n\npub fn main() {\n  case lib_a.answer() {\n    42 -> Nil\n    _ -> panic as \"wrong answer\"\n  }\n}\n",
    );
    write(
        &consumer.path().join("src/consumer.gleam"),
        "pub fn main() {\n  Nil\n}\n",
    );
    // Hide the original packages: archive consumers cannot fall back to them.
    fs::rename(root.join("packages"), root.join("original-packages")).unwrap();
    run(consumer.path(), "gleam", &["test"]);
    run(
        consumer.path(),
        "gleam",
        &["test", "--target", "javascript"],
    );
}

fn run(dir: &Path, program: &str, args: &[&str]) {
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bundle_merges_identical_requirements_and_promotes_dev_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    let a = root.join("packages/lib_a/gleam.toml");
    let b = root.join("packages/lib_b/gleam.toml");
    let a_text = fs::read_to_string(&a).unwrap();
    let b_text = fs::read_to_string(&b).unwrap();
    write(
        &a,
        &format!("{a_text}[dev-dependencies]\nexternal = \"== 1.0.0\"\n"),
    );
    write(
        &b,
        &format!("{b_text}external = {{ version = \"== 1.0.0\" }}\n"),
    );
    let fake = fake_gleam(root, "never");
    publish(root, &fake).assert().success();
    let published: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("published.toml")).unwrap()).unwrap();
    assert_eq!(
        published["dependencies"]["external"].as_str(),
        Some("== 1.0.0")
    );
    assert!(published["dev-dependencies"].get("external").is_none());
}

#[test]
fn bundle_keeps_hex_members_external_and_rebases_private_dev_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    let config = root.join("gleam.toml");
    write(
        &config,
        &fs::read_to_string(&config).unwrap().replace(
            "\"packages/lib_c\" = \"workspace\"",
            "\"packages/lib_c\" = \"hex\"",
        ),
    );
    let helper = root.join("packages/test_helper");
    write(
        &helper.join("gleam.toml"),
        "name = \"test_helper\"\nversion = \"1.0.0\"\n",
    );
    let config_text = fs::read_to_string(&config).unwrap();
    write(
        &config,
        &format!("{config_text}\"packages/test_helper\" = \"workspace\"\n"),
    );
    let a = root.join("packages/lib_a/gleam.toml");
    let text = fs::read_to_string(&a).unwrap();
    write(
        &a,
        &format!("{text}[dev-dependencies]\ntest_helper = {{ path = \"../test_helper\" }}\n"),
    );
    let fake = fake_gleam(root, "never");
    publish(root, &fake).assert().success();
    let published: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("published.toml")).unwrap()).unwrap();
    assert_eq!(
        published["dependencies"]["lib_c"].as_str(),
        Some(">= 1.0.0 and < 2.0.0")
    );
    assert!(!root.join("published-src/lib_c.gleam").exists());
    let dev = published["dev-dependencies"]["test_helper"]["path"]
        .as_str()
        .unwrap();
    assert!(Path::new(dev).is_absolute());
    assert_eq!(
        fs::canonicalize(dev).unwrap(),
        fs::canonicalize(helper).unwrap()
    );
}

#[test]
fn bundle_rejects_native_case_and_visibility_conflicts() {
    for (name, contents, error) in [
        ("lib_a.erl", "-module(lib_a).\n", "conflict"),
        ("LIB_A.gleam", "pub fn x() { Nil }\n", "conflict"),
        ("lib_a.mjs", "export const x = 1;\n", "conflict"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        workspace(tmp.path());
        write(&tmp.path().join("packages/lib_b/src").join(name), contents);
        let fake = fake_gleam(tmp.path(), "never");
        publish(tmp.path(), &fake)
            .arg("--dry-run")
            .assert()
            .failure()
            .stderr(predicate::str::contains(error));
    }
    let tmp = tempfile::tempdir().unwrap();
    workspace(tmp.path());
    let a = tmp.path().join("packages/lib_a/gleam.toml");
    let text = fs::read_to_string(&a).unwrap();
    write(&a, &format!("internal_modules = [\"lib_b\"]\n{text}"));
    let fake = fake_gleam(tmp.path(), "never");
    publish(tmp.path(), &fake)
        .arg("--dry-run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("internal_modules conflict"));
}

#[test]
fn bundle_rejects_unsupported_application_identity_and_symlinks() {
    for setting in [
        "[erlang]\napplication_start_module = \"lib_b\"\n",
        "[dependencies]\nremote = { git = \"https://example.invalid/repo\", ref = \"main\" }\n",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        workspace(tmp.path());
        let c = tmp.path().join("packages/lib_c/gleam.toml");
        let text = fs::read_to_string(&c).unwrap();
        write(&c, &format!("{text}{setting}"));
        let fake = fake_gleam(tmp.path(), "never");
        publish(tmp.path(), &fake)
            .arg("--dry-run")
            .assert()
            .failure();
        assert!(!tmp.path().join("gleam-log").exists());
    }
    let tmp = tempfile::tempdir().unwrap();
    workspace(tmp.path());
    let source = tmp.path().join("packages/lib_b/src/lib_b.gleam");
    let link = tmp.path().join("packages/lib_c/src/link.gleam");
    std::os::unix::fs::symlink(source, link).unwrap();
    let fake = fake_gleam(tmp.path(), "never");
    publish(tmp.path(), &fake)
        .arg("--dry-run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("symlink"));
}

#[test]
fn bundle_requires_opt_in_for_each_direct_private_dependency() {
    let tmp = tempfile::tempdir().unwrap();
    workspace(tmp.path());
    let a = tmp.path().join("packages/lib_a/gleam.toml");
    let text = fs::read_to_string(&a).unwrap();
    write(&a, &format!("{text}lib_c = {{ path = \"../lib_c\" }}\n"));
    let fake = fake_gleam(tmp.path(), "never");
    publish(tmp.path(), &fake)
        .arg("--dry-run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("lib_c"));
}

#[test]
fn bundle_preserves_compiler_and_application_requirements() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    workspace(root);
    for (name, app) in [("lib_a", "inets"), ("lib_b", "ssl")] {
        let path = root.join(format!("packages/{name}/gleam.toml"));
        let text = fs::read_to_string(&path).unwrap();
        write(
            &path,
            &format!("gleam = \">= 1.18.0\"\n{text}\n[erlang]\nextra_applications = [\"{app}\"]\n"),
        );
    }
    let fake = fake_gleam(root, "never");
    publish(root, &fake).assert().success();
    let published: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("published.toml")).unwrap()).unwrap();
    assert_eq!(published["gleam"].as_str(), Some(">= 1.18.0"));
    let apps = published["erlang"]["extra_applications"]
        .as_array()
        .unwrap();
    assert_eq!(
        apps.iter()
            .map(|app| app.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["inets", "ssl"]
    );
    let path = root.join("packages/lib_b/gleam.toml");
    let text = fs::read_to_string(&path).unwrap();
    write(&path, &text.replace(">= 1.18.0", ">= 1.19.0"));
    publish(root, &fake)
        .arg("--dry-run")
        .assert()
        .failure()
        .stderr(predicate::str::contains("compiler requirement conflict"));
}
