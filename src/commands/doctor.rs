//! `trellis doctor` — validate every workspace invariant that would otherwise
//! be enforced only by hope. Reports all problems, exits non-zero on any error.

use crate::config::{Strictness, TagLevel};
use crate::gleam::Requirement;
use crate::json::{Check, DoctorDocument, Finding, FixRecord, PackageLifecycleRecord, Severity};
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
/// Every fix carries a package and a workspace-relative path, because
/// `--format json` reports a fix the same way whichever check produced it.
struct Fix {
    kind: FixKind,
    package: String,
    rel_path: String,
    path: PathBuf,
    contents: String,
}

#[derive(Clone, Copy)]
enum FixKind {
    /// Seed a releasable member's missing CHANGELOG.md with the rendered
    /// header, so it matches regenerated output byte-for-byte.
    SeedChangelog,
    /// Rewrite a manifest.toml's locked workspace-internal versions — the same
    /// operation `version apply` performs.
    PatchLockfile,
    /// Capture a package's pre-trellis CHANGELOG.md body as a version section,
    /// so regenerating the changelog preserves it. `version apply` does this on
    /// a first release anyway; doing it here makes it visible beforehand.
    AdoptChangelog,
}

impl Fix {
    /// Stable identifier for the wire format; `describe` is the prose beside it.
    fn kind(&self) -> &'static str {
        match self.kind {
            FixKind::SeedChangelog => "seed_changelog",
            FixKind::PatchLockfile => "patch_lockfile",
            FixKind::AdoptChangelog => "adopt_changelog",
        }
    }

    fn describe(&self) -> String {
        match self.kind {
            FixKind::SeedChangelog => format!("seed CHANGELOG.md for `{}`", self.package),
            FixKind::PatchLockfile => format!("patch locked versions in {}", self.rel_path),
            FixKind::AdoptChangelog => {
                format!("adopt existing changelog history for `{}`", self.package)
            }
        }
    }

    fn record(&self) -> FixRecord<'_> {
        FixRecord {
            kind: self.kind(),
            description: self.describe(),
            file: &self.rel_path,
            package: Some(&self.package),
        }
    }

    fn apply(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&self.path, &self.contents)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }
}

#[derive(Default)]
struct Report {
    /// Every finding, in the order it was discovered. Text mode groups by
    /// severity when printing; the JSON payload preserves this order.
    findings: Vec<Finding>,
    fixes: Vec<Fix>,
    /// No [tools.trellis] anywhere; the root was inferred from git.
    configless: bool,
    /// `members` is not configured; the member list came from git.
    auto_members: bool,
    /// Every member's resolved lifecycle, in workspace (topological) order —
    /// the source for the text summary's counts, the JSON payload's
    /// `package_lifecycles`, and the package count.
    package_lifecycles: Vec<PackageLifecycleRecord>,
}

impl Report {
    fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }
    fn of_severity(&self, severity: Severity) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.severity == severity)
    }
    fn count(&self, severity: Severity) -> usize {
        self.of_severity(severity).count()
    }
    fn packages(&self) -> usize {
        self.package_lifecycles.len()
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
        report.configless = workspace.configless;
        report.auto_members = workspace.config.members.is_none();
        report.package_lifecycles = workspace
            .members
            .iter()
            .map(|member| PackageLifecycleRecord {
                name: member.name.clone(),
                lifecycle: member.lifecycle,
            })
            .collect();
        check_exclusions(workspace, &mut report);
        check_tag_collisions(workspace, &mut report);
        check_lockfiles(workspace, &mut report);
        check_pinned_refs(workspace, &mut report);
        check_changelogs(workspace, &mut report);
        check_fragments(workspace, &mut report);
        check_shared_dependencies(workspace, &mut report);
        check_tool_versions(workspace, &mut report);
    }
    Ok(report)
}

