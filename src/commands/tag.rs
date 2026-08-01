//! `trellis tag` — compare each releasable member's gleam.toml version
//! against existing tags and reconcile the difference, in topological order.
//!
//! A member tags in one of two lifecycles, chosen by `tag_mode`: immutable
//! `{name}-v{version}` tags, created once and never touched again, and moving
//! `{name}-v{series}` tags, force-moved to the release commit every time that
//! series releases. An optional repository series tag follows one anchor
//! package's manifest version independently of those modes. Only immutable
//! tags can carry a GitHub Release.

use crate::gleam::GleamManifest;
use crate::json::TagPlanDocument;
use crate::tools;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::HashSet;
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
    /// A moving repository tag whose lifecycle is anchored to one package.
    RepositorySeries,
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
    pub version: String,
    pub tag: String,
    pub kind: TagKind,
    pub action: TagAction,
}

/// Every tag the current versions call for, in topological order, each with
/// the work it needs. A legacy repository-wide package series tag (a
/// `series_tag_format` without `{name}`) is still claimed only once.
fn plan_tags(workspace: &Workspace) -> Result<Vec<PlannedTag>> {
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
                    version: member.version().to_string(),
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
                    version: member.version().to_string(),
                    tag,
                    kind: TagKind::Series,
                    action,
                });
            }
        }
    }
    if let Some(repository_series) = &workspace.config.publish.repository_series {
        let (index, anchor) = workspace
            .members
            .iter()
            .enumerate()
            .find(|(_, member)| member.name == repository_series.package)
            .with_context(|| {
                format!(
                    "repository series anchor package `{}` is not a workspace member",
                    repository_series.package
                )
            })?;
        let anchor_version = manifest_version_at_revision(workspace, anchor, "HEAD")?;
        if let Some(tag) = workspace
            .config
            .format_repository_series_tag(&anchor_version)
        {
            reject_repository_tag_package_collision(workspace, &tag)?;
            let action = if !existing.contains(tag.as_str()) {
                TagAction::Create
            } else if manifest_version_at_revision(workspace, anchor, &tag)? == anchor_version {
                TagAction::UpToDate
            } else {
                TagAction::Move
            };
            planned.push(PlannedTag {
                member: index,
                version: anchor_version,
                tag,
                kind: TagKind::RepositorySeries,
                action,
            });
        }
    }
    Ok(planned)
}

/// A repository tag must never occupy a package-tag namespace. `doctor`
/// reports the same configuration mistake, but mutating commands defend the
/// immutable package tags even when users have not run it first.
fn reject_repository_tag_package_collision(workspace: &Workspace, tag: &str) -> Result<()> {
    let all_packages: Vec<(usize, &str)> = workspace
        .members
        .iter()
        .enumerate()
        .filter(|(_, member)| member.releasable)
        .map(|(index, member)| (index, member.name.as_str()))
        .collect();
    let exact = match_tag_template(
        &all_packages,
        tag,
        &workspace.config.publish.tag_format,
        "{version}",
        |captured| semver::Version::parse(captured).is_ok(),
    )?;
    let series = match_tag_template(
        &all_packages,
        tag,
        &workspace.config.publish.series_tag_format,
        "{series}",
        is_series,
    )?;
    if !exact.is_empty() || !series.is_empty() {
        bail!(
            "repository series tag `{tag}` collides with a package tag namespace; \
             choose a distinct `repository_series.format`"
        );
    }
    Ok(())
}

/// Read the anchor's manifest from the commit named by an existing repository
/// tag. The version in that file — not package exact tags or `tag_mode` — is
/// the repository tag's release signal.
fn manifest_version_at_revision(
    workspace: &Workspace,
    anchor: &crate::workspace::Member,
    revision: &str,
) -> Result<String> {
    let manifest = if anchor.rel_path == "." {
        crate::workspace::GLEAM_TOML.to_string()
    } else {
        format!("{}/{}", anchor.rel_path, crate::workspace::GLEAM_TOML)
    };
    let object = format!("{revision}:{manifest}");
    let text = git_stdout(&workspace.root, &["show", &object]).with_context(|| {
        format!(
            "cannot read repository series anchor manifest `{manifest}` at revision `{revision}`"
        )
    })?;
    GleamManifest::parse(&text)
        .with_context(|| {
            format!(
                "cannot parse repository series anchor manifest `{manifest}` at revision `{revision}`"
            )
        })
        .map(|manifest| manifest.version)
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
                        version: &planned.version,
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
                planned.version,
                planned.tag
            );
        }
    }
    Ok(())
}

