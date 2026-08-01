//! `trellis info <package>` — details for a single member.

use crate::gleam::Requirement;
use crate::json::InfoDocument;
use crate::workspace::Workspace;
use anyhow::{Context, Result};

pub fn run(workspace: &Workspace, name: &str, json: bool) -> Result<()> {
    let idx = workspace
        .member_index(name)
        .with_context(|| format!("unknown package `{name}`"))?;
    if json {
        let document = InfoDocument::new(workspace, idx);
        println!("{}", serde_json::to_string_pretty(&document)?);
        return Ok(());
    }

    let member = &workspace.members[idx];
    crate::status!("name:       {}", member.name);
    crate::status!("version:    {}", member.version());
    crate::status!("path:       {}", member.rel_path);
    crate::status!("lifecycle:  {}", member.lifecycle.key());
    crate::status!("releasable: {}", member.releasable);
    // Only the tags this member's mode actually produces — a series-only
    // package has no per-version tag to report.
    if member.tag_mode.includes_exact() {
        crate::status!(
            "tag:        {}",
            workspace.config.format_tag(&member.name, member.version())
        );
    }
    if member.tag_mode.includes_series()
        && let Some(tag) = workspace
            .config
            .format_series_tag(&member.name, member.version())
    {
        crate::status!("series tag: {tag}");
    }
    let format_names = |indices: &[usize]| -> String {
        if indices.is_empty() {
            "(none)".to_string()
        } else {
            indices
                .iter()
                .map(|&i| workspace.members[i].name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    crate::status!(
        "workspace deps:       {}",
        format_names(workspace.deps_of(idx))
    );
    crate::status!(
        "workspace dependents: {}",
        format_names(workspace.dependents_of(idx))
    );
    let hex_deps: Vec<String> = member
        .manifest
        .dependencies
        .iter()
        .filter(|dep| matches!(dep.requirement, Requirement::Hex(_)))
        .map(|dep| dep.name.clone())
        .collect();
    crate::status!(
        "hex deps:             {}",
        if hex_deps.is_empty() {
            "(none)".to_string()
        } else {
            hex_deps.join(", ")
        }
    );
    Ok(())
}
