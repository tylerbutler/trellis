//! The native changelog engine (changie subsumed — design §7, revised).
//!
//! Layout, under `[tools.trellis.changelog] dir` (default `.changes/`):
//!   unreleased/*.toml        one fragment per change: package, kind, body
//!   <package>/v<X.Y.Z>.md    batched version sections, rendered once
//! Each package's CHANGELOG.md is assembled from its header plus its version
//! sections, newest first. All formats are minijinja templates, so the
//! rendered shape is configurable without a second tool.

use crate::config::{Bump, ChangelogConfig, KindConfig};
use crate::workspace::{Member, Workspace};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;

// ---- fragments -------------------------------------------------------------

/// Why a non-releasable member can never take a changelog entry — quoted by
/// both `changelog new` and fragment validation, so the wording stays one.
pub fn no_changelog_reason(member: &Member) -> String {
    format!(
        "package `{}` has release lifecycle `{}`, so it never gets a changelog entry",
        member.name,
        member.lifecycle.key()
    )
}

#[derive(Debug, Clone)]
pub struct Fragment {
    pub package: String,
    pub kind: String,
    /// The optional second grouping axis. `None` files the entry under the
    /// configured `uncategorized_label`.
    pub category: Option<String>,
    pub body: String,
    /// `None` for a fragment trellis generated rather than read from disk, so
    /// `consume_fragments` can never mistake one for a file to delete.
    pub path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFragment {
    /// The package the change belongs to. `project` is the original spelling,
    /// inherited from changie; it still parses so fragments written by an
    /// older trellis keep working. Removed at 1.0.
    #[serde(alias = "project")]
    package: String,
    kind: String,
    /// Optional: a fragment need not name a category. This struct denies
    /// unknown fields, so a fragment carrying one will not parse under a
    /// trellis older than 0.8.
    #[serde(default)]
    category: Option<String>,
    body: String,
}

/// A fragment that could not be used, and the file it came from. The path is
/// kept so `doctor` can point a GitHub annotation at the offending file rather
/// than at the workspace in general.
#[derive(Debug, Clone)]
pub struct FragmentProblem {
    pub message: String,
    pub path: PathBuf,
    /// The `package` the fragment claimed, when it parsed far enough to have
    /// one.
    pub package: Option<String>,
}

/// All unreleased fragments plus every problem found while reading them.
/// Callers decide whether problems are warnings (`changelog check` reports
/// them) or fatal (`version plan/apply` refuses — silently dropping a
/// fragment is exactly the drift this tool exists to prevent).
#[derive(Debug, Default)]
pub struct Fragments {
    pub fragments: Vec<Fragment>,
    pub problems: Vec<FragmentProblem>,
}

impl Fragments {
    pub fn for_package<'a>(&'a self, package: &'a str) -> impl Iterator<Item = &'a Fragment> {
        self.fragments.iter().filter(move |f| f.package == package)
    }

    pub fn count_for(&self, package: &str) -> usize {
        self.for_package(package).count()
    }

