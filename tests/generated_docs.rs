//! Checks for the generated CLI reference and man pages.

use assert_cmd::Command;
use std::fs;
use std::path::Path;

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
