//! `trellis list` — workspace members in topological order (dependencies
//! first).

use crate::json::ListDocument;
use crate::workspace::{SelectionFilter, Workspace};
use anyhow::Result;

pub struct ListOptions {
    pub json: bool,
    pub since: Option<String>,
    pub with_dependents: bool,
    pub releasable: bool,
}

pub fn run(workspace: &Workspace, options: &ListOptions) -> Result<()> {
    let selected = workspace.select(&SelectionFilter {
        names: Vec::new(),
        since: options.since.clone(),
        with_dependents: options.with_dependents,
        releasable_only: options.releasable,
    })?;

    if options.json {
        let document = ListDocument::new(workspace, &selected);
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        // Human-readable output is not a stable contract (see
        // json-output.mdx), but the two columns are always present and in
        // this order: a reader (or a quick `awk '{print $1}'`) can rely on
        // `name` coming first whatever else changes.
        let width = selected
            .iter()
            .map(|&idx| workspace.members[idx].name.len())
            .max()
            .unwrap_or(0);
        for idx in selected {
            let member = &workspace.members[idx];
            crate::status!("{:width$}  {}", member.name, member.lifecycle.key());
        }
    }
    Ok(())
}
