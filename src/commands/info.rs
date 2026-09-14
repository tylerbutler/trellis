//! `trellis info <package>` — details for a single member.

use crate::config::TagLevel;
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
    // Labels are dimmed with their padding outside the paint, so alignment
    // survives `--color never` transcripts byte-for-byte.
    let label = |text: &str| crate::term::dim(text);
    crate::status!(
        "{}       {}",
        label("name:"),
        crate::term::package(&member.name)
    );
    crate::status!("{}    {}", label("version:"), member.version());
    crate::status!("{}       {}", label("path:"), member.rel_path);
    crate::status!("{}  {}", label("lifecycle:"), member.lifecycle.key());
    crate::status!("{} {}", label("releasable:"), member.releasable());
    // Only the tags this member's mode actually produces — a series-only
    // package has no per-version tag to report.
    if member.tags.contains(&TagLevel::Exact) {
        crate::status!(
            "{}        {}",
            label("tag:"),
            workspace.config.exact_tag(&member.name, member.version())
        );
    }
    for tag in workspace.series_tags_of(idx) {
        crate::status!("{} {tag}", label("series tag:"));
    }
    let names =
        |indices: &[usize]| or_none(indices.iter().map(|&i| workspace.members[i].name.as_str()));
    crate::status!(
        "{}       {}",
        label("workspace deps:"),
        names(workspace.deps_of(idx))
    );
    crate::status!(
        "{} {}",
        label("workspace dependents:"),
        names(workspace.dependents_of(idx))
    );
    crate::status!(
        "{}             {}",
        label("hex deps:"),
        or_none(
            member
                .manifest
                .dependencies
                .iter()
                .filter(|dep| matches!(dep.requirement, Requirement::Hex(_)))
                .map(|dep| dep.name.as_str())
        )
    );
    Ok(())
}

/// Comma-separated, or `(none)` when empty.
fn or_none<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let joined = names.collect::<Vec<_>>().join(", ");
    if joined.is_empty() {
        "(none)".to_string()
    } else {
        joined
    }
}
