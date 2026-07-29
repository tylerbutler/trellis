//! `trellis version` — plan and apply version bumps on the native changelog
//! engine: compute each package's next version from its fragments' kinds,
//! render the version section, bump gleam.toml surgically, patch workspace
//! dependency versions in `manifest.toml`, then batch and reassemble
//! CHANGELOG.md — all with zero Hex network calls.

use crate::changelog;
use crate::commands::version_override::Overrides;
use crate::json::{Bump, UpdatedDependency, VersionApplyDocument, VersionPlanDocument};
use crate::lockfile;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug)]
pub struct PlanEntry {
    pub name: String,
    pub current: String,
    pub next: String,
    /// How many fragments the package owns on disk.
    pub fragments: usize,
    /// Workspace dependencies that bumped in this same plan, sorted by name.
    /// Empty when nothing rippled into this package.
    pub updated_deps: Vec<UpdatedDep>,
    /// The changelog entries `updated_deps` renders as. Held alongside rather
    /// than derived on demand so `apply` renders exactly what `plan` reported.
    pub generated: Vec<changelog::Fragment>,
    /// Whether `current` was itself a prerelease, so `--pre none` can tell a
    /// promotion from a package that merely joined the plan late.
    was_prerelease: bool,
}

#[derive(Debug)]
pub struct UpdatedDep {
    pub name: String,
    pub version: String,
}

/// One entry per releasable member that either owns unreleased fragments or
/// depends on something that bumped, in topological order. Any invalid
/// fragment is a hard error — silently dropping one is exactly the drift this
/// tool exists to prevent.
///
/// Dependents ripple because a path dep's Hex requirement is derived from the
/// dependency's version at publish time (`crate::rewrite`). Leaving a dependent
/// unbumped would let one published version resolve to two different dependency
/// sets, depending on whether it was fetched before or after the bump.
///
/// `workspace.members` is topologically ordered, so one forward sweep is enough:
/// by the time it reaches a member, every dependency's final version is settled.
/// Unreleasable members are never recorded, so a ripple stops at one rather than
/// skipping over it to its dependents.
pub fn compute_plan(workspace: &Workspace, overrides: &Overrides) -> Result<Vec<PlanEntry>> {
    let fragments = changelog::load_fragments(workspace)?;
    if !fragments.problems.is_empty() {
        bail!(
            "invalid changelog fragment(s):\n  - {}",
            fragments.problem_messages().join("\n  - ")
        );
    }
    // Before anything is computed, so a typo'd package name fails immediately
    // rather than being silently ignored.
    validate_named_packages(workspace, overrides)?;

    let config = &workspace.config.changelog;
    let mut plan = Vec::new();
    let mut bumped: BTreeMap<&str, String> = BTreeMap::new();
    for (idx, member) in workspace.members.iter().enumerate() {
        if !member.releasable {
            continue;
        }
        // Sorted by dependency name so the rendered order does not depend on
        // the graph's internal indexing.
        let mut rippled: Vec<(&str, &String)> = workspace
            .deps_of(idx)
            .iter()
            .filter_map(|&dep| {
                let dep_name = workspace.members[dep].name.as_str();
                bumped.get(dep_name).map(|version| (dep_name, version))
            })
            .collect();
        rippled.sort_unstable_by_key(|(name, _)| *name);
        let generated = rippled
            .iter()
            .map(|(dep_name, dep_version)| {
                changelog::dependency_fragment(config, &member.name, dep_name, dep_version)
            })
            .collect::<Result<Vec<_>>>()?;
        let updated_deps: Vec<UpdatedDep> = rippled
            .into_iter()
            .map(|(name, version)| UpdatedDep {
                name: name.to_string(),
                version: version.clone(),
            })
            .collect();

        let owned: Vec<&changelog::Fragment> = fragments.for_package(&member.name).collect();
        if owned.is_empty() && generated.is_empty() {
            continue;
        }
        let all: Vec<&changelog::Fragment> =
            owned.iter().copied().chain(generated.iter()).collect();
        let derived = changelog::derive_bump(&all, &config.kinds)
            .with_context(|| format!("cannot compute next version for `{}`", member.name))?;
        let current = semver::Version::parse(member.version())
            .with_context(|| format!("`{}` has an invalid version", member.name))?;
        let next = overrides.resolve(&member.name, &current, derived)?;
        bumped.insert(&member.name, next.to_string());
        plan.push(PlanEntry {
            name: member.name.clone(),
            current: member.version().to_string(),
            next: next.to_string(),
            fragments: owned.len(),
            updated_deps,
            generated,
            was_prerelease: !current.pre.is_empty(),
        });
    }
    if overrides.promoting() && !plan.iter().any(|entry| entry.was_prerelease) {
        bail!("--pre none promotes a prerelease, but no package in the plan is at one");
    }
    Ok(plan)
}