/// Returns true when the workspace is healthy (warnings allowed).
pub fn run(root: &Path, options: &DoctorOptions) -> Result<bool> {
    let text = options.format.is_text();
    if text {
        let checked = [
            "member globs resolve and every package has a parseable gleam.toml",
            "path dependencies stay inside the workspace; graph is acyclic",
            "task exclusion globs match members; no package depends on one unavailable at its release lifecycle",
            "tag format produces a unique tag per releasable package",
            "manifest.toml locked versions match workspace-internal gleam.toml versions",
            "pinned git dependency SHAs remain reachable from their tracked refs (advisory)",
            "each releasable package's version is not behind its CHANGELOG",
            "unreleased changelog fragments parse and reference valid packages, kinds, and categories",
            "[tools.trellis] carries no unrecognized or deprecated keys",
            "packages agree on the external dependencies they share",
            "gleam on PATH matches the .tool-versions pin (advisory)",
        ];
        for check in checked {
            crate::status!("{}", crate::term::dim(&format!("checked: {check}")));
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
                crate::status!(
                    "{}",
                    crate::term::dim(&format!("would fix: {}", fix.describe()))
                );
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
                crate::status!("{} {}", crate::term::ok("fixed:"), fix.describe());
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
                "{} {} finding(s) are auto-fixable; rerun with --fix",
                crate::term::note("note:"),
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
            "{} no [tools.trellis] configuration found; workspace root inferred from git, \
             {} member(s) auto-discovered",
            crate::term::note("note:"),
            report.packages()
        );
    } else if report.auto_members {
        crate::status!(
            "{} `members` is not configured; {} member(s) auto-discovered from git",
            crate::term::note("note:"),
            report.packages()
        );
    }
    for warning in report.of_severity(Severity::Warning) {
        crate::status!("{} {}", crate::term::warn("warning:"), warning.message);
    }
    for error in report.of_severity(Severity::Error) {
        crate::status!("{} {}", crate::term::err("error:"), error.message);
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
                packages: report.packages(),
                configless: report.configless,
                auto_members: report.auto_members,
                findings: &report.findings,
                fixes: report.fixes.iter().map(Fix::record).collect(),
                applied: applied.iter().map(Fix::record).collect(),
                package_lifecycles: &report.package_lifecycles,
            };
            println!("{}", serde_json::to_string_pretty(&document)?);
        }
        DoctorFormat::Github => print_annotations(report),
    }
    Ok(ok)
}

fn print_summary(report: &Report, ok: bool) {
    let warnings = report.count(Severity::Warning);
    // Compact lifecycle counts, always in `ReleaseLifecycle::ALL` order, so a
    // reader can scan the same position across workspaces rather than parsing
    // which label is which.
    let lifecycles = crate::config::ReleaseLifecycle::ALL
        .map(|lifecycle| {
            let count = report
                .package_lifecycles
                .iter()
                .filter(|record| record.lifecycle == lifecycle)
                .count();
            format!("{count} {}", lifecycle.key())
        })
        .join(", ");
    if ok {
        // The JSON field behind this count is still `members` — renaming a
        // stable key would bump `trellis.doctor/1`, so it waits for 1.0.
        crate::status!(
            "{} {} package(s) ({lifecycles}), {warnings} warning(s)",
            crate::term::ok("ok:"),
            report.packages()
        );
    } else {
        crate::status!(
            "{} {} error(s), {warnings} warning(s)",
            crate::term::err("FAILED:"),
            report.count(Severity::Error)
        );
    }
}

/// GitHub Actions workflow commands, one per finding.
///
/// Nothing else is printed — a healthy run emits an empty stdout, and the
/// configless/auto_members inference is not a `::notice`, since it would fire
/// on every run in an auto-discovered repository. The exit code still carries
/// the verdict.
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
                if let Some(package) = problem.package {
                    finding = finding.in_package(package);
                }
                report.push(finding);
            }
        }
        Err(err) => report.push(Finding::error(Check::ChangelogFragment, format!("{err:#}"))),
    }
}

