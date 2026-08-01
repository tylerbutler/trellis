//! `trellis tag` — compare each releasable member's gleam.toml version
//! against existing tags and reconcile the difference, in topological order.
//!
//! A member tags in one of two lifecycles, chosen by `tag_mode`: immutable
//! `{name}-v{version}` tags, created once and never touched again, and moving
//! `{name}-v{series}` tags, force-moved to the release commit every time that
//! series releases. Only the immutable ones can carry a GitHub Release — a
//! release bound to a moving tag would silently retarget.

use crate::github::GitHubClient;
use crate::json::TagPlanDocument;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

/// Which tag lifecycle a planned tag belongs to. The serialized names are wire
/// format — see `crate::json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TagKind {
    /// An immutable `tag_format` tag naming one version.
    Exact,
    /// A moving `series_tag_format` tag naming a release series.
    Series,
}

/// The work `tag create` would do for a planned tag, from local state alone.
/// The serialized names are wire format — see `crate::json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TagAction {
    /// The tag does not exist locally.
    Create,
    /// A series tag exists locally but points somewhere other than HEAD.
    Move,
    /// The tag already points where it should; only the remote may need work.
    UpToDate,
}

#[derive(Debug)]
pub struct PlannedTag {
    /// Index into `workspace.members`.
    pub member: usize,
    pub tag: String,
    pub kind: TagKind,
    pub action: TagAction,
}

/// Every tag the current versions call for, in topological order, each with
/// the work it needs. A repository-wide series tag (a `series_tag_format`
/// without `{name}`) is claimed by the first member that would produce it, so
/// it is moved once rather than once per package.
///
/// Local-only and read-only — no git ref is written and no remote is
/// queried. `tag plan`, `tag create`, and `release bootstrap` all start from
/// this same reconciliation, so a preview can never disagree with what
/// execution actually does.
pub(crate) fn plan_tags(workspace: &Workspace) -> Result<Vec<PlannedTag>> {
    let existing = git_stdout(&workspace.root, &["tag", "--list"])?;
    let existing: HashSet<&str> = existing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Unborn in a repository with no commits, where nothing can be tagged yet.
    let head = commit_of(&workspace.root, "HEAD")?;

    let mut planned: Vec<PlannedTag> = Vec::new();
    let mut claimed: HashSet<String> = HashSet::new();
    for (member, index) in workspace
        .members
        .iter()
        .zip(0..)
        .filter(|(member, _)| member.releasable)
    {
        if member.tag_mode.includes_exact() {
            let tag = workspace.config.format_tag(&member.name, member.version());
            let action = if existing.contains(tag.as_str()) {
                TagAction::UpToDate
            } else {
                TagAction::Create
            };
            if claimed.insert(tag.clone()) {
                planned.push(PlannedTag {
                    member: index,
                    tag,
                    kind: TagKind::Exact,
                    action,
                });
            }
        }
        // A prerelease belongs to no series, and so moves no tag.
        if member.tag_mode.includes_series()
            && let Some(tag) = workspace
                .config
                .format_series_tag(&member.name, member.version())
        {
            let action = if !existing.contains(tag.as_str()) {
                TagAction::Create
            } else if head.is_some() && commit_of(&workspace.root, &tag)? == head {
                TagAction::UpToDate
            } else {
                TagAction::Move
            };
            if claimed.insert(tag.clone()) {
                planned.push(PlannedTag {
                    member: index,
                    tag,
                    kind: TagKind::Series,
                    action,
                });
            }
        }
    }
    Ok(planned)
}

