//! `trellis doctor` — validate every workspace invariant that would otherwise
//! be enforced only by hope. Reports all problems, exits non-zero on any error.

use crate::lockfile;
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct DoctorOptions {
    /// Apply the fixable findings, then re-report what remains.
    pub fix: bool,
    /// List what `--fix` would do without touching any files.
    pub dry_run: bool,
}

/// A finding whose remediation is entirely mechanical, so `doctor --fix` can
/// apply it. The fix content is computed at check time (it's exactly what the
/// canonical command would write), so applying is a single write.
enum Fix {
    /// Seed a releasable member's missing CHANGELOG.md with the same header
    /// `trellis new` scaffolds, so it matches regenerated output byte-for-byte.
    SeedChangelog {
        package: String,
        path: PathBuf,
        contents: String,
    },
    /// Rewrite a manifest.toml's locked workspace-internal versions — the same
    /// operation `version apply` performs.
    PatchLockfile {
        display: String,
        path: PathBuf,
        contents: String,
    },
}

impl Fix {
    fn describe(&self) -> String {
        match self {
            Fix::SeedChangelog { package, .. } => format!("seed CHANGELOG.md for `{package}`"),
            Fix::PatchLockfile { display, .. } => format!("patch locked versions in {display}"),
        }
    }

    fn apply(&self) -> Result<()> {
        let (path, contents) = match self {
            Fix::SeedChangelog { path, contents, .. } => (path, contents),
            Fix::PatchLockfile { path, contents, .. } => (path, contents),
        };
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

#[derive(Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
    fixes: Vec<Fix>,
    members: usize,
    /// No [tools.trellis] anywhere; the root was inferred from git.
    configless: bool,
    /// `members` is not configured; the member list came from git.
    auto_members: bool,
}

impl Report {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }
    fn warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
    fn fix(&mut self, fix: Fix) {
        self.fixes.push(fix);
    }
}

/// Load the workspace and run every check, collecting findings and the fixes
/// that would remediate the mechanical ones. No output, no side effects.
fn inspect(root: &Path) -> Result<Report> {
    let (workspace, diagnostics) = Workspace::load_with_diagnostics(root)?;
    let mut report = Report {
        errors: diagnostics.errors,
        warnings: diagnostics.warnings,
        ..Report::default()
    };

    if let Some(workspace) = &workspace {
        report.members = workspace.members.len();
        report.configless = workspace.configless;
        report.auto_members = workspace.config.members.is_none();
        check_exclusions(workspace, &mut report);
        check_tag_collisions(workspace, &mut report);
        check_lockfiles(workspace, &mut report);
        check_changelogs(workspace, &mut report);
        check_fragments(workspace, &mut report);
        check_tool_versions(workspace, &mut report);
    }
    Ok(report)
}

/// Returns true when the workspace is healthy (warnings allowed).
pub fn run(root: &Path, options: &DoctorOptions) -> Result<bool> {
    let checked = [
        "member globs resolve and every member has a parseable gleam.toml",
        "path dependencies stay inside the workspace; graph is acyclic",
        "task exclusion globs match members; no releasable member depends on an unreleasable one",
        "tag format produces a unique tag per releasable member",
        "manifest.toml locked versions match workspace-internal gleam.toml versions",
        "each releasable member's version is not behind its CHANGELOG",
        "unreleased changelog fragments parse and reference valid packages and kinds",
        "gleam on PATH matches the .tool-versions pin (advisory)",
    ];
    for check in checked {
        println!("checked: {check}");
    }
    println!();

    let mut report = inspect(root)?;

    // --dry-run only previews; it never writes, so state (and exit code) is
    // identical to a plain run — a fixable error still fails, keeping CI honest.
    if options.dry_run {
        print_findings(&report);
        for fix in &report.fixes {
            println!("would fix: {}", fix.describe());
        }
        return Ok(finalize(&report));
    }

    // --fix applies every mechanical remedy, then re-inspects from disk so the
    // summary reflects the true post-fix state, not a guess.
    if options.fix && !report.fixes.is_empty() {
        for fix in &report.fixes {
            fix.apply()?;
            println!("fixed: {}", fix.describe());
        }
        println!();
        report = inspect(root)?;
    }

    print_findings(&report);
    if !options.fix && !report.fixes.is_empty() {
        println!(
            "note: {} finding(s) are auto-fixable; rerun with --fix",
            report.fixes.len()
        );
    }
    Ok(finalize(&report))
}

