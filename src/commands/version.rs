//! `trellis version` — plan and apply version bumps on the native changelog
//! engine: compute each package's next version from its fragments' kinds,
//! render and batch the version section, reassemble CHANGELOG.md, bump
//! gleam.toml surgically, then patch `manifest.toml` locked versions of
//! workspace-internal deps — all with zero Hex network calls.

use crate::changelog;
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
/// order. Any invalid fragment is a hard error — silently dropping one is
/// exactly the drift this tool exists to prevent.
pub fn compute_plan(workspace: &Workspace) -> Result<Vec<PlanEntry>> {
    let fragments = changelog::load_fragments(workspace)?;
    if !fragments.problems.is_empty() {
        bail!(
            "invalid changelog fragment(s):\n  - {}",
            fragments.problems.join("\n  - ")
        );
    }

    let kinds = &workspace.config.changelog.kinds;
    let mut plan = Vec::new();
    for member in workspace.members.iter().filter(|m| m.releasable) {
        let member_fragments: Vec<&changelog::Fragment> =
            fragments.for_project(&member.name).collect();
        if member_fragments.is_empty() {
            continue;
        }
        let next = changelog::next_version(member.version(), &member_fragments, kinds)
            .with_context(|| format!("cannot compute next version for `{}`", member.name))?;
        plan.push(PlanEntry {
            name: member.name.clone(),
            current: member.version().to_string(),
            next: next.to_string(),
            fragments: member_fragments.len(),
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

/// The release step: per pending package (topo order), render + batch the
/// version section, rebuild CHANGELOG.md, bump gleam.toml; then patch every
/// member's lockfile.
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

    let fragments = changelog::load_fragments(workspace)?;
    let date = changelog::today();
    for entry in &plan {
        let idx = workspace
            .member_index(&entry.name)
            .expect("plan entries come from members");
        let member = &workspace.members[idx];
        let member_fragments: Vec<&changelog::Fragment> =
            fragments.for_project(&entry.name).collect();
        let next = semver::Version::parse(&entry.next).expect("plan versions are valid");
        let tag = workspace.config.format_tag(&entry.name, &entry.next);
        let section = changelog::render_section(
            &workspace.config.changelog,
            &entry.name,
            &entry.next,
            &tag,
            &date,
            &member_fragments,
        )?;
        changelog::batch(workspace, &entry.name, &next, &section, &member_fragments)
            .with_context(|| format!("failed to batch `{}`", entry.name))?;
        changelog::bump_manifest_version(&member.path.join("gleam.toml"), &next)
            .with_context(|| format!("failed to bump `{}`", entry.name))?;
    }

    // Reload and verify every bump landed before touching lockfiles.
    let workspace = Workspace::load(&workspace.root)
        .context("workspace failed to reload after version bump")?;
    for entry in &plan {
        let idx = workspace
            .member_index(&entry.name)
            .with_context(|| format!("package `{}` disappeared during apply", entry.name))?;
        let actual = workspace.members[idx].version();
        if actual != entry.next {
            bail!(
                "version bump did not land for `{}`: gleam.toml says {actual}, expected {}",
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
