//! `trellis doctor` — validate every workspace invariant that would otherwise
//! be enforced only by hope. Reports all problems, exits non-zero on any error.

use crate::gleam;
use crate::workspace::Workspace;
use anyhow::Result;
use std::path::Path;

#[derive(Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Report {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }
    fn warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

/// Returns true when the workspace is healthy (warnings allowed).
pub fn run(root: &Path) -> Result<bool> {
    let (workspace, diagnostics) = Workspace::load_with_diagnostics(root)?;
    let mut report = Report {
        errors: diagnostics.errors,
        warnings: diagnostics.warnings,
    };

    if let Some(workspace) = &workspace {
        check_ignore_release(workspace, &mut report);
        check_tag_collisions(workspace, &mut report);
        check_lockfiles(workspace, &mut report);
        check_changelogs(workspace, &mut report);
        check_fragments(workspace, &mut report);
        check_tool_versions(workspace, &mut report);
    }

    let checked = [
        "member globs resolve and every member has a parseable gleam.toml",
        "path dependencies stay inside the workspace; graph is acyclic",
        "ignore-release globs match members; no releasable member depends on an unreleasable one",
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

    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    for error in &report.errors {
        println!("error: {error}");
    }
    if report.errors.is_empty() {
        let members = workspace.map(|ws| ws.members.len()).unwrap_or(0);
        println!(
            "ok: {members} member(s), {} warning(s)",
            report.warnings.len()
        );
        Ok(true)
    } else {
        println!(
            "FAILED: {} error(s), {} warning(s)",
            report.errors.len(),
            report.warnings.len()
        );
        Ok(false)
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

/// Check 6: every ignore-release glob matches at least one member (catches
/// typos), and no releasable member path-depends on an ignore-release member —
/// a published package cannot require a project that will never be on Hex.
fn check_ignore_release(workspace: &Workspace, report: &mut Report) {
    for pattern in &workspace.config.workspace.ignore_release {
        let matches = globset::Glob::new(pattern)
            .ok()
            .map(|g| g.compile_matcher())
            .map(|m| {
                workspace
                    .members
                    .iter()
                    .any(|member| m.is_match(&member.rel_path))
            });
        match matches {
            Some(true) => {}
            Some(false) => report.error(format!(
                "ignore-release glob `{pattern}` matches no member (typo?)"
            )),
            None => report.error(format!("ignore-release glob `{pattern}` is invalid")),
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
                     by ignore-release and will never exist on Hex",
                    member.name, dep.name
                ));
            }
        }
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
    for member in &workspace.members {
        let lockfile = member.path.join("manifest.toml");
        if !lockfile.is_file() {
            continue; // not generated yet; nothing to drift
        }
        let locked = match gleam::load_lockfile(&lockfile) {
            Ok(locked) => locked,
            Err(err) => {
                report.error(format!("{err:#}"));
                continue;
            }
        };
        for package in locked {
            let Some(idx) = workspace.member_index(&package.name) else {
                continue; // a Hex dep, not workspace-internal
            };
            let actual = workspace.members[idx].version();
            if package.version != actual {
                report.error(format!(
                    "{}/manifest.toml locks `{}` at {} but its gleam.toml says {} \
                     (run the version-apply lockfile patch)",
                    member.rel_path, package.name, package.version, actual
                ));
            }
        }
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