fn print_findings(report: &Report) {
    // Auto-discovery leaves no file saying "this is the workspace", so doctor
    // states the inference instead of leaving it invisible.
    if report.configless {
        println!(
            "note: no [tools.trellis] configuration found; workspace root inferred from git, \
             {} member(s) auto-discovered",
            report.members
        );
    } else if report.auto_members {
        println!(
            "note: `members` is not configured; {} member(s) auto-discovered from git",
            report.members
        );
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    for error in &report.errors {
        println!("error: {error}");
    }
}

/// Print the summary line and return whether the workspace is healthy.
fn finalize(report: &Report) -> bool {
    if report.errors.is_empty() {
        println!(
            "ok: {} member(s), {} warning(s)",
            report.members,
            report.warnings.len()
        );
        true
    } else {
        println!(
            "FAILED: {} error(s), {} warning(s)",
            report.errors.len(),
            report.warnings.len()
        );
        false
    }
}

/// Check 3 (revised for the native engine): every unreleased fragment must
/// parse, name a releasable member, and use a configured kind — an invalid
/// fragment would otherwise surface only at release time.
fn check_fragments(workspace: &Workspace, report: &mut Report) {
    match crate::changelog::load_fragments(workspace) {
        Ok(fragments) => {
            for problem in fragments.problems {
                report.error(problem);
            }
        }
        Err(err) => report.error(format!("{err:#}")),
    }
}

/// Advisory (design §11 q4): when `.tool-versions` pins gleam, warn if the
/// gleam on PATH is a different version. Enforcing toolchains is mise/asdf's
/// job — trellis only surfaces the mismatch, and only as a warning.
fn check_tool_versions(workspace: &Workspace, report: &mut Report) {
    let Ok(text) = std::fs::read_to_string(workspace.root.join(".tool-versions")) else {
        return;
    };
    let Some(pinned) = text.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("gleam ").map(|v| v.trim().to_string())
    }) else {
        return;
    };
    let Ok(output) = std::process::Command::new(crate::tools::gleam_bin())
        .arg("--version")
        .output()
    else {
        report.warning(format!(
            ".tool-versions pins gleam {pinned} but no gleam was found on PATH"
        ));
        return;
    };
    if !output.status.success() {
        return;
    }
    // `gleam --version` prints e.g. "gleam 1.5.1".
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(actual) = stdout
        .split_whitespace()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
    else {
        return;
    };
    if actual != pinned {
        report.warning(format!(
            "gleam on PATH is {actual} but .tool-versions pins {pinned}"
        ));
    }
}

/// Check 6: every exclusion glob matches at least one member (catches typos),
/// and no releasable member path-depends on a release-excluded member.
fn check_exclusions(workspace: &Workspace, report: &mut Report) {
    for (task, patterns) in &workspace.config.exclude {
        for pattern in patterns {
            check_exclusion_pattern(workspace, task, pattern, report);
        }
    }

    for (idx, member) in workspace.members.iter().enumerate() {
        if !member.releasable {
            continue;
        }
        for &dep in workspace.deps_of(idx) {
            let dep = &workspace.members[dep];
            if !dep.releasable {
                report.error(format!(
                    "releasable package `{}` path-depends on `{}`, which is excluded from release \
                     and will never exist on Hex",
                    member.name, dep.name
                ));
            }
        }
    }
}

fn check_exclusion_pattern(workspace: &Workspace, task: &str, pattern: &str, report: &mut Report) {
    let matches = globset::Glob::new(pattern)
        .ok()
        .map(|glob| glob.compile_matcher())
        .map(|matcher| {
            workspace
                .members
                .iter()
                .any(|member| matcher.is_match(&member.rel_path))
        });
    match matches {
        Some(true) => {}
        Some(false) => report.error(format!(
            "`{task}` exclusion glob `{pattern}` matches no member (typo?)"
        )),
        None => report.error(format!("`{task}` exclusion glob `{pattern}` is invalid")),
    }
}

/// Check 7: no two releasable members produce the same tag.
fn check_tag_collisions(workspace: &Workspace, report: &mut Report) {
    let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    for member in workspace.members.iter().filter(|m| m.releasable) {
        let tag = workspace.config.format_tag(&member.name, member.version());
        if let Some(other) = seen.insert(tag.clone(), &member.name) {
            report.error(format!(
                "tag collision: `{other}` and `{}` both produce tag `{tag}`",
                member.name
            ));
        }
    }
}

