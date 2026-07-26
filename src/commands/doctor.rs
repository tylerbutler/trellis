//! `trellis doctor` — validate every workspace invariant that would otherwise
//! be enforced only by hope. Reports all problems, exits non-zero on any error.

use crate::json::{Check, DoctorDocument, Finding, FixRecord, Severity};
use crate::lockfile;
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const TOOL_VERSIONS: &str = ".tool-versions";

/// How findings are rendered. `text` is for a person; the other two are the
/// machine-readable surfaces CI consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum DoctorFormat {
    #[default]
    Text,
    /// The `trellis.doctor/1` payload.
    Json,
    /// GitHub Actions workflow commands, so findings land on the file in the
    /// PR's Files tab instead of in a log nobody expands.
    Github,
}

impl DoctorFormat {
    /// Whether prose — the `checked:` preamble, progress lines, the summary —
    /// may be printed. Structured formats own stdout entirely.
    fn is_text(self) -> bool {
        self == DoctorFormat::Text
    }
}

#[derive(Default)]
pub struct DoctorOptions {
    /// Apply the fixable findings, then re-report what remains.
    pub fix: bool,
    /// List what `--fix` would do without touching any files.
    pub dry_run: bool,
    pub format: DoctorFormat,
}

/// A finding whose remediation is entirely mechanical, so `doctor --fix` can
/// apply it. The fix content is computed at check time (it's exactly what the
/// canonical command would write), so applying is a single write.
///
/// Every variant carries a package and a workspace-relative path, because
/// `--format json` reports a fix the same way whichever check produced it.
enum Fix {
    /// Seed a releasable member's missing CHANGELOG.md with the same header
    /// `trellis new` scaffolds, so it matches regenerated output byte-for-byte.
    SeedChangelog {
        package: String,
        rel_path: String,
        path: PathBuf,
        contents: String,
    },
    /// Rewrite a manifest.toml's locked workspace-internal versions — the same
    /// operation `version apply` performs.
    PatchLockfile {
        package: String,
        rel_path: String,
        path: PathBuf,
        contents: String,
    },
    /// Capture a package's pre-trellis CHANGELOG.md body as a version section,
    /// so regenerating the changelog preserves it. `version apply` does this on
    /// a first release anyway; doing it here makes it visible beforehand.
    AdoptChangelog {
        package: String,
        rel_path: String,
        path: PathBuf,
        contents: String,
    },
}

impl Fix {
    /// Stable identifier for the wire format; `describe` is the prose beside it.
    fn kind(&self) -> &'static str {
        match self {
            Fix::SeedChangelog { .. } => "seed-changelog",
            Fix::PatchLockfile { .. } => "patch-lockfile",
            Fix::AdoptChangelog { .. } => "adopt-changelog",
        }
    }

    fn describe(&self) -> String {
        match self {
            Fix::SeedChangelog { package, .. } => format!("seed CHANGELOG.md for `{package}`"),
            Fix::PatchLockfile { rel_path, .. } => format!("patch locked versions in {rel_path}"),
            Fix::AdoptChangelog { package, .. } => {
                format!("adopt existing changelog history for `{package}`")
            }
        }
    }

    fn package(&self) -> &str {
        match self {
            Fix::SeedChangelog { package, .. }
            | Fix::PatchLockfile { package, .. }
            | Fix::AdoptChangelog { package, .. } => package,
        }
    }

    fn rel_path(&self) -> &str {
        match self {
            Fix::SeedChangelog { rel_path, .. }
            | Fix::PatchLockfile { rel_path, .. }
            | Fix::AdoptChangelog { rel_path, .. } => rel_path,
        }
    }

    fn record(&self) -> FixRecord<'_> {
        FixRecord {
            kind: self.kind(),
            description: self.describe(),
            file: self.rel_path(),
            package: Some(self.package()),
        }
    }

    fn apply(&self) -> Result<()> {
        let (path, contents) = match self {
            Fix::SeedChangelog { path, contents, .. } => (path, contents),
            Fix::PatchLockfile { path, contents, .. } => (path, contents),
            Fix::AdoptChangelog { path, contents, .. } => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create {}", parent.display()))?;
                }
                (path, contents)
            }
        };
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

