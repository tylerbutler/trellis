//! The native changelog engine (changie subsumed — design §7, revised).
//!
//! Layout, under `[tools.trellis.changelog] dir` (default `.changes/`):
//!   unreleased/*.toml        one fragment per change: project, kind, body
//!   <package>/v<X.Y.Z>.md    batched version sections, rendered once
//! Each package's CHANGELOG.md is assembled from its header plus its version
//! sections, newest first. All formats are minijinja templates, so the
//! rendered shape is configurable without a second tool.

use crate::config::{Bump, ChangelogConfig, KindConfig};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;

// ---- fragments -------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Fragment {
    pub project: String,
    pub kind: String,
    pub body: String,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFragment {
    project: String,
    kind: String,
    body: String,
}

/// All unreleased fragments plus every problem found while reading them.
/// Callers decide whether problems are warnings (`changelog check` reports
/// them) or fatal (`version plan/apply` refuses — silently dropping a
/// fragment is exactly the drift this tool exists to prevent).
#[derive(Debug, Default)]
pub struct Fragments {
    pub fragments: Vec<Fragment>,
    pub problems: Vec<String>,
}

impl Fragments {
    pub fn for_project<'a>(&'a self, project: &'a str) -> impl Iterator<Item = &'a Fragment> {
        self.fragments.iter().filter(move |f| f.project == project)
    }

    pub fn count_for(&self, project: &str) -> usize {
        self.for_project(project).count()
    }
}

pub fn unreleased_dir(workspace: &Workspace) -> PathBuf {
    workspace
        .root
        .join(&workspace.config.changelog.dir)
        .join("unreleased")
}

