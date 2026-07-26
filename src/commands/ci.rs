//! `trellis ci` — structured output for GitHub Actions. `matrix` emits the
//! exact `strategy.matrix` shape workflows consume; `outputs` emits key=value
//! lines suitable for `$GITHUB_OUTPUT`; `tag-package` resolves a pushed tag
//! ($GITHUB_REF_NAME) to the package it belongs to.

use super::tag::{ResolvedTag, TagKind};
use crate::json::{CiMatrix, CiTagPackageDocument};
use crate::workspace::{SelectionFilter, Workspace};
use anyhow::Result;

/// Resolve a tag to its package for shell substitution, e.g.
/// `trellis lockfile refresh --package "$(trellis ci tag-package "$GITHUB_REF_NAME")"`.
pub fn tag_package(workspace: &Workspace, tag: &str, json_output: bool) -> Result<()> {
    let resolved = super::tag::resolve_tag(workspace, tag)?;
    let member = &workspace.members[resolved.member()];
    if json_output {
        // A series tag identifies the package but no version, so it reports
        // the series it names instead of `tag-version`.
        let (tag_kind, tag_version, tag_series) = match &resolved {
            ResolvedTag::Exact { version, .. } => (TagKind::Exact, Some(version.as_str()), None),
            ResolvedTag::Series { series, .. } => (TagKind::Series, None, Some(series.as_str())),
        };
        let document = CiTagPackageDocument {
            schema: CiTagPackageDocument::SCHEMA,
            name: &member.name,
            path: &member.rel_path,
            version: member.version(),
            tag_kind,
            tag_version,
            tag_series,
        };
        println!("{}", serde_json::to_string(&document)?);
    } else {
        println!("{}", member.name);
    }
    Ok(())
}

pub fn matrix(workspace: &Workspace, since: Option<String>, releasable: bool) -> Result<()> {
    let selected = workspace.select(&SelectionFilter {
        names: Vec::new(),
        since,
        with_dependents: true, // a change to a dep can break its dependents
        releasable_only: releasable,
    })?;
    let matrix = CiMatrix::new(workspace, &selected);
    println!("{}", serde_json::to_string(&matrix)?);
    Ok(())
}

pub fn outputs(workspace: &Workspace) -> Result<()> {
    let all: Vec<&str> = workspace.members.iter().map(|m| m.name.as_str()).collect();
    let releasable: Vec<&str> = workspace
        .members
        .iter()
        .filter(|m| m.releasable)
        .map(|m| m.name.as_str())
        .collect();
    let version_files: Vec<String> = workspace
        .members
        .iter()
        .filter(|m| m.releasable)
        .map(|m| format!("{}/gleam.toml", m.rel_path))
        .collect();
    let tags: Vec<String> = workspace
        .members
        .iter()
        .filter(|m| m.releasable && m.tag_mode.includes_exact())
        .map(|m| workspace.config.format_tag(&m.name, m.version()))
        .collect();
    // Deduplicated: a repository-wide series tag is one tag, however many
    // members move it.
    let mut series_tags: Vec<String> = Vec::new();
    for member in workspace
        .members
        .iter()
        .filter(|m| m.releasable && m.tag_mode.includes_series())
    {
        if let Some(tag) = workspace
            .config
            .format_series_tag(&member.name, member.version())
            && !series_tags.contains(&tag)
        {
            series_tags.push(tag);
        }
    }

    println!("projects={}", serde_json::to_string(&all)?);
    println!("releasable={}", serde_json::to_string(&releasable)?);
    println!("version-files={}", serde_json::to_string(&version_files)?);
    println!("tags={}", serde_json::to_string(&tags)?);
    println!("series-tags={}", serde_json::to_string(&series_tags)?);
    Ok(())
}