#[derive(Default)]
struct Report {
    /// Every finding, in the order it was discovered. Text mode groups by
    /// severity when printing; the JSON payload preserves this order.
    findings: Vec<Finding>,
    fixes: Vec<Fix>,
    members: usize,
    /// No [tools.trellis] anywhere; the root was inferred from git.
    configless: bool,
    /// `members` is not configured; the member list came from git.
    auto_members: bool,
}

impl Report {
    fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }
    fn error(&mut self, check: Check, message: impl Into<String>) {
        self.push(Finding::error(check, message));
    }
    fn fix(&mut self, fix: Fix) {
        self.fixes.push(fix);
    }
    fn of_severity(&self, severity: Severity) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.severity == severity)
    }
    fn count(&self, severity: Severity) -> usize {
        self.of_severity(severity).count()
    }
}

/// Load the workspace and run every check, collecting findings and the fixes
/// that would remediate the mechanical ones. No output, no side effects.
fn inspect(root: &Path) -> Result<Report> {
    let (workspace, diagnostics) = Workspace::load_with_diagnostics(root)?;
    let mut report = Report {
        findings: diagnostics.findings,
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
    let text = options.format.is_text();
    if text {
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
            crate::status!("checked: {check}");
        }
        crate::status!();
    }

    let mut report = inspect(root)?;

    // --dry-run only previews; it never writes, so state (and exit code) is
    // identical to a plain run — a fixable error still fails, keeping CI honest.
    if options.dry_run {
        if text {
            print_findings(&report);
            for fix in &report.fixes {
                crate::status!("would fix: {}", fix.describe());
            }
        }
        return finish(&report, &[], options.format);
    }

    // --fix applies every mechanical remedy, then re-inspects from disk so the
    // summary reflects the true post-fix state, not a guess.
    let mut applied: Vec<Fix> = Vec::new();
    if options.fix && !report.fixes.is_empty() {
        for fix in &report.fixes {
            fix.apply()?;
            if text {
                crate::status!("fixed: {}", fix.describe());
            }
        }
        if text {
            crate::status!();
        }
        // Moved out before the re-inspect, which by design no longer reports
        // them — this is the only remaining record of what was written.
        applied = std::mem::take(&mut report.fixes);
        report = inspect(root)?;
    }

    if text {
        print_findings(&report);
        if !options.fix && !report.fixes.is_empty() {
            crate::status!(
                "note: {} finding(s) are auto-fixable; rerun with --fix",
                report.fixes.len()
            );
        }
    }
    finish(&report, &applied, options.format)
}

fn print_findings(report: &Report) {
    // Auto-discovery leaves no file saying "this is the workspace", so doctor
    // states the inference instead of leaving it invisible.
    if report.configless {
        crate::status!(
            "note: no [tools.trellis] configuration found; workspace root inferred from git, \
             {} member(s) auto-discovered",
            report.members
        );
    } else if report.auto_members {
        crate::status!(
            "note: `members` is not configured; {} member(s) auto-discovered from git",
            report.members
        );
    }
    for warning in report.of_severity(Severity::Warning) {
        crate::status!("warning: {}", warning.message);
    }
    for error in report.of_severity(Severity::Error) {
        crate::status!("error: {}", error.message);
    }
}

/// Emit whatever the chosen format still owes, then report health. The exit
/// code is the same in every format — only the rendering differs.
fn finish(report: &Report, applied: &[Fix], format: DoctorFormat) -> Result<bool> {
    let ok = report.count(Severity::Error) == 0;
    match format {
        DoctorFormat::Text => print_summary(report, ok),
        DoctorFormat::Json => {
            let document = DoctorDocument {
                schema: DoctorDocument::SCHEMA,
                ok,
                members: report.members,
                configless: report.configless,
                auto_members: report.auto_members,
                findings: &report.findings,
                fixes: report.fixes.iter().map(Fix::record).collect(),
                applied: applied.iter().map(Fix::record).collect(),
            };
            println!("{}", serde_json::to_string_pretty(&document)?);
        }
        DoctorFormat::Github => print_annotations(report),
    }
    Ok(ok)
}

fn print_summary(report: &Report, ok: bool) {
    let warnings = report.count(Severity::Warning);
    if ok {
        crate::status!("ok: {} member(s), {warnings} warning(s)", report.members);
    } else {
        crate::status!(
            "FAILED: {} error(s), {warnings} warning(s)",
            report.count(Severity::Error)
        );
    }
}

