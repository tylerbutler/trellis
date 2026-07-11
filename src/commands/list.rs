//! `trellis list` — members in topological order. This alone replaces the
//! hand-maintained, hand-ordered package list in a justfile.

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
        let items: Vec<_> = selected
            .iter()
            .map(|&idx| super::member_json(workspace, idx))
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        for idx in selected {
            println!("{}", workspace.members[idx].name);
        }
    }
    Ok(())
}
