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
        for idx in selected {
            crate::status!("{}", workspace.members[idx].name);
        }
    }
    Ok(())
}