/// GitHub Actions workflow commands, one per finding.
///
/// Nothing else is printed — a healthy run emits an empty stdout, and the
/// configless/auto-members inference is deliberately not a `::notice`, since it
/// would fire on every run in an auto-discovered repository. The exit code
/// still carries the verdict.
fn print_annotations(report: &Report) {
    for finding in &report.findings {
        let level = match finding.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let mut properties = format!("title={}", finding.check.as_str());
        if let Some(file) = &finding.file {
            properties.push_str(&format!(",file={}", escape_property(file)));
        }
        println!("::{level} {properties}::{}", escape_data(&finding.message));
    }
}

/// A workflow command is one line, so a message containing a newline — a toml
/// parse error, say — would otherwise truncate at the break or, worse, inject a
/// second command.
fn escape_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Property values additionally escape the delimiters of the property list
/// itself.
fn escape_property(value: &str) -> String {
    escape_data(value).replace(':', "%3A").replace(',', "%2C")
}

/// Check 3 (revised for the native engine): every unreleased fragment must
/// parse, name a releasable member, and use a configured kind — an invalid
/// fragment would otherwise surface only at release time.
fn check_fragments(workspace: &Workspace, report: &mut Report) {
    match crate::changelog::load_fragments(workspace) {
        Ok(fragments) => {
            for problem in fragments.problems {
                let mut finding = Finding::error(Check::ChangelogFragment, problem.message)
                    .at(workspace.rel_path_of(&problem.path));
                if let Some(project) = problem.project {
                    finding = finding.in_package(project);
                }
                report.push(finding);
            }
        }
        Err(err) => report.error(Check::ChangelogFragment, format!("{err:#}")),
    }
}

/// Advisory (design §11 q4): when `.tool-versions` pins gleam, warn if the
/// gleam on PATH is a different version. Enforcing toolchains is mise/asdf's
/// job — trellis only surfaces the mismatch, and only as a warning.
fn check_tool_versions(workspace: &Workspace, report: &mut Report) {
    let Ok(text) = std::fs::read_to_string(workspace.root.join(TOOL_VERSIONS)) else {
        return;
    };
    let Some(pinned) = text.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("gleam ").map(|v| v.trim().to_string())
    }) else {
        return;
    };
    let gleam = crate::tools::gleam_bin();
    crate::term::trace_command(&gleam, &["--version"], &workspace.root);
    let Ok(output) = std::process::Command::new(&gleam).arg("--version").output() else {
        report.push(
            Finding::warning(
                Check::Toolchain,
                format!(".tool-versions pins gleam {pinned} but no gleam was found on PATH"),
            )
            .at(TOOL_VERSIONS),
        );
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
        report.push(
            Finding::warning(
                Check::Toolchain,
                format!("gleam on PATH is {actual} but .tool-versions pins {pinned}"),
            )
            .at(TOOL_VERSIONS),
        );
    }
}

/// Check 6: every exclusion glob matches at least one member (catches typos),
/// and no releasable member path-depends on a release-excluded member.
fn check_exclusions(workspace: &Workspace, report: &mut Report) {
    for (task, patterns) in &workspace.config.exclude {
        for pattern in patterns {
            check_member_glob(
                workspace,
                &format!("`{task}` exclusion glob"),
                pattern,
                report,
            );
        }
    }
    // Same typo trap, one table over: a tag-mode override that matches nothing
    // silently leaves its packages on the workspace default.
    for (mode, patterns) in &workspace.config.publish.tag_mode_overrides {
        for pattern in patterns {
            check_member_glob(
                workspace,
                &format!("`tag-mode-overrides.{mode}` glob"),
                pattern,
                report,
            );
        }
    }

    for (idx, member) in workspace.members.iter().enumerate() {
        if !member.releasable {
            continue;
        }
        for &dep in workspace.deps_of(idx) {
            let dep = &workspace.members[dep];
            if !dep.releasable {
                report.push(
                    Finding::error(
                        Check::ReleaseBoundary,
                        format!(
                            "releasable package `{}` path-depends on `{}`, which is excluded \
                             from release and will never exist on Hex",
                            member.name, dep.name
                        ),
                    )
                    .at(format!("{}/gleam.toml", member.rel_path))
                    .in_package(member.name.clone()),
                );
            }
        }
    }
}

