//! `trellis ci` — structured output for GitHub Actions. `matrix` emits the
//! exact `strategy.matrix` shape workflows consume; `outputs` emits key=value
//! lines suitable for `$GITHUB_OUTPUT`; `tag-package` resolves a pushed tag
//! ($GITHUB_REF_NAME) to the package it belongs to.

use crate::workspace::{SelectionFilter, Workspace};
use anyhow::Result;
use serde_json::json;

/// Resolve a tag to its package for shell substitution, e.g.
/// `trellis lockfile refresh --package "$(trellis ci tag-package "$GITHUB_REF_NAME")"`.
pub fn tag_package(workspace: &Workspace, tag: &str, json_output: bool) -> Result<()> {
    let (idx, tag_version) = super::tag::resolve_tag(workspace, tag)?;
    let member = &workspace.members[idx];
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "name": member.name,
                "path": member.rel_path,
                "version": member.version(),
                "tag-version": tag_version,
            }))?
        );
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
    let include: Vec<_> = selected
        .iter()
        .map(|&idx| {
            let member = &workspace.members[idx];
            json!({
                "name": member.name,
                "path": member.rel_path,
                "version": member.version(),
            })
        })
        .collect();
    println!("{}", serde_json::to_string(&json!({ "include": include }))?);
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
        .filter(|m| m.releasable)
        .map(|m| workspace.config.format_tag(&m.name, m.version()))
        .collect();

    println!("projects={}", serde_json::to_string(&all)?);
    println!("releasable={}", serde_json::to_string(&releasable)?);
    println!("version-files={}", serde_json::to_string(&version_files)?);
    println!("tags={}", serde_json::to_string(&tags)?);
    Ok(())
}
