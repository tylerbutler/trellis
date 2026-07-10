//! `trellis info <package>` — details for a single member.

use crate::gleam::Requirement;
use crate::workspace::Workspace;
use anyhow::{Context, Result};

pub fn run(workspace: &Workspace, name: &str, json: bool) -> Result<()> {
    let idx = workspace
        .member_index(name)
        .with_context(|| format!("unknown package `{name}`"))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&super::member_json(workspace, idx))?
        );
        return Ok(());
    }

    let member = &workspace.members[idx];
    println!("name:       {}", member.name);
    println!("version:    {}", member.version());
    println!("path:       {}", member.rel_path);
    println!("releasable: {}", member.releasable);
    println!(
        "tag:        {}",
        workspace.config.format_tag(&member.name, member.version())
    );
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
    println!(
        "workspace deps:       {}",
        format_names(workspace.deps_of(idx))
    );
    println!(
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
    println!(
        "hex deps:             {}",
        if hex_deps.is_empty() {
            "(none)".to_string()
        } else {
            hex_deps.join(", ")
        }
    );
    Ok(())
}