fn check_member_glob(workspace: &Workspace, label: &str, pattern: &str, report: &mut Report) {
    let matches = globset::Glob::new(pattern)
        .ok()
        .map(|glob| glob.compile_matcher())
        .map(|matcher| {
            workspace
                .members
                .iter()
                .any(|member| matcher.is_match(&member.rel_path))
        });
    // Both cases are a claim about the root manifest's [tools.trellis] table,
    // which is where the glob was written.
    match matches {
        Some(true) => {}
        Some(false) => report.push(
            Finding::error(
                Check::ExclusionGlob,
                format!("{label} `{pattern}` matches no member (typo?)"),
            )
            .at(crate::workspace::GLEAM_TOML),
        ),
        None => report.push(
            Finding::error(
                Check::ExclusionGlob,
                format!("{label} `{pattern}` is invalid"),
            )
            .at(crate::workspace::GLEAM_TOML),
        ),
    }
}

/// Check 7: no two releasable members produce the same tag, for series tags as
/// well as exact ones.
///
/// A `series-tag-format` without `{name}` is the exception: sharing one
/// repository-wide tag is the point, so it warns rather than errors. The
/// warning is keyed on the format and the member count, not on today's
/// versions — `resolve_tag` substitutes `{name}` per member, so with no
/// `{name}` to substitute *every* series tag matches every member regardless
/// of what they're versioned at. Warning only on a version collision would go
/// quiet on exactly the configurations `trellis ci tag-package` still fails on.
fn check_tag_collisions(workspace: &Workspace, report: &mut Report) {
    let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    for member in workspace.members.iter().filter(|m| m.releasable) {
        if member.tag_mode.includes_exact() {
            let tag = workspace.config.format_tag(&member.name, member.version());
            if let Some(other) = seen.insert(tag.clone(), &member.name) {
                report.push(
                    Finding::error(
                        Check::TagCollision,
                        format!(
                            "tag collision: `{other}` and `{}` both produce tag `{tag}`",
                            member.name
                        ),
                    )
                    .at(format!("{}/gleam.toml", member.rel_path))
                    .in_package(member.name.clone()),
                );
            }
        }
    }

    let series_members: Vec<&str> = workspace
        .members
        .iter()
        .filter(|m| m.releasable && m.tag_mode.includes_series())
        .map(|m| m.name.as_str())
        .collect();
    let names = |members: &[&str]| {
        members
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    if workspace.config.series_tag_is_repo_wide() {
        if series_members.len() > 1 {
            report.push(
                Finding::warning(
                    Check::TagCollision,
                    format!(
                        "`series-tag-format` `{}` has no {{name}}, so one repository-wide series \
                         tag covers {}; `trellis ci tag-package` cannot resolve such a tag to one \
                         package",
                        workspace.config.publish.series_tag_format,
                        names(&series_members)
                    ),
                )
                .at(crate::workspace::GLEAM_TOML),
            );
        }
        return;
    }

    let mut sharing: std::collections::BTreeMap<String, Vec<&str>> =
        std::collections::BTreeMap::new();
    for member in workspace
        .members
        .iter()
        .filter(|m| m.releasable && m.tag_mode.includes_series())
    {
        if let Some(tag) = workspace
            .config
            .format_series_tag(&member.name, member.version())
        {
            sharing.entry(tag).or_default().push(&member.name);
        }
    }
    for (tag, members) in sharing {
        if members.len() > 1 {
            report.push(
                Finding::error(
                    Check::TagCollision,
                    format!(
                        "series tag collision: {} all produce tag `{tag}`",
                        names(&members)
                    ),
                )
                .at(crate::workspace::GLEAM_TOML),
            );
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
        let rel_path = format!("{}/manifest.toml", member.rel_path);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                report.push(
                    Finding::error(
                        Check::LockfileDrift,
                        format!("failed to read {}: {err}", path.display()),
                    )
                    .at(&rel_path)
                    .in_package(member.name.clone()),
                );
                continue;
            }
        };
        let (new_text, patched) = match lockfile::patch_locked_versions(&text, &versions) {
            Ok(result) => result,
            Err(err) => {
                report.push(
                    Finding::error(Check::LockfileDrift, format!("{err:#}"))
                        .at(&rel_path)
                        .in_package(member.name.clone()),
                );
                continue;
            }
        };
        if patched.is_empty() {
            continue;
        }
        // One rewrite clears every drifted entry in this manifest, so each of
        // these findings is fixable by the single fix pushed below.
        for entry in &patched {
            report.push(
                Finding::error(
                    Check::LockfileDrift,
                    format!(
                        "{}/manifest.toml locks `{}` at {} but its gleam.toml says {} \
                         (run the version-apply lockfile patch)",
                        member.rel_path, entry.name, entry.old, entry.new
                    ),
                )
                .at(&rel_path)
                .in_package(member.name.clone())
                .fixable(),
            );
        }
        report.fix(Fix::PatchLockfile {
            package: member.name.clone(),
            rel_path,
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
        let rel_changelog = format!("{}/CHANGELOG.md", member.rel_path);
        if !changelog.is_file() {
            // The stub is the same header `trellis new` scaffolds, so a later
            // `version apply` regenerates it byte-for-byte. Rendering it first
            // is what decides whether the warning is fixable at all.
            let header = crate::changelog::render_header(&workspace.config.changelog, &member.name);
            report.push(
                Finding::warning(
                    Check::ChangelogMissing,
                    format!("releasable package `{}` has no CHANGELOG.md", member.name),
                )
                .at(&rel_changelog)
                .in_package(member.name.clone())
                .fixable_if(header.is_ok()),
            );
            match header {
                Ok(header) => report.fix(Fix::SeedChangelog {
                    package: member.name.clone(),
                    rel_path: rel_changelog,
                    path: changelog,
                    contents: format!("{}\n", header.trim_end()),
                }),
                Err(err) => report.push(
                    Finding::error(
                        Check::PackageVersion,
                        format!(
                            "cannot render a CHANGELOG.md header for `{}`: {err:#}",
                            member.name
                        ),
                    )
                    .in_package(member.name.clone()),
                ),
            }
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&changelog) else {
            report.push(
                Finding::warning(
                    Check::ChangelogUnreadable,
                    format!("could not read {}/CHANGELOG.md", member.rel_path),
                )
                .at(&rel_changelog)
                .in_package(member.name.clone()),
            );
            continue;
        };
        let Ok(current) = semver::Version::parse(member.version()) else {
            report.push(
                Finding::error(
                    Check::PackageVersion,
                    format!(
                        "package `{}` version `{}` is not valid semver",
                        member.name,
                        member.version()
                    ),
                )
                .at(format!("{}/gleam.toml", member.rel_path))
                .in_package(member.name.clone()),
            );
            continue;
        };
        if let Some(latest) = crate::changelog::latest_changelog_version(&text)
            && current < latest
        {
            // The manifest is what has to change, so blame it rather than the
            // changelog that reported the newer version.
            report.push(
                Finding::error(
                    Check::ChangelogBehind,
                    format!(
                        "package `{}` gleam.toml version {} is behind its CHANGELOG ({latest})",
                        member.name, current
                    ),
                )
                .at(format!("{}/gleam.toml", member.rel_path))
                .in_package(member.name.clone()),
            );
        }

        // CHANGELOG.md is regenerated from `.changes/<pkg>/`, so history that
        // was never batched would vanish on the next release. `version apply`
        // adopts it automatically; surfacing it here means nobody meets it for
        // the first time mid-release.
        match crate::changelog::plan_adoption(workspace, &member.name, member.version()) {
            Ok(Some(adoption)) => {
                report.push(
                    Finding::warning(
                        Check::ChangelogAdoption,
                        format!(
                            "package `{}` has changelog history that trellis has not batched \
                             yet; it will be adopted on the next release",
                            member.name
                        ),
                    )
                    .at(&rel_changelog)
                    .in_package(member.name.clone())
                    .fixable(),
                );
                report.fix(Fix::AdoptChangelog {
                    package: member.name.clone(),
                    rel_path: rel_changelog,
                    path: adoption.path,
                    contents: adoption.contents,
                });
            }
            Ok(None) => {}
            Err(err) => report.push(
                Finding::error(
                    Check::ChangelogAdoption,
                    format!("cannot read `{}`'s changelog history: {err:#}", member.name),
                )
                .at(&rel_changelog)
                .in_package(member.name.clone()),
            ),
        }
    }
}