/// A `--bump pkg=` or `--set pkg=` naming something that is not a releasable
/// member is a typo, not a no-op.
fn validate_named_packages(workspace: &Workspace, overrides: &Overrides) -> Result<()> {
    for name in overrides.named_packages() {
        match workspace.member_index(name) {
            None => bail!("--bump/--set names unknown package `{name}`"),
            Some(idx) if !workspace.members[idx].releasable => {
                bail!("--bump/--set names `{name}`, which is excluded from releases");
            }
            Some(_) => {}
        }
    }
    Ok(())
}

pub fn plan(workspace: &Workspace, overrides: &Overrides, json: bool) -> Result<()> {
    let plan = compute_plan(workspace, overrides)?;
    if json {
        let document = VersionPlanDocument {
            schema: VersionPlanDocument::SCHEMA,
            bumped: bumps(&plan),
            fragments_retained: overrides.retains_fragments(),
        };
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else if plan.is_empty() {
        crate::status!("no unreleased changes; nothing to bump");
    } else {
        for entry in &plan {
            crate::status!(
                "{}: {} -> {} ({})",
                entry.name,
                entry.current,
                entry.next,
                entry.why()
            );
        }
        if overrides.retains_fragments() {
            crate::status!(
                "note: prerelease — fragments stay unreleased and will be released again \
                 by the final version"
            );
        }
    }
    Ok(())
}

impl PlanEntry {
    /// Why this package is being released, for the human-readable plan.
    fn why(&self) -> String {
        let mut parts = Vec::new();
        if self.fragments > 0 {
            parts.push(format!("{} fragment(s)", self.fragments));
        }
        if !self.updated_deps.is_empty() {
            let names: Vec<&str> = self
                .updated_deps
                .iter()
                .map(|dep| dep.name.as_str())
                .collect();
            parts.push(format!("dependencies: {}", names.join(", ")));
        }
        parts.join(", ")
    }
}

/// `version plan` and `version apply` report the same entry shape under the
/// same `bumped` key, so they share one conversion.
fn bumps(plan: &[PlanEntry]) -> Vec<Bump<'_>> {
    plan.iter()
        .map(|entry| Bump {
            name: &entry.name,
            current: &entry.current,
            next: &entry.next,
            fragments: entry.fragments,
            updated_dependencies: entry
                .updated_deps
                .iter()
                .map(|dep| UpdatedDependency {
                    name: &dep.name,
                    version: &dep.version,
                })
                .collect(),
        })
        .collect()
}