/// Members must agree on the external dependencies they share.
///
/// This is the purest instance of the design principle — *verify anything that
/// must be duplicated*. Nothing else notices that `lat_core` requires
/// `gleam_stdlib >= 0.44.0` while `lat_cli` requires `>= 0.60.0`; it is what
/// people install syncpack for in other ecosystems, and trellis already has
/// every input in the workspace model.
///
/// Requirement strings are compared verbatim, never parsed as ranges: trellis
/// stores them as written, and a check that must-be-identical strings are
/// identical is the honest reading of "these are duplicated". `>= 1.0` and
/// `>=1.0` therefore read as divergent, which the message says out loud.
///
/// Path dependencies are out of scope — they carry no requirement to agree on,
/// and lockfile drift already covers them.
fn check_shared_dependencies(workspace: &Workspace, report: &mut Report) {
    let strictness = workspace.config.doctor.shared_dependencies;
    if strictness == Strictness::Off {
        return;
    }

    // dependency name -> requirement -> the members asking for it.
    let mut requirements: BTreeMap<&str, BTreeMap<&str, Vec<&str>>> = BTreeMap::new();
    for member in &workspace.members {
        for dependency in &member.manifest.dependencies {
            if let Requirement::Hex(requirement) = &dependency.requirement {
                requirements
                    .entry(&dependency.name)
                    .or_default()
                    .entry(requirement)
                    .or_default()
                    .push(&member.name);
            }
        }
    }

    for (dependency, by_requirement) in requirements {
        if by_requirement.len() < 2 {
            continue;
        }
        let detail = by_requirement
            .iter()
            .map(|(requirement, members)| format!("`{requirement}` ({})", members.join(", ")))
            .collect::<Vec<_>>()
            .join(" vs ");
        let message = format!(
            "packages disagree on `{dependency}`: {detail}. Requirements are compared as \
             written, so whitespace counts"
        );
        report.push(match strictness {
            Strictness::Error => Finding::error(Check::SharedDependency, message),
            _ => Finding::warning(Check::SharedDependency, message),
        });
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
/// and no member's runtime distribution would depend on a package unavailable
/// in it — see [`crate::config::ReleaseLifecycle::available_to`].
fn check_exclusions(workspace: &Workspace, report: &mut Report) {
    for (task, patterns) in &workspace.config.exclude {
        // `@members` is validated in `Workspace::load_with_diagnostics`,
        // against the pre-filter candidate set — by the time `workspace`
        // exists here, a working exclusion has already removed its own
        // evidence from `workspace.members`.
        if task == crate::config::MEMBERS_EXCLUDE_KEY {
            continue;
        }
        for pattern in patterns {
            check_member_glob(
                workspace,
                &format!("`{task}` exclusion glob"),
                pattern,
                report,
            );
        }
    }
    // Same typo trap, one table over: a package-tags override that matches
    // nothing silently leaves its packages on the workspace default.
    for pattern in workspace.config.publish.package_tags_overrides.keys() {
        check_member_glob(workspace, "`package_tags_overrides` glob", pattern, report);
    }
    // And again for `publish.lifecycle.packages`: a glob matching nothing
    // silently leaves its packages on the legacy mapping or the default.
    for pattern in workspace.config.publish.lifecycle.packages.keys() {
        check_member_glob(
            workspace,
            "`publish.lifecycle.packages` glob",
            pattern,
            report,
        );
    }

    for (idx, member) in workspace.members.iter().enumerate() {
        for &dep in workspace.runtime_deps_of(idx) {
            let dep = &workspace.members[dep];
            if !dep.lifecycle.available_to(member.lifecycle) {
                report.push(
                    Finding::error(
                        Check::ReleaseBoundary,
                        format!(
                            "package `{0}` (lifecycle `{1}`) path-depends on `{2}` (lifecycle \
                             `{3}`), which is unavailable in `{0}`'s distribution",
                            member.name,
                            member.lifecycle.key(),
                            dep.name,
                            dep.lifecycle.key(),
                        ),
                    )
                    .at(format!("{}/gleam.toml", member.rel_path))
                    .in_package(&member.name),
                );
            }
        }
    }
}

fn check_member_glob(workspace: &Workspace, label: &str, pattern: &str, report: &mut Report) {
    let problem = match globset::Glob::new(pattern).map(|glob| glob.compile_matcher()) {
        Err(_) => "is invalid",
        Ok(matcher)
            if !workspace
                .members
                .iter()
                .any(|m| matcher.is_match(&m.rel_path)) =>
        {
            "matches no member (typo?)"
        }
        Ok(_) => return,
    };
    // Either way the claim is about the root manifest's [tools.trellis]
    // table, which is where the glob was written.
    report.push(
        Finding::error(
            Check::ExclusionGlob,
            format!("{label} `{pattern}` {problem}"),
        )
        .at(crate::workspace::GLEAM_TOML),
    );
}

/// Check 7: no two releasable members produce the same tag, for series tags as
/// well as exact ones.
///
/// A `series_tag_format` without `{name}` is the exception: it is deprecated
/// in favour of the `repository_tag_*` keys, and warns rather than errors so
/// that repositories using it keep releasing. The warning is keyed on the
/// format alone, not on today's versions or member count — `resolve_tag`
/// substitutes `{name}` per member, so with no `{name}` to substitute *every*
/// series tag matches every member regardless of what they're versioned at.
/// Warning only on a version collision, or only once a second package makes it
/// ambiguous, would go quiet on exactly the configurations that break as soon
/// as the workspace grows.
fn check_tag_collisions(workspace: &Workspace, report: &mut Report) {
    fn insert(
        seen: &mut std::collections::HashMap<String, String>,
        report: &mut Report,
        tag: String,
        owner: String,
        member: &crate::workspace::Member,
    ) {
        if let Some(other) = seen.get(&tag) {
            report.push(
                Finding::error(
                    Check::TagCollision,
                    format!("tag collision: {other} and {owner} both produce tag `{tag}`"),
                )
                .at(crate::workspace::GLEAM_TOML)
                .in_package(&member.name),
            );
        } else {
            seen.insert(tag, owner);
        }
    }

    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for member in workspace.members.iter().filter(|m| m.releasable()) {
        if member.tags.contains(&TagLevel::Exact) {
            let tag = workspace.config.exact_tag(&member.name, member.version());
            insert(
                &mut seen,
                report,
                tag,
                format!("package `{}` exact tag", member.name),
                member,
            );
        }
    }

    let series_members: Vec<&str> = workspace
        .members
        .iter()
        .filter(|m| m.releasable() && m.tags.iter().any(|t| t.is_series()))
        .map(|m| m.name.as_str())
        .collect();
    let names = |members: &[&str]| {
        members
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let repo_wide = workspace.config.series_tag_is_repo_wide();
    if repo_wide && !series_members.is_empty() {
        // A deprecation, not a collision: the shape is wrong even where it
        // happens to be unambiguous today, because one more series-mode
        // package makes it ambiguous with no other config change.
        let ambiguity = if series_members.len() > 1 {
            format!(
                ", so one repository-wide series tag covers {} and `trellis ci tag-package` \
                 cannot resolve it to one package",
                names(&series_members)
            )
        } else {
            String::new()
        };
        report.push(
            Finding::warning(
                Check::WorkspaceConfig,
                format!(
                    "`series_tag_format` `{}` has no {{name}}{ambiguity}. A `{{name}}`-less \
                     `series_tag_format` is deprecated and will be removed at 1.0; declare \
                     `[tools.trellis.publish.repository_series]` with an anchor `package` \
                     instead, and restore `{{name}}` here. Note the repository tag is not \
                     resolved by `ci tag-package` or `publish --tag`",
                    workspace.config.publish.series_tag_format,
                ),
            )
            .at(crate::workspace::GLEAM_TOML),
        );
    }

    // Members sharing a legacy `{name}`-less series tag is intentional — the
    // ambiguity warning above covers it — so claim each such tag only once.
    let mut legacy_claimed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for member in workspace
        .members
        .iter()
        .filter(|m| m.releasable() && m.tags.iter().any(|t| t.is_series()))
    {
        for tag in workspace
            .config
            .series_tags(&member.name, member.version(), &member.tags)
        {
            if repo_wide && !legacy_claimed.insert(tag.clone()) {
                continue;
            }
            insert(
                &mut seen,
                report,
                tag,
                format!("package `{}` series tag", member.name),
                member,
            );
        }
    }

    // The repository tag's collision rule is `tag create`'s namespace check:
    // it must not look like any package's tag at any version, not merely avoid
    // the tag strings today's versions happen to produce.
    if let Some(index) = workspace.repository_series_anchor {
        let anchor = &workspace.members[index];
        for tag in workspace.config.repository_tags(anchor.version()) {
            if crate::commands::tag::repository_tag_collides_with_packages(workspace, &tag)
                .unwrap_or(false)
            {
                report.push(
                    Finding::error(
                        Check::TagCollision,
                        format!(
                            "tag collision: repository tag `{tag}` (anchored to `{}`) \
                             occupies a package tag namespace; choose a distinct \
                             `repository_tag_format`",
                            anchor.name
                        ),
                    )
                    .at(crate::workspace::GLEAM_TOML)
                    .in_package(&anchor.name),
                );
            }
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
        let lockfile = match lockfile::patch_member(member, &versions) {
            Ok(Some(lockfile)) => lockfile,
            Ok(None) => continue, // no lockfile yet, or nothing drifted
            Err(err) => {
                report.push(
                    Finding::error(Check::LockfileDrift, format!("{err:#}"))
                        .at(format!("{}/manifest.toml", member.rel_path))
                        .in_package(&member.name),
                );
                continue;
            }
        };
        // One rewrite clears every drifted entry in this manifest, so each of
        // these findings is fixable by the single fix pushed below.
        for entry in &lockfile.patched {
            report.push(
                Finding::error(
                    Check::LockfileDrift,
                    format!(
                        "{}/manifest.toml locks `{}` at {} but its gleam.toml says {} \
                         (run the version-apply lockfile patch)",
                        member.rel_path, entry.name, entry.old, entry.new
                    ),
                )
                .at(&lockfile.rel_path)
                .in_package(&member.name)
                .fixable(),
            );
        }
        report.fixes.push(Fix {
            kind: FixKind::PatchLockfile,
            package: member.name.clone(),
            rel_path: lockfile.rel_path,
            path: lockfile.path,
            contents: lockfile.text,
        });
    }
}

/// Advisory: each `# trellis:pin` tracked ref should still contain its pinned
/// SHA. Warnings, not errors — the check needs the network, and re-pinning is
/// a supply-chain decision `--fix` must not make. `trellis pin --check` is the
/// enforcing form.
fn check_pinned_refs(workspace: &Workspace, report: &mut Report) {
    let indices: Vec<usize> = (0..workspace.members.len()).collect();
    match crate::commands::pin::check_pins(workspace, &indices) {
        Ok(drifts) => {
            for drift in drifts {
                report.push(
                    Finding::warning(Check::PinnedRef, drift.message)
                        .at(&drift.rel_path)
                        .in_package(&drift.package),
                );
            }
        }
        Err(err) => report.push(Finding::warning(
            Check::PinnedRef,
            format!("could not verify pinned refs: {err:#}"),
        )),
    }
}

/// Check 4 (best-effort until the changelog layer lands): each releasable
/// member should have a CHANGELOG.md, and its gleam.toml version must not be
/// behind the newest version mentioned in it.
fn check_changelogs(workspace: &Workspace, report: &mut Report) {
    for member in workspace.members.iter().filter(|m| m.releasable()) {
        let changelog = member.path.join("CHANGELOG.md");
        let rel_changelog = format!("{}/CHANGELOG.md", member.rel_path);
        if !changelog.is_file() {
            // The stub is the rendered header alone, so a later
            // `version apply` regenerates it byte-for-byte. Rendering it first
            // is what decides whether the warning is fixable at all.
            let header = crate::changelog::render_header(&workspace.config.changelog, &member.name);
            report.push(
                Finding::warning(
                    Check::ChangelogMissing,
                    format!("releasable package `{}` has no CHANGELOG.md", member.name),
                )
                .at(&rel_changelog)
                .in_package(&member.name)
                .fixable_if(header.is_ok()),
            );
            match header {
                Ok(header) => report.fixes.push(Fix {
                    kind: FixKind::SeedChangelog,
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
                    .in_package(&member.name),
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
                .in_package(&member.name),
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
                .in_package(&member.name),
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
                .in_package(&member.name),
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
                    .in_package(&member.name)
                    .fixable(),
                );
                report.fixes.push(Fix {
                    kind: FixKind::AdoptChangelog,
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
                .in_package(&member.name),
            ),
        }
    }
}