    /// Problem messages alone, for the callers that only render prose.
    pub fn problem_messages(&self) -> Vec<String> {
        self.problems
            .iter()
            .map(|problem| problem.message.clone())
            .collect()
    }
}

pub fn unreleased_dir(workspace: &Workspace) -> PathBuf {
    workspace
        .root
        .join(&workspace.config.changelog.dir)
        .join("unreleased")
}

fn versions_dir(workspace: &Workspace, package: &str) -> PathBuf {
    workspace
        .root
        .join(&workspace.config.changelog.dir)
        .join(package)
}

/// Read and validate every unreleased fragment: it must parse, name a
/// releasable workspace member, use a configured kind, and have a body.
pub fn load_fragments(workspace: &Workspace) -> Result<Fragments> {
    let mut result = Fragments::default();
    let dir = unreleased_dir(workspace);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(result), // nothing unreleased yet
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort(); // deterministic order across filesystems

    let kinds = &workspace.config.changelog.kinds;
    let categories = &workspace.config.changelog.categories;
    for path in paths {
        let display = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: RawFragment = match toml::from_str(&text) {
            Ok(raw) => raw,
            Err(err) => {
                result.problems.push(FragmentProblem {
                    message: format!("fragment `{display}`: {err}"),
                    path,
                    package: None,
                });
                continue;
            }
        };
        // Past the parse, every problem names the same file and package.
        let blame = |message: String| FragmentProblem {
            message,
            path: path.clone(),
            package: Some(raw.package.clone()),
        };
        match workspace.member_index(&raw.package) {
            Some(idx) if workspace.members[idx].releasable() => {}
            Some(idx) => {
                result.problems.push(blame(format!(
                    "fragment `{display}`: {}",
                    no_changelog_reason(&workspace.members[idx])
                )));
                continue;
            }
            None => {
                result.problems.push(blame(format!(
                    "fragment `{display}`: package `{}` is not a workspace member",
                    raw.package
                )));
                continue;
            }
        }
        if !kinds.iter().any(|k| k.label == raw.kind) {
            result.problems.push(blame(format!(
                "fragment `{display}`: kind `{}` is not one of {}",
                raw.kind,
                kind_labels(kinds)
            )));
            continue;
        }
        if let Some(category) = &raw.category
            && !categories.iter().any(|c| c == category)
        {
            // With no `categories` configured there is no list to print, and
            // "not one of" would read as though the label were merely
            // misspelled. Name the key that turns the axis on instead.
            result.problems.push(blame(if categories.is_empty() {
                format!(
                    "fragment `{display}`: category `{category}` is set, but no `categories` \
                     are configured; add them under [tools.trellis.changelog]"
                )
            } else {
                format!(
                    "fragment `{display}`: category `{category}` is not one of {}",
                    category_labels(categories)
                )
            }));
            continue;
        }
        if raw.body.trim().is_empty() {
            result
                .problems
                .push(blame(format!("fragment `{display}`: body is empty")));
            continue;
        }
        result.fragments.push(Fragment {
            package: raw.package,
            kind: raw.kind,
            category: raw.category,
            body: raw.body.trim().to_string(),
            path: Some(path),
        });
    }
    Ok(result)
}

pub fn kind_labels(kinds: &[KindConfig]) -> String {
    kinds
        .iter()
        .map(|k| k.label.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn category_labels(categories: &[String]) -> String {
    categories.join(", ")
}

/// The entry recording that a workspace dependency bumped in this release.
///
/// A ripple is modelled as an ordinary fragment so the rest of the engine
/// needs no special case: `next_version` folds the `dependency_kind` bump into
/// the same max-bump rule, and `render_section` files it under that kind's
/// heading alongside any hand-written entries. It is never written to disk —
/// the body embeds the dependency's new version, which is only settled once
/// the whole plan is computed.
pub fn dependency_fragment(
    config: &ChangelogConfig,
    package: &str,
    dependency: &str,
    dependency_version: &str,
) -> Result<Fragment> {
    let body = render(
        &config.dependency_body,
        "dependency_body",
        // `project` is the pre-1.0 spelling of `package`, kept so existing
        // `dependency_body` templates keep rendering. Removed at 1.0.
        minijinja::context! { dependency, dependency_version, package, project => package },
    )?;
    Ok(Fragment {
        package: package.to_string(),
        kind: config.dependency_kind.clone(),
        // A ripple belongs to no one part of the package, so it files under
        // `uncategorized_label` with any other entry that named no category.
        category: None,
        body: body.trim().to_string(),
        path: None,
    })
}

/// The filename stem a body earns: its leading words, kebab-cased. Naming the
/// file after the change is what makes a directory of fragments readable, and
/// what keeps two branches from both claiming `<package>-1.toml`.
fn body_slug(body: &str) -> String {
    let mut out = String::new();
    for word in body
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(4)
    {
        let word = word.to_lowercase();
        if !out.is_empty() {
            if out.len() + 1 + word.len() > 40 {
                break;
            }
            out.push('-');
        }
        out.push_str(&word);
    }
    out.truncate(40);
    out
}

/// Write a new fragment file, named for the package and the change it
/// describes. A body with nothing nameable in it (punctuation only) falls back
/// to the package alone, and any clash takes the next free `-<n>` suffix.
pub fn write_fragment(
    workspace: &Workspace,
    package: &str,
    kind: &str,
    category: Option<&str>,
    body: &str,
) -> Result<PathBuf> {
    let dir = unreleased_dir(workspace);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let mut doc = toml_edit::DocumentMut::new();
    doc["package"] = toml_edit::value(package);
    doc["kind"] = toml_edit::value(kind);
    if let Some(category) = category {
        doc["category"] = toml_edit::value(category);
    }
    doc["body"] = toml_edit::value(body);
    let stem = match body_slug(body) {
        slug if slug.is_empty() => package.to_string(),
        slug => format!("{package}-{slug}"),
    };
    for n in 1u32.. {
        let path = dir.join(match n {
            1 => format!("{stem}.toml"),
            n => format!("{stem}-{n}.toml"),
        });
        if !path.exists() {
            std::fs::write(&path, doc.to_string())
                .with_context(|| format!("failed to write {}", path.display()))?;
            return Ok(path);
        }
    }
    unreachable!("some counter is always free")
}

// ---- version computation ----------------------------------------------------

/// The bump the fragments call for: the largest among their kinds.
///
/// Deriving the *level* rather than the whole version is what lets
/// `version --bump/--set/--pre` override it, or combine it with a prerelease
/// label, before any version is constructed.
pub fn derive_bump(fragments: &[&Fragment], kinds: &[KindConfig]) -> Result<Bump> {
    fragments
        .iter()
        .filter_map(|fragment| {
            kinds
                .iter()
                .find(|k| k.label == fragment.kind)
                .map(|k| k.bump)
        })
        .max()
        .context("no fragments to compute a version bump from")
}

/// Apply a bump level, producing a clean release version.
///
/// Prerelease and build metadata on `current` are dropped: bumping *from*
/// `1.0.0-rc.1` by a minor means 1.1.0, not 1.1.0-rc.1. A caller that wants to
/// stay in a prerelease cycle bumps from the base version itself.
pub fn apply_bump(current: &semver::Version, bump: Bump) -> semver::Version {
    match bump {
        Bump::Major => semver::Version::new(current.major + 1, 0, 0),
        Bump::Minor => semver::Version::new(current.major, current.minor + 1, 0),
        Bump::Patch => semver::Version::new(current.major, current.minor, current.patch + 1),
    }
}

// ---- rendering ---------------------------------------------------------------

fn render(template: &str, what: &str, context: minijinja::Value) -> Result<String> {
    let mut env = minijinja::Environment::new();
    env.add_template(what, template)
        .with_context(|| format!("invalid {what} template"))?;
    env.get_template(what)
        .expect("just added")
        .render(context)
        .with_context(|| format!("failed to render {what} template"))
}

/// The `{{ series }}` variable available to `version_format`: the semver
/// compatibility range a version sits in — `0.Y` while the major is 0, `X`
/// after. Deliberately its own derivation rather than one of
/// [`crate::config::SeriesTag`]'s levels: this names a heading in a rendered
/// changelog and must not shift when a workspace changes which git tags it
/// publishes. `None` for a prerelease, which belongs to no series.
fn compatibility_series(version: &str) -> Option<String> {
    let version = semver::Version::parse(version).ok()?;
    if !version.pre.is_empty() {
        return None;
    }
    Some(if version.major == 0 {
        format!("0.{}", version.minor)
    } else {
        version.major.to_string()
    })
}

/// Render one version section: the version heading, then the entries grouped
/// by kind in configured order — or, when categories are configured, by
/// category first and kind within each.
pub fn render_section(
    config: &ChangelogConfig,
    name: &str,
    version: &str,
    tag: &str,
    date: &str,
    fragments: &[&Fragment],
) -> Result<String> {
    // Empty for a prerelease, which belongs to no series.
    let series = compatibility_series(version).unwrap_or_default();
    let mut out = render(
        &config.version_format,
        "version_format",
        minijinja::context! { name, version, date, tag, series },
    )?;
    out.push('\n');

    if !config.categories_enabled() {
        // Axis off: kind headings sit directly under the version heading.
        render_kinds(config, &mut out, name, version, fragments, None)?;
        return Ok(out);
    }

    for category in &config.categories {
        let entries: Vec<&Fragment> = fragments
            .iter()
            .copied()
            .filter(|f| f.category.as_deref() == Some(category.as_str()))
            .collect();
        if entries.is_empty() {
            continue; // same skip-empty rule kinds follow
        }
        render_category_heading(config, &mut out, category, name, version)?;
        render_kinds(config, &mut out, name, version, &entries, Some(category))?;
    }

    // Everything that named no category, including the generated ripple
    // entries, trails the named ones under one shared heading.
    let uncategorized: Vec<&Fragment> = fragments
        .iter()
        .copied()
        .filter(|f| f.category.is_none())
        .collect();
    if !uncategorized.is_empty() {
        let label = &config.uncategorized_label;
        render_category_heading(config, &mut out, label, name, version)?;
        render_kinds(config, &mut out, name, version, &uncategorized, Some(label))?;
    }
    Ok(out)
}

/// One category heading. The uncategorized block goes through here too, so a
/// custom `category_format` shapes every heading in the section alike.
fn render_category_heading(
    config: &ChangelogConfig,
    out: &mut String,
    category: &str,
    name: &str,
    version: &str,
) -> Result<()> {
    out.push('\n');
    out.push_str(&render(
        &config.category_format,
        "category_format",
        minijinja::context! { category, name, version },
    )?);
    out.push('\n');
    Ok(())
}

/// The kind headings and entries for one group of fragments. Opens each kind
/// with a blank line, which also separates it from a category heading above.
fn render_kinds(
    config: &ChangelogConfig,
    out: &mut String,
    name: &str,
    version: &str,
    fragments: &[&Fragment],
    category: Option<&str>,
) -> Result<()> {
    for kind in &config.kinds {
        let entries: Vec<&&Fragment> = fragments.iter().filter(|f| f.kind == kind.label).collect();
        if entries.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(&render(
            config.kind_format(),
            "kind_format",
            minijinja::context! { kind => kind.label, category, name, version },
        )?);
        out.push('\n');
        out.push('\n');
        for fragment in entries {
            out.push_str(&render(
                &config.change_format,
                "change_format",
                minijinja::context! {
                    body => fragment.body,
                    kind => fragment.kind,
                    category,
                    name,
                    version,
                },
            )?);
            out.push('\n');
        }
    }
    Ok(())
}

// ---- adoption ----------------------------------------------------------------

/// A package's pre-trellis CHANGELOG.md body, captured as one version section.
///
/// CHANGELOG.md is a generated file: it is rebuilt from `<dir>/<package>/v*.md`
/// alone. Without this, the first release of a package that already had a
/// changelog would silently delete all of its history. Capturing the body
/// verbatim keeps it byte-for-byte and needs no heading parsing — it simply
/// sorts below every section trellis goes on to write.
#[derive(Debug)]
pub struct Adoption {
    pub version: semver::Version,
    pub path: PathBuf,
    pub contents: String,
}

/// The history to adopt for `package`, if any. `None` once trellis has batched
/// a version for it: from then on CHANGELOG.md is fully generated, so leftover
/// content is drift rather than history.
pub fn plan_adoption(
    workspace: &Workspace,
    package: &str,
    current: &str,
) -> Result<Option<Adoption>> {
    let idx = workspace
        .member_index(package)
        .with_context(|| format!("unknown package `{package}`"))?;
    let dir = versions_dir(workspace, package);
    let already_batched = std::fs::read_dir(&dir).is_ok_and(|entries| {
        entries
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
    });
    if already_batched {
        return Ok(None);
    }

    let path = workspace.members[idx].path.join("CHANGELOG.md");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let body = strip_header(&text);
    if body.is_empty() {
        return Ok(None); // a header-only stub carries no history
    }

    let version = match latest_changelog_version(&text) {
        Some(version) => version,
        None => semver::Version::parse(current).with_context(|| {
            format!("cannot date `{package}`'s changelog history: `{current}` is not valid semver")
        })?,
    };
    Ok(Some(Adoption {
        path: dir.join(format!("v{version}.md")),
        version,
        contents: format!("{body}\n"),
    }))
}

/// Everything after a leading `# ` header line and the blank lines under it.
fn strip_header(text: &str) -> &str {
    let rest = match text.strip_prefix("# ") {
        Some(after) => after.split_once('\n').map(|(_, rest)| rest).unwrap_or(""),
        None => text,
    };
    rest.trim()
}

/// Newest semver mentioned in a `## ...` heading, tolerating the common
/// changie/keep-a-changelog shapes: `## 1.2.3`, `## [1.2.3]`, `## v1.2.3`,
/// `## name-v1.2.3 - 2026-01-01`.
pub fn latest_changelog_version(text: &str) -> Option<semver::Version> {
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

// ---- batch + merge -----------------------------------------------------------

/// Render a package's complete CHANGELOG.md with an optional pending section
/// and an optional block of adopted pre-trellis history.
pub fn render_merged_changelog(
    workspace: &Workspace,
    package: &str,
    pending: Option<(&semver::Version, &str)>,
    adopted: Option<&Adoption>,
) -> Result<String> {
    workspace
        .member_index(package)
        .with_context(|| format!("unknown package `{package}`"))?;
    let config = &workspace.config.changelog;
    let dir = versions_dir(workspace, package);

    let mut sections: Vec<(semver::Version, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let Ok(version) = semver::Version::parse(stem.trim_start_matches('v')) else {
                bail!(
                    "{} is not named v<semver>.md; refusing to guess its order",
                    path.display()
                );
            };
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            sections.push((version, text));
        }
    }
    if let Some(adoption) = adopted {
        sections.push((adoption.version.clone(), adoption.contents.clone()));
    }
    if let Some((version, section)) = pending {
        sections.retain(|(existing, _)| existing != version);
        sections.push((version.clone(), section.to_string()));
    }
    sections.sort_by(|a, b| b.0.cmp(&a.0));

    let header = render_header(config, package)?;
    let mut out = header.trim_end().to_string();
    out.push('\n');
    for (_, section) in &sections {
        out.push('\n');
        out.push_str(section.trim_end());
        out.push('\n');
    }

    Ok(out)
}

/// Render the CHANGELOG header for a package (also used by `trellis new`
/// for the initial stub, so scaffolded changelogs match regenerated ones).
pub fn render_header(config: &ChangelogConfig, name: &str) -> Result<String> {
    render(
        &config.header_format,
        "header_format",
        minijinja::context! { name },
    )
}

/// Write a pre-rendered version section and complete package changelog.
pub fn write_batch(
    workspace: &Workspace,
    package: &str,
    version: &semver::Version,
    section: &str,
    changelog: &str,
) -> Result<()> {
    let idx = workspace
        .member_index(package)
        .with_context(|| format!("unknown package `{package}`"))?;
    let dir = versions_dir(workspace, package);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let section_path = dir.join(format!("v{version}.md"));
    std::fs::write(&section_path, section)
        .with_context(|| format!("failed to write {}", section_path.display()))?;
    let path = workspace.members[idx].path.join("CHANGELOG.md");
    std::fs::write(&path, changelog).with_context(|| format!("failed to write {}", path.display()))
}

/// Delete the fragment files a release consumed. Generated fragments have no
/// file behind them and are skipped.
pub fn consume_fragments(fragments: &[&Fragment]) -> Result<()> {
    for fragment in fragments {
        let Some(path) = &fragment.path else {
            continue;
        };
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

// ---- dates -------------------------------------------------------------------

/// Today as YYYY-MM-DD (UTC). SOURCE_DATE_EPOCH (the reproducible-builds
/// convention) overrides the clock, which also keeps tests deterministic.
pub fn today() -> String {
    let epoch_seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    let (year, month, day) = civil_from_days(epoch_seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days-since-epoch to (year, month, day), after Howard Hinnant's
/// `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ChangelogConfig;

    fn fragment(package: &str, kind: &str, body: &str) -> Fragment {
        Fragment {
            package: package.to_string(),
            kind: kind.to_string(),
            category: None,
            body: body.to_string(),
            path: Some(PathBuf::from("unused")),
        }
    }

    /// A fragment's filename comes from its body, so the name has to survive
    /// whatever a body contains: punctuation, case, unicode, or nothing usable.
    #[test]
    fn fragment_names_are_slugs_of_their_body() {
        assert_eq!(
            body_slug("repair the flux capacitor, again"),
            "repair-the-flux-capacitor"
        );
        assert_eq!(body_slug("Fix `Vine::grow` panic!"), "fix-vine-grow-panic");
        assert_eq!(body_slug("  x  "), "x");
        assert_eq!(body_slug("!!! ---"), "");
        assert_eq!(body_slug(""), "");
        // Four long words still stop at the length cap, mid-word if need be.
        let long = body_slug("aaaaaaaaaaaaaaa bbbbbbbbbbbbbbb ccccccccccccccc");
        assert_eq!(long, "aaaaaaaaaaaaaaa-bbbbbbbbbbbbbbb");
        assert!(body_slug(&"z".repeat(60)).len() <= 40);
    }

    /// `{{ series }}` renders a heading in a committed changelog, so its
    /// derivation is pinned here and does not follow `publish.package_tags`.
    #[test]
    fn the_series_variable_is_the_compatibility_range() {
        assert_eq!(compatibility_series("0.0.17").as_deref(), Some("0.0"));
        assert_eq!(compatibility_series("0.12.3").as_deref(), Some("0.12"));
        assert_eq!(compatibility_series("1.2.3").as_deref(), Some("1"));
        assert_eq!(compatibility_series("10.0.0").as_deref(), Some("10"));
        assert_eq!(compatibility_series("1.0.0+build.5").as_deref(), Some("1"));
        assert_eq!(compatibility_series("1.0.0-beta"), None);
        assert_eq!(compatibility_series("not-a-version"), None);
    }

    fn categorized(package: &str, kind: &str, category: &str, body: &str) -> Fragment {
        Fragment {
            category: Some(category.to_string()),
            ..fragment(package, kind, body)
        }
    }

    /// Bump one version by what a set of fragments derives, the way
    /// `compute_plan` does.
    fn next_version(current: &str, fragments: &[&Fragment], kinds: &[KindConfig]) -> String {
        let current = semver::Version::parse(current).unwrap();
        apply_bump(&current, derive_bump(fragments, kinds).unwrap()).to_string()
    }

    #[test]
    fn the_largest_bump_among_the_kinds_wins() {
        let kinds = ChangelogConfig::default().kinds;
        let fixed = fragment("p", "Fixed", "x");
        let added = fragment("p", "Added", "y");
        let breaking = fragment("p", "Breaking", "z");
        let next = |frags: Vec<&Fragment>| next_version("1.2.3", &frags, &kinds);
        assert_eq!(next(vec![&fixed]), "1.2.4");
        assert_eq!(next(vec![&fixed, &added]), "1.3.0");
        assert_eq!(next(vec![&fixed, &added, &breaking]), "2.0.0");
    }

    #[test]
    fn deriving_a_bump_needs_at_least_one_known_kind() {
        let kinds = ChangelogConfig::default().kinds;
        let unknown = fragment("p", "Invented", "x");
        assert!(derive_bump(&[], &kinds).is_err());
        assert!(derive_bump(&[&unknown], &kinds).is_err());
    }

    #[test]
    fn applying_a_bump_drops_any_prerelease() {
        // The RC cycle bumps from the base version instead; this is the path a
        // package takes when it leaves a cycle behind entirely.
        let rc = semver::Version::parse("1.0.0-rc.1").unwrap();
        assert_eq!(apply_bump(&rc, Bump::Minor).to_string(), "1.1.0");
        assert_eq!(apply_bump(&rc, Bump::Patch).to_string(), "1.0.1");
    }

    #[test]
    fn renders_a_section_with_default_templates() {
        let config = ChangelogConfig::default();
        let fix = fragment("lat_core", "Fixed", "repair the flux capacitor");
        let add = fragment("lat_core", "Added", "grow more vines");
        let section = render_section(
            &config,
            "lat_core",
            "1.3.0",
            "lat_core-v1.3.0",
            "2026-07-11",
            &[&fix, &add],
        )
        .unwrap();
        assert_eq!(
            section,
            "## v1.3.0 - 2026-07-11\n\n### Added\n\n- grow more vines\n\n### Fixed\n\n- repair the flux capacitor\n"
        );
    }

    /// A configured category becomes a heading above the kinds, which drop a
    /// level to sit under it. Categories render in configured order — not the
    /// order the fragments happen to arrive in — exactly as kinds do.
    #[test]
    fn categories_group_above_kinds_in_configured_order() {
        let config = ChangelogConfig {
            categories: vec!["build".to_string(), "publish".to_string()],
            ..Default::default()
        };
        let publish = categorized("lat_cli", "Added", "publish", "--dry-run");
        let build_add = categorized("lat_cli", "Added", "build", "--watch");
        let build_fix = categorized("lat_cli", "Fixed", "build", "stop rebuilding deps");
        let section = render_section(
            &config,
            "lat_cli",
            "1.3.0",
            "lat_cli-v1.3.0",
            "2026-07-11",
            &[&publish, &build_add, &build_fix],
        )
        .unwrap();
        assert_eq!(
            section,
            "## v1.3.0 - 2026-07-11\n\
             \n### build\n\
             \n#### Added\n\n- --watch\n\
             \n#### Fixed\n\n- stop rebuilding deps\n\
             \n### publish\n\
             \n#### Added\n\n- --dry-run\n"
        );
    }

    /// Entries naming no category trail the named ones under one heading —
    /// including the generated ripple entries, which belong to no one part of
    /// a package.
    #[test]
    fn uncategorized_entries_render_last_under_their_label() {
        let config = ChangelogConfig {
            categories: vec!["build".to_string()],
            ..Default::default()
        };
        let loose = fragment("lat_cli", "Fixed", "a general fix");
        let build = categorized("lat_cli", "Added", "build", "--watch");
        let section = render_section(
            &config,
            "lat_cli",
            "1.3.0",
            "lat_cli-v1.3.0",
            "2026-07-11",
            &[&loose, &build],
        )
        .unwrap();
        assert_eq!(
            section,
            "## v1.3.0 - 2026-07-11\n\
             \n### build\n\
             \n#### Added\n\n- --watch\n\
             \n### Other\n\
             \n#### Fixed\n\n- a general fix\n"
        );
    }

    /// A category with nothing in it is skipped, the same way an empty kind is
    /// — configuring the axis does not commit a package to filling every
    /// heading in every release.
    #[test]
    fn empty_categories_are_skipped() {
        let config = ChangelogConfig {
            categories: vec!["build".to_string(), "publish".to_string()],
            ..Default::default()
        };
        let build = categorized("lat_cli", "Added", "build", "--watch");
        let section = render_section(
            &config,
            "lat_cli",
            "1.3.0",
            "lat_cli-v1.3.0",
            "2026-07-11",
            &[&build],
        )
        .unwrap();
        assert_eq!(
            section,
            "## v1.3.0 - 2026-07-11\n\n### build\n\n#### Added\n\n- --watch\n"
        );
        // No stray `Other` heading when nothing is uncategorized, either.
        assert!(!section.contains("Other"));
    }

    /// The uncategorized block is a category like any other as far as the
    /// templates are concerned, and both headings can see the category name.
    #[test]
    fn custom_category_templates_get_full_context() {
        let config = ChangelogConfig {
            categories: vec!["build".to_string()],
            category_format: "### `{{ category }}` in {{ name }}".to_string(),
            kind_format_override: Some("**{{ kind }}** ({{ category }})".to_string()),
            change_format: "* {{ body }} [{{ category }}]".to_string(),
            uncategorized_label: "Everything else".to_string(),
            ..Default::default()
        };
        let build = categorized("lat_cli", "Added", "build", "--watch");
        let loose = fragment("lat_cli", "Added", "something general");
        let section = render_section(
            &config,
            "lat_cli",
            "1.3.0",
            "lat_cli-v1.3.0",
            "2026-07-11",
            &[&build, &loose],
        )
        .unwrap();
        assert!(section.contains("### `build` in lat_cli"), "{section}");
        assert!(section.contains("**Added** (build)"), "{section}");
        assert!(section.contains("* --watch [build]"), "{section}");
        assert!(
            section.contains("### `Everything else` in lat_cli"),
            "{section}"
        );
        assert!(section.contains("* something general [Everything else]"));
    }

    /// The whole compatibility contract in one assertion: a fragment may carry
    /// a category, but with the axis switched off it changes nothing about
    /// what gets rendered.
    #[test]
    fn a_category_renders_nothing_when_no_categories_are_configured() {
        let config = ChangelogConfig::default();
        let build = categorized("lat_cli", "Added", "build", "--watch");
        let section = render_section(
            &config,
            "lat_cli",
            "1.3.0",
            "lat_cli-v1.3.0",
            "2026-07-11",
            &[&build],
        )
        .unwrap();
        assert_eq!(
            section,
            "## v1.3.0 - 2026-07-11\n\n### Added\n\n- --watch\n"
        );
    }

    /// The version heading must stay the only `## ` line in a section.
    /// `commands::tag::changelog_section` extracts release notes by scanning
    /// for `## ` and stopping at the next one, so a category heading at that
    /// level would truncate every set of release notes to nothing.
    #[test]
    fn categories_leave_the_version_heading_the_only_h2() {
        let config = ChangelogConfig {
            categories: vec!["build".to_string()],
            ..Default::default()
        };
        let build = categorized("lat_cli", "Added", "build", "--watch");
        let loose = fragment("lat_cli", "Fixed", "a general fix");
        let section = render_section(
            &config,
            "lat_cli",
            "1.3.0",
            "lat_cli-v1.3.0",
            "2026-07-11",
            &[&build, &loose],
        )
        .unwrap();
        let h2s: Vec<&str> = section
            .lines()
            .filter(|line| line.starts_with("## "))
            .collect();
        assert_eq!(h2s, ["## v1.3.0 - 2026-07-11"]);

        // And the extractor really does recover the whole categorized body.
        let notes = crate::commands::tag::changelog_section(&section, "1.3.0").unwrap();
        assert!(notes.contains("### build"), "{notes}");
        assert!(notes.contains("#### Added"), "{notes}");
        assert!(notes.contains("- a general fix"), "{notes}");
    }

    #[test]
    fn custom_templates_get_full_context() {
        let config = ChangelogConfig {
            version_format: "## {{ tag }} ({{ date }})".to_string(),
            kind_format_override: Some("**{{ kind | upper }}**".to_string()),
            change_format: "* {{ body }} [{{ kind }}]".to_string(),
            ..Default::default()
        };
        let add = fragment("p", "Added", "thing");
        let section =
            render_section(&config, "p", "0.2.0", "p-v0.2.0", "2026-01-01", &[&add]).unwrap();
        assert!(section.starts_with("## p-v0.2.0 (2026-01-01)\n"));
        assert!(section.contains("**ADDED**"));
        assert!(section.contains("* thing [Added]"));
    }

    #[test]
    fn invalid_template_is_a_clear_error() {
        let config = ChangelogConfig {
            version_format: "## {{ version".to_string(),
            ..Default::default()
        };
        let add = fragment("p", "Added", "x");
        let err = render_section(&config, "p", "0.2.0", "t", "d", &[&add]).unwrap_err();
        assert!(format!("{err:#}").contains("version_format"));
    }

    #[test]
    fn dependency_fragment_uses_the_configured_kind_and_body() {
        let config = ChangelogConfig::default();
        let generated = dependency_fragment(&config, "lat_mid", "lat_core", "1.3.0").unwrap();
        assert_eq!(generated.package, "lat_mid");
        assert_eq!(generated.kind, "Dependencies");
        assert_eq!(generated.body, "Updated lat_core to 1.3.0");
        assert!(generated.path.is_none(), "generated fragments have no file");
    }

    #[test]
    fn dependency_body_template_gets_full_context() {
        let config = ChangelogConfig {
            dependency_body: "{{ package }} now needs {{ dependency }} {{ dependency_version }}"
                .to_string(),
            ..Default::default()
        };
        let generated = dependency_fragment(&config, "lat_mid", "lat_core", "1.3.0").unwrap();
        assert_eq!(generated.body, "lat_mid now needs lat_core 1.3.0");
    }

    /// A pure ripple bumps by whatever `dependency_kind` is configured to bump,
    /// and a package's own larger bump still wins.
    #[test]
    fn generated_fragments_participate_in_the_max_bump_rule() {
        let config = ChangelogConfig::default();
        let generated = dependency_fragment(&config, "lat_mid", "lat_core", "1.3.0").unwrap();
        let added = fragment("lat_mid", "Added", "a feature");
        let next = |frags: Vec<&Fragment>| next_version("0.5.0", &frags, &config.kinds);
        assert_eq!(next(vec![&generated]), "0.5.1");
        assert_eq!(next(vec![&generated, &added]), "0.6.0");
    }

    #[test]
    fn consume_fragments_skips_generated_ones() {
        let config = ChangelogConfig::default();
        let generated = dependency_fragment(&config, "lat_mid", "lat_core", "1.3.0").unwrap();
        // `unused` does not exist; removing it would error if it were attempted.
        consume_fragments(&[&generated]).unwrap();
    }

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

    #[test]
    fn strip_header_keeps_the_body_verbatim() {
        assert_eq!(
            strip_header("# lat_core changelog\n\n## v1.0.0\n\n- a thing\n"),
            "## v1.0.0\n\n- a thing"
        );
        // A header-only stub has no history behind it.
        assert_eq!(strip_header("# lat_core changelog\n"), "");
        // A changelog that never had a header is all body.
        assert_eq!(strip_header("## v1.0.0\n"), "## v1.0.0");
    }

    #[test]
    fn civil_dates_are_correct() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // leap year
        assert_eq!(civil_from_days(19_723 + 59), (2024, 2, 29));
    }

    #[test]
    fn source_date_epoch_formats_as_utc_date() {
        // 2026-07-11T00:00:00Z
        unsafe { std::env::set_var("SOURCE_DATE_EPOCH", "1783728000") };
        assert_eq!(today(), "2026-07-11");
        unsafe { std::env::remove_var("SOURCE_DATE_EPOCH") };
    }
}