/// The release step: preflight every pending package and lockfile, write all
/// version bumps, then batch fragments and rebuild each CHANGELOG.md.
pub fn apply(workspace: &Workspace, overrides: &Overrides, json: bool) -> Result<bool> {
    let plan = compute_plan(workspace, overrides)?;
    if plan.is_empty() {
        if json {
            let document = VersionApplyDocument {
                schema: VersionApplyDocument::SCHEMA,
                bumped: Vec::new(),
                lockfiles: Vec::new(),
                adopted: Vec::new(),
                fragments_retained: false,
            };
            println!("{}", serde_json::to_string_pretty(&document)?);
        } else {
            crate::status!("no unreleased changes; nothing to apply");
        }
        return Ok(true);
    }

    let fragments = changelog::load_fragments(workspace)?;
    let date = changelog::today();
    let mut prepared_versions = Vec::new();
    for entry in &plan {
        let idx = workspace
            .member_index(&entry.name)
            .expect("plan entries come from members");
        let member = &workspace.members[idx];
        let member_fragments: Vec<&changelog::Fragment> =
            fragments.for_package(&entry.name).collect();
        // Generated ripple entries render alongside the real ones, but are
        // deliberately absent from what `consume_fragments` is later given.
        let rendered: Vec<&changelog::Fragment> = member_fragments
            .iter()
            .copied()
            .chain(entry.generated.iter())
            .collect();
        let next = semver::Version::parse(&entry.next).expect("plan versions are valid");
        let tag = workspace.config.format_tag(&entry.name, &entry.next);
        let section = changelog::render_section(
            &workspace.config.changelog,
            &entry.name,
            &entry.next,
            &tag,
            &date,
            &rendered,
        )?;
        // A package releasing for the first time may already have a
        // hand-written CHANGELOG.md; adopt it so regenerating preserves it.
        let adoption = changelog::plan_adoption(workspace, &entry.name, &entry.current)
            .with_context(|| format!("failed to read `{}`'s changelog history", entry.name))?;
        let changelog = changelog::render_merged_changelog(
            workspace,
            &entry.name,
            Some((&next, &section)),
            adoption.as_ref(),
        )
        .with_context(|| format!("failed to merge `{}`", entry.name))?;
        let manifest_path = member.path.join("gleam.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest = changelog::render_manifest_version(&manifest, &next)
            .with_context(|| format!("failed to bump `{}`", entry.name))?;
        prepared_versions.push(PreparedVersion {
            name: entry.name.clone(),
            next,
            section,
            changelog,
            manifest_path,
            manifest,
            adoption,
        });
    }

    let mut versions: BTreeMap<String, String> = workspace
        .members
        .iter()
        .map(|member| (member.name.clone(), member.version().to_string()))
        .collect();
    for entry in &plan {
        versions.insert(entry.name.clone(), entry.next.clone());
    }

    let mut prepared_lockfiles = Vec::new();
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
            prepared_lockfiles.push(PreparedLockfile {
                display: format!("{}/manifest.toml", member.rel_path),
                path,
                text: new_text,
            });
        }
    }

    for prepared in &prepared_versions {
        std::fs::write(&prepared.manifest_path, &prepared.manifest)
            .with_context(|| format!("failed to write {}", prepared.manifest_path.display()))?;
    }
    for prepared in &prepared_lockfiles {
        std::fs::write(&prepared.path, &prepared.text)
            .with_context(|| format!("failed to write {}", prepared.path.display()))?;
    }

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

    let mut adopted_files = Vec::new();
    for prepared in &prepared_versions {
        if let Some(adoption) = &prepared.adoption {
            if let Some(parent) = adoption.path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(&adoption.path, &adoption.contents)
                .with_context(|| format!("failed to write {}", adoption.path.display()))?;
            adopted_files.push(display_path(&workspace, &adoption.path));
        }
        changelog::write_batch(
            &workspace,
            &prepared.name,
            &prepared.next,
            &prepared.section,
            &prepared.changelog,
        )
        .with_context(|| format!("failed to write changelog for `{}`", prepared.name))?;
    }
    // A prerelease renders its section but leaves the fragments in place: they
    // are still unreleased as far as the final version is concerned, and
    // retiring them at rc.1 would leave the eventual 1.0.0 with nothing to say.
    if !overrides.retains_fragments() {
        for prepared in &prepared_versions {
            let member_fragments: Vec<&changelog::Fragment> =
                fragments.for_package(&prepared.name).collect();
            changelog::consume_fragments(&member_fragments)
                .with_context(|| format!("failed to consume fragments for `{}`", prepared.name))?;
        }
    }

    let patched_files: Vec<&str> = prepared_lockfiles
        .iter()
        .map(|prepared| prepared.display.as_str())
        .collect();

    if json {
        let document = VersionApplyDocument {
            schema: VersionApplyDocument::SCHEMA,
            bumped: bumps(&plan),
            lockfiles: patched_files,
            adopted: adopted_files.iter().map(String::as_str).collect(),
            fragments_retained: overrides.retains_fragments(),
        };
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        for entry in &plan {
            crate::status!("bumped {}: {} -> {}", entry.name, entry.current, entry.next);
        }
        for file in &patched_files {
            crate::status!("patched {file}");
        }
        for file in &adopted_files {
            crate::status!("adopted existing changelog history as {file}");
        }
        if overrides.retains_fragments() {
            crate::status!("kept fragments unreleased for the final version");
        }
    }
    Ok(true)
}

/// A path relative to the workspace root, for output that stays stable
/// wherever the repository is checked out.
fn display_path(workspace: &Workspace, path: &std::path::Path) -> String {
    path.strip_prefix(&workspace.root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

struct PreparedVersion {
    name: String,
    next: semver::Version,
    section: String,
    changelog: String,
    manifest_path: PathBuf,
    manifest: String,
    /// Pre-trellis changelog history to preserve, on a first release.
    adoption: Option<changelog::Adoption>,
}

struct PreparedLockfile {
    display: String,
    path: PathBuf,
    text: String,
}
