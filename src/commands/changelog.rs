//! `trellis changelog` — the changie interop layer. `sync` regenerates the
//! derived `projects:` section of `.changie.yaml`; `check` decides which
//! changed packages still need a fragment; `new` wraps `changie new`.

use crate::changie;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use serde_json::json;

// ---- sync --------------------------------------------------------------

/// How the on-disk `.changie.yaml` compares to what `sync` would write.
#[derive(Debug, PartialEq, Eq)]
pub enum SyncStatus {
    Clean,
    Drifted,
    Missing,
}

/// The full text `sync` would write, and how the current file compares to it.
pub fn sync_status(workspace: &Workspace) -> (SyncStatus, String) {
    let config_path = workspace.root.join(changie::CONFIG_FILE);
    match std::fs::read_to_string(&config_path) {
        Ok(existing) => {
            let expected = changie::splice_projects(&existing, &changie::projects_block(workspace));
            if existing == expected {
                (SyncStatus::Clean, expected)
            } else {
                (SyncStatus::Drifted, expected)
            }
        }
        Err(_) => (SyncStatus::Missing, changie::starter_config(workspace)),
    }
}

/// Write (or with `check_only`, verify) the generated `.changie.yaml`
/// projects. Returns false when `check_only` finds drift.
pub fn sync(workspace: &Workspace, check_only: bool) -> Result<bool> {
    let (status, expected) = sync_status(workspace);
    let config_path = workspace.root.join(changie::CONFIG_FILE);
    match (status, check_only) {
        (SyncStatus::Clean, _) => {
            println!("{} is up to date", changie::CONFIG_FILE);
            Ok(true)
        }
        (SyncStatus::Drifted, true) => {
            println!(
                "{} projects are out of date; run `trellis changelog sync`",
                changie::CONFIG_FILE
            );
            Ok(false)
        }
        (SyncStatus::Missing, true) => {
            println!(
                "{} does not exist; run `trellis changelog sync` to create it",
                changie::CONFIG_FILE
            );
            Ok(false)
        }
        (status, false) => {
            std::fs::write(&config_path, expected)
                .with_context(|| format!("failed to write {}", config_path.display()))?;
            match status {
                SyncStatus::Missing => println!("created {}", changie::CONFIG_FILE),
                _ => println!("updated {} projects", changie::CONFIG_FILE),
            }
            Ok(true)
        }
    }
}

// ---- new ---------------------------------------------------------------

/// Wrap `changie new`, routing to a project. Interactive: changie prompts for
/// kind and body.
pub fn new_fragment(workspace: &Workspace, package: Option<&str>) -> Result<()> {
    let mut args = vec!["new"];
    if let Some(name) = package {
        let idx = workspace
            .member_index(name)
            .with_context(|| format!("unknown package `{name}`"))?;
        if !workspace.members[idx].releasable {
            bail!("package `{name}` is excluded from release by ignore-release");
        }
        args.extend(["--project", name]);
    }
    changie::run_interactive(&workspace.root, &args)
}

// ---- check -------------------------------------------------------------

pub struct CheckOptions {
    pub base: String,
    pub head: String,
    pub json: bool,
}

struct PackageStatus {
    name: String,
    fragments: usize,
}

/// Map the base...head diff to releasable packages and decide which still
/// need a changelog fragment. Returns false (non-zero exit) when any does.
pub fn check(workspace: &Workspace, options: &CheckOptions) -> Result<bool> {
    let changed = crate::git::changed_members_between(workspace, &options.base, &options.head)?;
    let env = changie::locate(&workspace.root);
    let fragments = changie::unreleased_fragments(&env)?;

    let mut invalid: Vec<String> = fragments
        .missing_project
        .iter()
        .map(|file| format!("fragment `{file}` has no project key"))
        .collect();
    for project in fragments.by_project.keys() {
        match workspace.member_index(project) {
            Some(idx) if workspace.members[idx].releasable => {}
            Some(_) => invalid.push(format!(
                "fragment project `{project}` is excluded from release by ignore-release"
            )),
            None => invalid.push(format!(
                "fragment project `{project}` is not a workspace member"
            )),
        }
    }

    let statuses: Vec<PackageStatus> = workspace
        .members
        .iter()
        .enumerate()
        .filter(|(idx, member)| changed.contains(idx) && member.releasable)
        .map(|(_, member)| PackageStatus {
            name: member.name.clone(),
            fragments: fragments.by_project.get(&member.name).copied().unwrap_or(0),
        })
        .collect();

    let needs_entry: Vec<&str> = statuses
        .iter()
        .filter(|status| status.fragments == 0)
        .map(|status| status.name.as_str())
        .collect();
    let ok = needs_entry.is_empty() && invalid.is_empty();

    if options.json {
        let payload = json!({
            "has-entries": !fragments.by_project.is_empty(),
            "needs-entry": !needs_entry.is_empty(),
            "invalid-fragments": invalid,
            "packages": statuses.iter().map(|status| json!({
                "name": status.name,
                "changed": true,
                "has-entry": status.fragments > 0,
                "fragments": status.fragments,
            })).collect::<Vec<_>>(),
            "preview": preview(&statuses, &invalid),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        if statuses.is_empty() {
            println!(
                "no releasable packages changed between {} and {}",
                options.base, options.head
            );
        }
        for status in &statuses {
            let state = if status.fragments > 0 {
                format!("{} fragment(s)", status.fragments)
            } else {
                "needs a changelog entry".to_string()
            };
            println!("{}: {state}", status.name);
        }
        for problem in &invalid {
            println!("invalid: {problem}");
        }
    }
    Ok(ok)
}

/// Markdown summary for the PR sticky comment.
fn preview(statuses: &[PackageStatus], invalid: &[String]) -> String {
    let mut out = String::from("### Changelog check\n\n");
    if statuses.is_empty() {
        out.push_str("No releasable packages changed.\n");
    } else {
        out.push_str("| package | fragments |\n| --- | --- |\n");
        for status in statuses {
            if status.fragments > 0 {
                out.push_str(&format!("| {} | ✅ {} |\n", status.name, status.fragments));
            } else {
                out.push_str(&format!("| {} | ❌ needs an entry |\n", status.name));
            }
        }
        if statuses.iter().any(|status| status.fragments == 0) {
            out.push_str("\nAdd one with `trellis changelog new --package <name>`.\n");
        }
    }
    for problem in invalid {
        out.push_str(&format!("\n⚠️ {problem}\n"));
    }
    out
}