/// Check 5: each member's manifest.toml must lock workspace-internal deps at
/// those deps' actual gleam.toml versions (catches a missed lockfile patch
/// after a version bump).
fn check_lockfiles(workspace: &Workspace, report: &mut Report) {
    // The fix is exactly the operation `version apply` performs: rewrite each
    // locked workspace-internal version to its member's current gleam.toml
    // version. Computing the patch here doubles as the drift check.
    let versions: BTreeMap<String, String> = workspace
        .members
        .iter()
        .map(|member| (member.name.clone(), member.version().to_string()))
        .collect();

    for member in &workspace.members {
        let path = member.path.join("manifest.toml");
        if !path.is_file() {
            continue; // not generated yet; nothing to drift
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                report.error(format!("failed to read {}: {err}", path.display()));
                continue;
            }
        };
        let (new_text, patched) = match lockfile::patch_locked_versions(&text, &versions) {
            Ok(result) => result,
            Err(err) => {
                report.error(format!("{err:#}"));
                continue;
            }
        };
        if patched.is_empty() {
            continue;
        }
        for entry in &patched {
            report.error(format!(
                "{}/manifest.toml locks `{}` at {} but its gleam.toml says {} \
                 (run the version-apply lockfile patch)",
                member.rel_path, entry.name, entry.old, entry.new
            ));
        }
        report.fix(Fix::PatchLockfile {
            display: format!("{}/manifest.toml", member.rel_path),
            path,
            contents: new_text,
        });
    }
}

/// Check 4 (best-effort until the changelog layer lands): each releasable
/// member should have a CHANGELOG.md, and its gleam.toml version must not be
/// behind the newest version mentioned in it.
fn check_changelogs(workspace: &Workspace, report: &mut Report) {
    for member in workspace.members.iter().filter(|m| m.releasable) {
        let changelog = member.path.join("CHANGELOG.md");
        if !changelog.is_file() {
            report.warning(format!(
                "releasable package `{}` has no CHANGELOG.md",
                member.name
            ));
            // The stub is the same header `trellis new` scaffolds, so a later
            // `version apply` regenerates it byte-for-byte.
            match crate::changelog::render_header(&workspace.config.changelog, &member.name) {
                Ok(header) => report.fix(Fix::SeedChangelog {
                    package: member.name.clone(),
                    path: changelog,
                    contents: format!("{}\n", header.trim_end()),
                }),
                Err(err) => report.error(format!(
                    "cannot render a CHANGELOG.md header for `{}`: {err:#}",
                    member.name
                )),
            }
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&changelog) else {
            report.warning(format!("could not read {}/CHANGELOG.md", member.rel_path));
            continue;
        };
        let Ok(current) = semver::Version::parse(member.version()) else {
            report.error(format!(
                "package `{}` version `{}` is not valid semver",
                member.name,
                member.version()
            ));
            continue;
        };
        if let Some(latest) = latest_changelog_version(&text)
            && current < latest
        {
            report.error(format!(
                "package `{}` gleam.toml version {} is behind its CHANGELOG ({latest})",
                member.name, current
            ));
        }
    }
}

/// Newest semver mentioned in a `## ...` heading, tolerating the common
/// changie/keep-a-changelog shapes: `## 1.2.3`, `## [1.2.3]`, `## v1.2.3`,
/// `## name-v1.2.3 - 2026-01-01`.
fn latest_changelog_version(text: &str) -> Option<semver::Version> {
    text.lines()
        .filter_map(|line| line.strip_prefix("## "))
        .filter_map(|heading| {
            let token = heading.split_whitespace().next()?;
            let token = token.trim_matches(['[', ']']);
            let token = token.rsplit_once("-v").map(|(_, v)| v).unwrap_or(token);
            let token = token.strip_prefix('v').unwrap_or(token);
            semver::Version::parse(token).ok()
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::latest_changelog_version;

    #[test]
    fn parses_common_changelog_headings() {
        let text =
            "# Changelog\n\n## lattice_core-v1.2.0 - 2026-01-05\n\n## [1.1.0]\n\n## v1.0.0\n";
        assert_eq!(
            latest_changelog_version(text),
            Some(semver::Version::new(1, 2, 0))
        );
    }

    #[test]
    fn ignores_non_version_headings() {
        assert_eq!(latest_changelog_version("## Unreleased\n## Notes\n"), None);
    }
}
