//! `trellis version` — plan and apply version bumps. `apply` drives changie
//! (`batch` + `merge`), verifies the replacements actually landed, then
//! patches `manifest.toml` locked versions of workspace-internal deps with
//! zero Hex network calls.

use crate::changie;
use crate::lockfile;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct PlanEntry {
    pub name: String,
    pub current: String,
    pub next: String,
    pub fragments: usize,
}

/// One entry per releasable member with unreleased fragments, in topological
/// order; `next` comes from `changie next auto`. Fragments pointing at
/// unknown or unreleasable projects are hard errors — silently dropping a
/// fragment is exactly the drift this tool exists to prevent.
pub fn compute_plan(workspace: &Workspace) -> Result<Vec<PlanEntry>> {
    let env = changie::locate(&workspace.root);
    let fragments = changie::unreleased_fragments(&env)?;

    if !fragments.missing_project.is_empty() {
        bail!(
            "unreleased fragment(s) without a project key: {}",
            fragments.missing_project.join(", ")
        );
    }
    for project in fragments.by_project.keys() {
        match workspace.member_index(project) {
            Some(idx) if workspace.members[idx].releasable => {}
            Some(_) => {
                bail!("fragment project `{project}` is excluded from release by ignore-release")
            }
            None => bail!("fragment project `{project}` is not a workspace member"),
        }
    }

    let mut plan = Vec::new();
    for member in workspace.members.iter().filter(|m| m.releasable) {
        let Some(&count) = fragments.by_project.get(&member.name) else {
            continue;
        };
        let stdout = changie::run(
            &workspace.root,
            &["next", "auto", "--project", &member.name],
        )?;
        let next = changie::parse_next_version(&stdout)?;
        plan.push(PlanEntry {
            name: member.name.clone(),
            current: member.version().to_string(),
            next: next.to_string(),
            fragments: count,
        });
    }
    Ok(plan)
}

pub fn plan(workspace: &Workspace, json: bool) -> Result<()> {
    let plan = compute_plan(workspace)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&plan_json(&plan))?);
    } else if plan.is_empty() {
        println!("no unreleased changes; nothing to bump");
    } else {
        for entry in &plan {
            println!(
                "{}: {} -> {} ({} fragment(s))",
                entry.name, entry.current, entry.next, entry.fragments
            );
        }
    }
    Ok(())
}

fn plan_json(plan: &[PlanEntry]) -> serde_json::Value {
    json!(
        plan.iter()
            .map(|entry| {
                json!({
                    "name": entry.name,
                    "current": entry.current,
                    "next": entry.next,
                    "fragments": entry.fragments,
                })
            })
            .collect::<Vec<_>>()
    )
}

/// The release step: `changie batch` per project, one `changie merge`, verify
/// every gleam.toml picked up its new version, then patch lockfiles.
pub fn apply(workspace: &Workspace, json: bool) -> Result<bool> {
    let plan = compute_plan(workspace)?;
    if plan.is_empty() {
        if json {
            println!("{}", json!({"bumped": [], "lockfiles": []}));
        } else {
            println!("no unreleased changes; nothing to apply");
        }
        return Ok(true);
    }

    for entry in &plan {
        changie::run(
            &workspace.root,
            &["batch", "auto", "--project", &entry.name],
        )
        .with_context(|| format!("changie batch failed for `{}`", entry.name))?;
    }
    changie::run(&workspace.root, &["merge"]).context("changie merge failed")?;

    // Reload: the batches rewrote gleam.toml versions on disk. Verify each
    // landed — a missing replacements block would otherwise fail silently.
    let workspace = Workspace::load(&workspace.root)
        .context("workspace failed to reload after version bump")?;
    for entry in &plan {
        let idx = workspace
            .member_index(&entry.name)
            .with_context(|| format!("package `{}` disappeared during apply", entry.name))?;
        let actual = workspace.members[idx].version();
        if actual != entry.next {
            bail!(
                "changie batch did not update `{}`: gleam.toml says {actual}, expected {} — \
                 check the replacements block in .changie.yaml (`trellis changelog sync`)",
                entry.name,
                entry.next
            );
        }
    }

    // Patch every member's manifest.toml so locked workspace-internal deps
    // match the new versions — the release.yml sed logic, as tested code.
    let versions: BTreeMap<String, String> = workspace
        .members
        .iter()
        .map(|member| (member.name.clone(), member.version().to_string()))
        .collect();
    let mut patched_files = Vec::new();
    for member in &workspace.members {
        let path = member.path.join("manifest.toml");
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let (new_text, patched) = lockfile::patch_locked_versions(&text, &versions)
            .with_context(|| format!("failed to patch {}", path.display()))?;
        if !patched.is_empty() {
            std::fs::write(&path, new_text)
                .with_context(|| format!("failed to write {}", path.display()))?;
            patched_files.push(format!("{}/manifest.toml", member.rel_path));
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "bumped": plan_json(&plan),
                "lockfiles": patched_files,
            }))?
        );
    } else {
        for entry in &plan {
            println!("bumped {}: {} -> {}", entry.name, entry.current, entry.next);
        }
        for file in &patched_files {
            println!("patched {file}");
        }
    }
    Ok(true)
}