fn versions_dir(workspace: &Workspace, project: &str) -> PathBuf {
    workspace
        .root
        .join(&workspace.config.changelog.dir)
        .join(project)
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
                result.problems.push(format!("fragment `{display}`: {err}"));
                continue;
            }
        };
        match workspace.member_index(&raw.project) {
            Some(idx) if workspace.members[idx].releasable => {}
            Some(_) => {
                result.problems.push(format!(
                    "fragment `{display}`: project `{}` is excluded from release by `@release`",
                    raw.project
                ));
                continue;
            }
            None => {
                result.problems.push(format!(
                    "fragment `{display}`: project `{}` is not a workspace member",
                    raw.project
                ));
                continue;
            }
        }
        if !kinds.iter().any(|k| k.label == raw.kind) {
            result.problems.push(format!(
                "fragment `{display}`: kind `{}` is not one of {}",
                raw.kind,
                kind_labels(kinds)
            ));
            continue;
        }
        if raw.body.trim().is_empty() {
            result
                .problems
                .push(format!("fragment `{display}`: body is empty"));
            continue;
        }
        result.fragments.push(Fragment {
            project: raw.project,
            kind: raw.kind,
            body: raw.body.trim().to_string(),
            path,
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

/// Write a new fragment file, picking an unused `<project>-<n>.toml` name.
pub fn write_fragment(
    workspace: &Workspace,
    project: &str,
    kind: &str,
    body: &str,
) -> Result<PathBuf> {
    let dir = unreleased_dir(workspace);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let mut doc = toml_edit::DocumentMut::new();
    doc["project"] = toml_edit::value(project);
    doc["kind"] = toml_edit::value(kind);
    doc["body"] = toml_edit::value(body);
    for n in 1u32.. {
        let path = dir.join(format!("{project}-{n}.toml"));
        if !path.exists() {
            std::fs::write(&path, doc.to_string())
                .with_context(|| format!("failed to write {}", path.display()))?;
            return Ok(path);
        }
    }
    unreachable!("some counter is always free")
}

// ---- version computation ----------------------------------------------------

/// The next version for a package: current bumped by the largest bump among
/// its fragments' kinds.
pub fn next_version(
    current: &str,
    fragments: &[&Fragment],
    kinds: &[KindConfig],
) -> Result<semver::Version> {
    let current = semver::Version::parse(current)
        .with_context(|| format!("`{current}` is not valid semver"))?;
    let bump = fragments
        .iter()
        .filter_map(|fragment| {
            kinds
                .iter()
                .find(|k| k.label == fragment.kind)
                .map(|k| k.bump)
        })
        .max()
        .context("no fragments to compute a version bump from")?;
    Ok(match bump {
        Bump::Major => semver::Version::new(current.major + 1, 0, 0),
        Bump::Minor => semver::Version::new(current.major, current.minor + 1, 0),
        Bump::Patch => semver::Version::new(current.major, current.minor, current.patch + 1),
    })
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

/// The shared context for all four changelog templates: `name`, `version`,
/// `date`, `tag`, `kind`, `body`. Fields not meaningful for a given template
/// are passed as empty strings, so every template can reference any of them.
fn context(
    name: &str,
    version: &str,
    date: &str,
    tag: &str,
    kind: &str,
    body: &str,
) -> minijinja::Value {
    minijinja::context! { name, version, date, tag, kind, body }
}

/// Render one version section: the version heading, then each kind (in
/// configured order) with its entries.
pub fn render_section(
    config: &ChangelogConfig,
    name: &str,
    version: &str,
    tag: &str,
    date: &str,
    fragments: &[&Fragment],
) -> Result<String> {
    let mut out = render(
        &config.version_format,
        "version-format",
        context(name, version, date, tag, "", ""),
    )?;
    out.push('\n');
    for kind in &config.kinds {
        let entries: Vec<&&Fragment> = fragments.iter().filter(|f| f.kind == kind.label).collect();
        if entries.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(&render(
            &config.kind_format,
            "kind-format",
            context(name, version, date, tag, &kind.label, ""),
        )?);
        out.push('\n');
        out.push('\n');
        for fragment in entries {
            out.push_str(&render(
                &config.change_format,
                "change-format",
                context(name, version, date, tag, &fragment.kind, &fragment.body),
            )?);
            out.push('\n');
        }
    }
    Ok(out)
}

// ---- batch + merge -----------------------------------------------------------

/// Render a package's complete CHANGELOG.md with an optional pending section.
pub fn render_merged_changelog(
    workspace: &Workspace,
    project: &str,
    pending: Option<(&semver::Version, &str)>,
) -> Result<String> {
    workspace
        .member_index(project)
        .with_context(|| format!("unknown package `{project}`"))?;
    let config = &workspace.config.changelog;
    let dir = versions_dir(workspace, project);

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
    if let Some((version, section)) = pending {
        sections.retain(|(existing, _)| existing != version);
        sections.push((version.clone(), section.to_string()));
    }
    sections.sort_by(|a, b| b.0.cmp(&a.0));

    let header = render_header(config, project)?;
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
        "header-format",
        context(name, "", "", "", "", ""),
    )
}

/// Write a pre-rendered version section and complete package changelog.
pub fn write_batch(
    workspace: &Workspace,
    project: &str,
    version: &semver::Version,
    section: &str,
    changelog: &str,
) -> Result<()> {
    let idx = workspace
        .member_index(project)
        .with_context(|| format!("unknown package `{project}`"))?;
    let dir = versions_dir(workspace, project);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let section_path = dir.join(format!("v{version}.md"));
    std::fs::write(&section_path, section)
        .with_context(|| format!("failed to write {}", section_path.display()))?;
    let path = workspace.members[idx].path.join("CHANGELOG.md");
    std::fs::write(&path, changelog).with_context(|| format!("failed to write {}", path.display()))
}

pub fn consume_fragments(fragments: &[&Fragment]) -> Result<()> {
    for fragment in fragments {
        std::fs::remove_file(&fragment.path)
            .with_context(|| format!("failed to remove {}", fragment.path.display()))?;
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

pub fn render_manifest_version(text: &str, next: &semver::Version) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("failed to parse gleam.toml")?;
    let Some(value) = doc.get_mut("version").and_then(|item| item.as_value_mut()) else {
        bail!("gleam.toml has no version field");
    };
    let mut replacement = toml_edit::Value::from(next.to_string());
    *replacement.decor_mut() = value.decor().clone();
    *value = replacement;
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ChangelogConfig;

    fn fragment(project: &str, kind: &str, body: &str) -> Fragment {
        Fragment {
            project: project.to_string(),
            kind: kind.to_string(),
            body: body.to_string(),
            path: PathBuf::from("unused"),
        }
    }

    #[test]
    fn next_version_uses_the_largest_bump() {
        let kinds = ChangelogConfig::default().kinds;
        let fixed = fragment("p", "Fixed", "x");
        let added = fragment("p", "Added", "y");
        let breaking = fragment("p", "Breaking", "z");
        let next =
            |frags: Vec<&Fragment>| next_version("1.2.3", &frags, &kinds).unwrap().to_string();
        assert_eq!(next(vec![&fixed]), "1.2.4");
        assert_eq!(next(vec![&fixed, &added]), "1.3.0");
        assert_eq!(next(vec![&fixed, &added, &breaking]), "2.0.0");
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

    #[test]
    fn custom_templates_get_full_context() {
        let config = ChangelogConfig {
            version_format: "## {{ tag }} ({{ date }})".to_string(),
            kind_format: "**{{ kind | upper }}**".to_string(),
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
    fn header_format_gets_the_same_context_shape_as_the_other_templates() {
        let config = ChangelogConfig {
            header_format: "# {{ name }}{{ version }}{{ date }}{{ tag }}{{ kind }}{{ body }}!"
                .to_string(),
            ..Default::default()
        };
        assert_eq!(render_header(&config, "p").unwrap(), "# p!");
    }

    #[test]
    fn invalid_template_is_a_clear_error() {
        let config = ChangelogConfig {
            version_format: "## {{ version".to_string(),
            ..Default::default()
        };
        let add = fragment("p", "Added", "x");
        let err = render_section(&config, "p", "0.2.0", "t", "d", &[&add]).unwrap_err();
        assert!(format!("{err:#}").contains("version-format"));
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