pub fn plan(workspace: &Workspace, json: bool) -> Result<()> {
    let pending: Vec<PlannedTag> = plan_tags(workspace)?
        .into_iter()
        .filter(|planned| planned.action != TagAction::UpToDate)
        .collect();
    if json {
        let document = TagPlanDocument {
            schema: TagPlanDocument::SCHEMA,
            tags: pending
                .iter()
                .map(|planned| {
                    let member = &workspace.members[planned.member];
                    crate::json::PlannedTag {
                        name: &member.name,
                        version: member.version(),
                        tag: &planned.tag,
                        kind: planned.kind,
                        action: planned.action,
                    }
                })
                .collect(),
        };
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else if pending.is_empty() {
        crate::status!("every releasable package version is already tagged");
    } else {
        for planned in &pending {
            let member = &workspace.members[planned.member];
            let verb = match planned.action {
                TagAction::Move => "moves tag",
                _ => "needs tag",
            };
            crate::status!(
                "{}: {} {verb} {}",
                member.name,
                member.version(),
                planned.tag
            );
        }
    }
    Ok(())
}

pub struct CreateOptions {
    pub push: bool,
    pub github_release: bool,
    /// Report every planned action without doing anything (a conflicting tag
    /// still fails the command).
    pub dry_run: bool,
}

pub fn create(workspace: &Workspace, options: &CreateOptions) -> Result<()> {
    let push = options.push || options.github_release;
    let planned = plan_tags(workspace)?;
    // Without a remote to reconcile, a tag that already points where it
    // should is nothing to do; with one, it may still be missing from origin.
    let targets: Vec<&PlannedTag> = planned
        .iter()
        .filter(|planned| push || planned.action != TagAction::UpToDate)
        .collect();
    if targets.is_empty() {
        crate::status!("every releasable package version is already tagged");
        return Ok(());
    }

    // Resolved once, up front: a missing token or non-GitHub origin should
    // fail before any tag is created or pushed, not between two of them.
    let github = if options.github_release {
        Some(GitHubClient::for_repo(&workspace.root)?)
    } else {
        None
    };

    // One `ls-remote` answers "what does origin have?" for every planned tag
    // at once — a single network round trip that the conflict preflight and
    // the per-tag actions below all read from.
    let remote_oids = if push {
        let tags: Vec<&str> = targets.iter().map(|planned| planned.tag.as_str()).collect();
        remote_tag_oids(&workspace.root, &tags)?
    } else {
        HashMap::new()
    };

    // An immutable tag whose local and remote objects disagree is the one
    // conflict trellis refuses to resolve. Check every planned tag before
    // mutating any of them, so one package's conflict fails the whole run
    // rather than leaving an earlier package half-tagged.
    if push {
        let mut conflicts = Vec::new();
        for planned in &targets {
            if planned.kind != TagKind::Exact {
                continue;
            }
            let tag = &planned.tag;
            if let (Some(local), Some(remote)) =
                (local_tag_oid(&workspace.root, tag)?, remote_oids.get(tag))
                && local != *remote
            {
                conflicts.push(format!(
                    "tag `{tag}` points to different objects locally ({local}) and on origin ({remote})"
                ));
            }
        }
        if !conflicts.is_empty() {
            bail!("{}", conflicts.join("; "));
        }
    }

    for planned in targets {
        let remote_oid = remote_oids.get(&planned.tag).map(String::as_str);
        match planned.kind {
            TagKind::Exact => create_exact_tag(
                workspace,
                planned,
                options,
                github.as_ref(),
                push,
                remote_oid,
            )?,
            TagKind::Series => move_series_tag(workspace, planned, options, push, remote_oid)?,
        }
    }
    Ok(())
}

/// An immutable tag: created once, fetched when origin already has it, and
/// never rewritten. Local and remote disagreeing about what it names is a
/// history problem trellis refuses to paper over.
fn create_exact_tag(
    workspace: &Workspace,
    planned: &PlannedTag,
    options: &CreateOptions,
    github: Option<&GitHubClient>,
    push: bool,
    remote_oid: Option<&str>,
) -> Result<()> {
    let member = &workspace.members[planned.member];
    let tag = &planned.tag;
    // `plan_tags` already read the local tag list, so `action` says whether
    // the tag exists here; local/remote divergence was rejected by `create`'s
    // preflight.
    if planned.action == TagAction::Create {
        if remote_oid.is_some() {
            if options.dry_run {
                crate::status!("would fetch {tag}");
            } else {
                git_stdout(&workspace.root, &["fetch", "origin", "tag", tag])?;
                crate::status!("fetched {tag}");
            }
        } else if options.dry_run {
            crate::status!("would tag {tag}");
        } else {
            let mut args = crate::git::identity_fallback_args(&workspace.root);
            args.extend([
                "tag".into(),
                "-a".into(),
                tag.clone(),
                "-m".into(),
                format!("{} {}", member.name, member.version()),
            ]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            git_stdout(&workspace.root, &args)?;
            crate::status!("tagged {tag}");
        }
    }
    if push && remote_oid.is_none() {
        if options.dry_run {
            crate::status!("would push {tag}");
        } else {
            git_stdout(&workspace.root, &["push", "origin", tag])
                .with_context(|| format!("failed to push tag {tag}"))?;
            crate::status!("pushed {tag}");
        }
    }
    if let Some(github) = github {
        if github.release_exists(tag)? {
            crate::status!("GitHub release {tag} already exists; skipping");
        } else if options.dry_run {
            crate::status!("would create GitHub release {tag}");
        } else {
            let notes = release_notes(workspace, planned.member);
            github.create_release(tag, tag, &notes)?;
            crate::status!("created GitHub release {tag}");
        }
    }
    Ok(())
}

/// A series tag is the one ref trellis rewrites: it is force-moved to the
/// release commit and force-pushed. Local and remote pointing at different
/// objects is the normal state between releases, not an error, and no GitHub
/// Release is ever attached — it would silently retarget on the next move.
fn move_series_tag(
    workspace: &Workspace,
    planned: &PlannedTag,
    options: &CreateOptions,
    push: bool,
    remote_oid: Option<&str>,
) -> Result<()> {
    let member = &workspace.members[planned.member];
    let tag = &planned.tag;
    let moved = planned.action != TagAction::UpToDate;
    if moved {
        let (verb, done) = if planned.action == TagAction::Create {
            ("tag", "tagged")
        } else {
            ("move", "moved")
        };
        if options.dry_run {
            crate::status!("would {verb} {tag}");
        } else {
            let mut args = crate::git::identity_fallback_args(&workspace.root);
            args.extend([
                "tag".into(),
                "-f".into(),
                "-a".into(),
                tag.clone(),
                "-m".into(),
                format!("{} {}", member.name, member.version()),
            ]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            git_stdout(&workspace.root, &args)?;
            crate::status!("{done} {tag}");
        }
    }
    if push {
        // Re-read after any move: a re-tag writes a fresh annotated object, so
        // it never matches origin (in a dry run `moved` stands in for that);
        // the comparison also catches "already where it belongs, but origin
        // disagrees".
        let local_oid = local_tag_oid(&workspace.root, tag)?;
        if moved || local_oid.as_deref() != remote_oid {
            let verb = if remote_oid.is_none() {
                "push"
            } else {
                "force-push"
            };
            if options.dry_run {
                crate::status!("would {verb} {tag}");
            } else {
                git_stdout(&workspace.root, &["push", "--force", "origin", tag])
                    .with_context(|| format!("failed to push tag {tag}"))?;
                crate::status!("{verb}ed {tag}");
            }
        }
    }
    Ok(())
}

fn local_tag_oid(root: &Path, tag: &str) -> Result<Option<String>> {
    rev_parse(root, &format!("refs/tags/{tag}"), tag)
}

/// The commit a revision points at, peeling annotated tags — the comparison a
/// series tag needs, since re-tagging the same commit still writes a fresh tag
/// object. `None` when the revision doesn't exist, including an unborn HEAD.
fn commit_of(root: &Path, revision: &str) -> Result<Option<String>> {
    rev_parse(root, &format!("{revision}^{{commit}}"), revision)
}

fn rev_parse(root: &Path, reference: &str, subject: &str) -> Result<Option<String>> {
    let args = ["rev-parse", "--verify", "--quiet", reference];
    crate::term::trace_command("git", &args, root);
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .context("failed to run git")?;
    match output.status.code() {
        Some(0) => output
            .stdout
            .split(|byte| byte.is_ascii_whitespace())
            .find(|part| !part.is_empty())
            .map(|oid| String::from_utf8_lossy(oid).into_owned())
            .map(Some)
            .context("git rev-parse returned no object ID"),
        Some(1) => Ok(None),
        _ => bail!("git rev-parse failed while checking `{subject}`"),
    }
}

/// The object each requested tag names on origin, from one `ls-remote` for
/// the whole batch. Tags origin doesn't have are absent from the map. Peeled
/// `^{}` entries are skipped so the oid is the tag object itself — the same
/// object `local_tag_oid` reports.
fn remote_tag_oids(root: &Path, tags: &[&str]) -> Result<HashMap<String, String>> {
    let refs: Vec<String> = tags.iter().map(|tag| format!("refs/tags/{tag}")).collect();
    let mut args = vec!["ls-remote", "--tags", "origin"];
    args.extend(refs.iter().map(String::as_str));
    let stdout = git_stdout(root, &args)?;
    let mut oids = HashMap::new();
    for line in stdout.lines() {
        if let Some((oid, reference)) = line.split_once('\t')
            && let Some(tag) = reference.strip_prefix("refs/tags/")
            && !tag.ends_with("^{}")
        {
            oids.insert(tag.to_string(), oid.to_string());
        }
    }
    Ok(oids)
}

/// The member's CHANGELOG section for its current version, or a minimal
/// fallback body.
fn release_notes(workspace: &Workspace, idx: usize) -> String {
    let member = &workspace.members[idx];
    std::fs::read_to_string(member.path.join("CHANGELOG.md"))
        .ok()
        .and_then(|text| changelog_section(&text, member.version()))
        .unwrap_or_else(|| format!("{} {}", member.name, member.version()))
}

/// Extract the `## …` section whose heading names `version`, using the same
/// tolerant heading forms as the doctor check (`## 1.2.3`, `## [1.2.3]`,
/// `## name-v1.2.3 - date`).
pub fn changelog_section(text: &str, version: &str) -> Option<String> {
    let Ok(wanted) = semver::Version::parse(version) else {
        return None;
    };
    let mut section: Option<String> = None;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if section.is_some() {
                break; // next section starts; we're done
            }
            if heading_version(heading) == Some(wanted.clone()) {
                section = Some(String::new());
            }
        } else if let Some(section) = section.as_mut() {
            section.push_str(line);
            section.push('\n');
        }
    }
    section
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn heading_version(heading: &str) -> Option<semver::Version> {
    let token = heading.split_whitespace().next()?;
    let token = token.trim_matches(['[', ']']);
    let token = token.rsplit_once("-v").map(|(_, v)| v).unwrap_or(token);
    let token = token.strip_prefix('v').unwrap_or(token);
    semver::Version::parse(token).ok()
}

/// A pushed tag resolved back to the package it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTag {
    /// An immutable tag, carrying the version it claims — which may differ
    /// from gleam.toml; the caller decides whether that's fatal.
    Exact { member: usize, version: String },
    /// A moving series tag, carrying the series it names. It identifies a
    /// package but no particular version.
    Series { member: usize, series: String },
}

impl ResolvedTag {
    pub fn member(&self) -> usize {
        match self {
            ResolvedTag::Exact { member, .. } | ResolvedTag::Series { member, .. } => *member,
        }
    }
}

/// Resolve a tag like `lat_core-v1.2.0` or `lat_core-v0.3` to the releasable
/// member it belongs to. Exact tags are tried first; a candidate only counts
/// if what the template captured is actually a version (or, for series tags, a
/// series), so `lat_core-v0.3` doesn't resolve as version "0.3".
pub fn resolve_tag(workspace: &Workspace, tag: &str) -> Result<ResolvedTag> {
    let exact = match_tag_template(
        &candidates(workspace, |mode| mode.includes_exact()),
        tag,
        &workspace.config.publish.tag_format,
        "{version}",
        |captured| semver::Version::parse(captured).is_ok(),
    )?;
    if let Some((member, version)) = exact.into_iter().next() {
        return Ok(ResolvedTag::Exact { member, version });
    }

    let series = match_tag_template(
        &candidates(workspace, |mode| mode.includes_series()),
        tag,
        &workspace.config.publish.series_tag_format,
        "{series}",
        is_series,
    )?;
    if series.len() > 1 && workspace.config.series_tag_is_repo_wide() {
        bail!(
            "tag `{tag}` is a repository-wide series tag shared by {}; it names no single package",
            series
                .iter()
                .map(|(member, _)| format!("`{}`", workspace.members[*member].name))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some((member, series)) = series.into_iter().next() {
        return Ok(ResolvedTag::Series { member, series });
    }

    bail!(
        "tag `{tag}` does not match any releasable package (tag format: {}, series tag format: {})",
        workspace.config.publish.tag_format,
        workspace.config.publish.series_tag_format
    )
}

/// Releasable members whose tag mode passes `wanted`, as `match_tag_template`
/// candidates.
fn candidates(
    workspace: &Workspace,
    wanted: impl Fn(crate::config::TagMode) -> bool,
) -> Vec<(usize, &str)> {
    workspace
        .members
        .iter()
        .zip(0..)
        .filter(|(member, _)| member.releasable && wanted(member.tag_mode))
        .map(|(member, index)| (index, member.name.as_str()))
        .collect()
}

/// Match `tag` against `template` once per candidate, with `{name}`
/// substituted, and return what `placeholder` captured. Longest package name
/// first, so `lat_core_extra-v1.0.0` never resolves to `lat_core`.
fn match_tag_template(
    candidates: &[(usize, &str)],
    tag: &str,
    template: &str,
    placeholder: &str,
    accept: impl Fn(&str) -> bool,
) -> Result<Vec<(usize, String)>> {
    let Some((prefix_template, suffix_template)) = template.split_once(placeholder) else {
        bail!("tag format `{template}` has no {placeholder} placeholder");
    };
    let mut matches: Vec<(usize, &str, String)> = Vec::new();
    for (index, name) in candidates {
        let prefix = prefix_template.replace("{name}", name);
        let suffix = suffix_template.replace("{name}", name);
        if tag.len() > prefix.len() + suffix.len()
            && tag.starts_with(&prefix)
            && tag.ends_with(&suffix)
        {
            let captured = &tag[prefix.len()..tag.len() - suffix.len()];
            if accept(captured) {
                matches.push((*index, name, captured.to_string()));
            }
        }
    }
    matches.sort_by_key(|(_, name, _)| std::cmp::Reverse(name.len()));
    Ok(matches
        .into_iter()
        .map(|(index, _, captured)| (index, captured))
        .collect())
}

/// A series is `X` or `0.Y` — one or two all-numeric parts. Requiring the
/// shape is what keeps the two tag formats from claiming each other's tags
/// when they share a prefix, as the defaults do.
fn is_series(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    (1..=2).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    crate::term::trace_command("git", args, cwd);
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{changelog_section, is_series, match_tag_template};

    fn packages() -> Vec<(usize, &'static str)> {
        vec![(0, "lat_core"), (1, "lat_core_extra")]
    }

    fn versions(tag: &str) -> Vec<(usize, String)> {
        match_tag_template(&packages(), tag, "{name}-v{version}", "{version}", |c| {
            semver::Version::parse(c).is_ok()
        })
        .unwrap()
    }

    fn series(tag: &str) -> Vec<(usize, String)> {
        match_tag_template(&packages(), tag, "{name}-v{series}", "{series}", is_series).unwrap()
    }

    #[test]
    fn longest_package_name_matches_first() {
        assert_eq!(
            versions("lat_core_extra-v1.0.0"),
            vec![(1, "1.0.0".to_string())]
        );
        assert_eq!(versions("lat_core-v1.2.0"), vec![(0, "1.2.0".to_string())]);
    }

    #[test]
    fn the_two_tag_formats_do_not_claim_each_others_tags() {
        // `lat_core-v0.3` fits the exact template too, but "0.3" is not a
        // version — so only the series template matches it, and vice versa.
        assert!(versions("lat_core-v0.3").is_empty());
        assert_eq!(series("lat_core-v0.3"), vec![(0, "0.3".to_string())]);
        assert!(series("lat_core-v1.2.0").is_empty());
        assert_eq!(series("lat_core-v2"), vec![(0, "2".to_string())]);
    }

    #[test]
    fn a_repo_wide_series_tag_matches_every_candidate() {
        let matches =
            match_tag_template(&packages(), "v0.0", "v{series}", "{series}", is_series).unwrap();
        assert_eq!(matches.len(), 2, "ambiguous by construction: {matches:?}");
    }

    #[test]
    fn unmatched_and_malformed_templates() {
        assert!(versions("some-other-tag").is_empty());
        let err = match_tag_template(&packages(), "v1", "{name}-latest", "{version}", |_| true)
            .unwrap_err();
        assert!(err.to_string().contains("{version}"), "{err:#}");
    }

    #[test]
    fn series_tokens_are_one_or_two_numbers() {
        assert!(is_series("0") && is_series("0.0") && is_series("12") && is_series("0.12"));
        assert!(!is_series("1.2.3"));
        assert!(!is_series("0.0-rc"));
        assert!(!is_series(""));
        assert!(!is_series("0."));
        assert!(!is_series("v1"));
    }

    #[test]
    fn extracts_the_matching_section_only() {
        let text = concat!(
            "# Changelog\n\n",
            "## lat_core-v1.3.0 - 2026-07-01\n\n",
            "### Added\n* new thing\n\n",
            "## [1.2.0]\n\n* older\n",
        );
        assert_eq!(
            changelog_section(text, "1.3.0").unwrap(),
            "### Added\n* new thing"
        );
        assert_eq!(changelog_section(text, "1.2.0").unwrap(), "* older");
        assert_eq!(changelog_section(text, "9.9.9"), None);
    }
}
