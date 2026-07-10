//! `trellis ci` — structured output for GitHub Actions. `matrix` emits the
//! exact `strategy.matrix` shape workflows consume; `outputs` emits key=value
//! lines suitable for `$GITHUB_OUTPUT`.

use crate::workspace::{SelectionFilter, Workspace};
use anyhow::Result;
use serde_json::json;

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