pub struct CreateOptions {
    pub push: bool,
    pub github_release: bool,
}

pub fn create(workspace: &Workspace, options: &CreateOptions) -> Result<()> {
    let push = options.push || options.github_release;
    if push {
        reconcile_remote_repository_series_tag(workspace)?;
    }
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

    /// Reconcile the repository series tag from origin before planning a push.
    /// The remote anchor version is authoritative: a stale clone may advance it,
    /// but may never move it backward.
    fn reconcile_remote_repository_series_tag(workspace: &Workspace) -> Result<()> {
        let Some(repository_series) = &workspace.config.publish.repository_series else {
            return Ok(());
        };
        let (_, anchor) = workspace
            .members
            .iter()
            .enumerate()
            .find(|(_, member)| member.name == repository_series.package)
            .with_context(|| {
                format!(
                    "repository series anchor package `{}` is not a workspace member",
                    repository_series.package
                )
            })?;
        let anchor_version = manifest_version_at_revision(workspace, anchor, "HEAD")?;
        let Some(tag) = workspace
            .config
            .format_repository_series_tag(&anchor_version)
        else {
            return Ok(());
        };
        if remote_tag_oid(&workspace.root, &tag)?.is_none() {
            return Ok(());
        }

        const REMOTE_REF: &str = "refs/trellis/repository-series";
        let source = format!("refs/tags/{tag}:{REMOTE_REF}");
        git_stdout(&workspace.root, &["fetch", "--force", "origin", &source])?;
        let reconcile = (|| {
            let remote_version = manifest_version_at_revision(workspace, anchor, REMOTE_REF)?;
            let head_version = semver::Version::parse(&anchor_version).with_context(|| {
                format!("invalid anchor package version `{anchor_version}` at HEAD")
            })?;
            let remote_version_parsed =
                semver::Version::parse(&remote_version).with_context(|| {
                    format!(
                        "invalid anchor package version `{remote_version}` at remote tag `{tag}`"
                    )
                })?;
            if remote_version_parsed > head_version {
                bail!(
                    "repository series tag `{tag}` on origin is anchored at newer {} version \
                     `{remote_version}`; refusing to move it backward to `{anchor_version}`",
                    anchor.name
                );
            }

            let remote_oid = rev_parse(&workspace.root, REMOTE_REF, REMOTE_REF)?
                .context("fetched repository series tag has no object ID")?;
            if local_tag_oid(&workspace.root, &tag)?.as_deref() != Some(remote_oid.as_str()) {
                let local_ref = format!("refs/tags/{tag}");
                git_stdout(&workspace.root, &["update-ref", &local_ref, &remote_oid])?;
                crate::status!("fetched {tag}");
            }
            Ok(())
        })();
        let cleanup = git_stdout(&workspace.root, &["update-ref", "-d", REMOTE_REF]);
        reconcile.and(cleanup.map(|_| ()))
    }

    for planned in targets {
        match planned.kind {
            TagKind::Exact => create_exact_tag(workspace, planned, options, push)?,
            TagKind::Series | TagKind::RepositorySeries => {
                move_series_tag(workspace, planned, push)?
            }
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
    push: bool,
) -> Result<()> {
    let member = &workspace.members[planned.member];
    let tag = &planned.tag;
    let local_oid = local_tag_oid(&workspace.root, tag)?;
    let remote_oid = if push {
        remote_tag_oid(&workspace.root, tag)?
    } else {
        None
    };
    if let (Some(local), Some(remote)) = (&local_oid, &remote_oid)
        && local != remote
    {
        bail!("tag `{tag}` points to different objects locally ({local}) and on origin ({remote})");
    }
    if local_oid.is_none() {
        if remote_oid.is_some() {
            git_stdout(&workspace.root, &["fetch", "origin", "tag", tag])?;
            crate::status!("fetched {tag}");
        } else {
            let mut args = crate::git::identity_fallback_args(&workspace.root);
            args.extend([
                "tag".into(),
                "-a".into(),
                tag.clone(),
                "-m".into(),
                format!("{} {}", member.name, planned.version),
            ]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            git_stdout(&workspace.root, &args)?;
            crate::status!("tagged {tag}");
        }
    }
    if push && remote_oid.is_none() {
        git_stdout(&workspace.root, &["push", "origin", tag])
            .with_context(|| format!("failed to push tag {tag}"))?;
        crate::status!("pushed {tag}");
    }
    if options.github_release {
        if github_release_exists(&workspace.root, tag)? {
            crate::status!("GitHub release {tag} already exists; skipping");
        } else {
            let notes = release_notes(workspace, planned.member);
            let gh = tools::gh_bin();
            let args = ["release", "create", tag, "--title", tag, "--notes", &notes];
            crate::term::trace_command(&gh, &args, &workspace.root);
            let output = Command::new(&gh)
                .args(args)
                .current_dir(&workspace.root)
                .output()
                .with_context(|| format!("failed to run `{gh}` — is the GitHub CLI installed?"))?;
            if !output.status.success() {
                bail!(
                    "`{gh} release create {tag}` failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            crate::status!("created GitHub release {tag}");
        }
    }
    Ok(())
}

/// A series tag is the one ref trellis rewrites: it is force-moved to the
/// release commit and force-pushed. Local and remote pointing at different
/// objects is the normal state between releases, not an error, and no GitHub
/// Release is ever attached — it would silently retarget on the next move.
fn move_series_tag(workspace: &Workspace, planned: &PlannedTag, push: bool) -> Result<()> {
    let member = &workspace.members[planned.member];
    let tag = &planned.tag;
    let remote_oid = if push {
        remote_tag_oid(&workspace.root, tag)?
    } else {
        None
    };
    if planned.action != TagAction::UpToDate {
        let mut args = crate::git::identity_fallback_args(&workspace.root);
        args.extend([
            "tag".into(),
            "-f".into(),
            "-a".into(),
            tag.clone(),
            "-m".into(),
            format!("{} {}", member.name, planned.version),
        ]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git_stdout(&workspace.root, &args)?;
        if planned.action == TagAction::Create {
            crate::status!("tagged {tag}");
        } else {
            crate::status!("moved {tag}");
        }
    }
    if push {
        // Re-read after any move: a re-tag writes a fresh annotated object, so
        // this also catches "already where it belongs, but origin disagrees".
        let local_oid = local_tag_oid(&workspace.root, tag)?;
        if local_oid != remote_oid {
            if planned.kind == TagKind::RepositorySeries {
                let lease = format!(
                    "--force-with-lease=refs/tags/{tag}:{}",
                    remote_oid.as_deref().unwrap_or("")
                );
                git_stdout(&workspace.root, &["push", &lease, "origin", tag])
                    .with_context(|| format!("failed to push tag {tag}"))?;
            } else {
                git_stdout(&workspace.root, &["push", "--force", "origin", tag])
                    .with_context(|| format!("failed to push tag {tag}"))?;
            }
            if remote_oid.is_none() {
                crate::status!("pushed {tag}");
            } else {
                crate::status!("force-pushed {tag}");
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

fn remote_tag_oid(root: &Path, tag: &str) -> Result<Option<String>> {
    let reference = format!("refs/tags/{tag}");
    let args = ["ls-remote", "--exit-code", "--tags", "origin", &reference];
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
            .context("git ls-remote returned no object ID"),
        Some(2) => Ok(None),
        _ => bail!(
            "git ls-remote failed while checking tag `{tag}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn github_release_exists(root: &Path, tag: &str) -> Result<bool> {
    let gh = tools::gh_bin();
    let args = ["release", "view", tag, "--json", "tagName"];
    crate::term::trace_command(&gh, &args, root);
    let output = Command::new(&gh)
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to run `{gh}` — is the GitHub CLI installed?"))?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("release not found") {
        return Ok(false);
    }
    bail!("`{gh} release view {tag}` failed: {}", stderr.trim())
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
    if let Some(repository_series) = &workspace.config.publish.repository_series {
        let anchor = workspace
            .members
            .iter()
            .enumerate()
            .find(|(_, member)| member.name == repository_series.package)
            .map(|(index, member)| vec![(index, member.name.as_str())])
            .unwrap_or_default();
        if !match_tag_template(
            &anchor,
            tag,
            &repository_series.format,
            "{series}",
            is_series,
        )?
        .is_empty()
        {
            bail!("tag `{tag}` is a repository series tag and does not identify a package release");
        }
    }
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
