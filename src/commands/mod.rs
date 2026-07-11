pub mod changelog;
pub mod ci;
pub mod doctor;
pub mod exec;
pub mod graph;
pub mod info;
pub mod list;
pub mod lockfile;
pub mod new;
pub mod publish;
pub mod release;
pub mod run;
pub mod tag;
pub mod version;

use crate::workspace::Workspace;
use serde_json::json;

/// The JSON shape shared by `list --json` and `info --json`.
pub fn member_json(workspace: &Workspace, idx: usize) -> serde_json::Value {
    let member = &workspace.members[idx];
    json!({
        "name": member.name,
        "version": member.version(),
        "path": member.rel_path,
        "releasable": member.releasable,
        "dependencies": workspace
            .deps_of(idx)
            .iter()
            .map(|&dep| workspace.members[dep].name.clone())
            .collect::<Vec<_>>(),
        "dependents": workspace
            .dependents_of(idx)
            .iter()
            .map(|&dep| workspace.members[dep].name.clone())
            .collect::<Vec<_>>(),
    })
}
