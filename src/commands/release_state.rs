use super::version::PlanEntry;
use super::version_override::{Intent, Overrides};
use crate::git::{git_output, git_stdout, slash_path};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const SCHEMA: &str = "trellis.release_intent/1";

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    schema: String,
    pub base: String,
    pub branch: String,
    pub base_commit: String,
    pub intent: Intent,
    pub packages: BTreeMap<String, Package>,
    pub inputs: BTreeMap<String, String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Package {
    pub path: String,
    pub current: String,
    pub next: String,
}

pub(super) fn path(workspace: &Workspace) -> String {
    slash_path(&Path::new(&workspace.config.changelog.dir).join("release.toml"))
}

pub(super) fn validate_location(workspace: &Workspace) -> Result<()> {
    let dir = Path::new(&workspace.config.changelog.dir);
    ensure!(
        !dir.is_absolute()
            && !dir
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "release state requires a repository-relative changelog directory"
    );
    let mut location = workspace.root.clone();
    for component in Path::new(&path(workspace)).components() {
        location.push(component);
        match std::fs::symlink_metadata(&location) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "release state path contains a symlink: {}",
                location.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("cannot inspect release state location"),
        }
    }
    let ignored = git_output(
        &workspace.root,
        &["check-ignore", "--quiet", "--", &path(workspace)],
    )?;
    match ignored.status.code() {
        Some(0) => bail!(
            "release intent file `{}` is ignored by git; remove its ignore rule before preparation",
            path(workspace)
        ),
        Some(1) => {}
        _ => bail!(
            "cannot check release intent ignore rules: {}",
            String::from_utf8_lossy(&ignored.stderr)
        ),
    }
    Ok(())
}

fn validate_oid(oid: &str) -> Result<()> {
    ensure!(
        matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid release-state commit `{oid}`"
    );
    Ok(())
}

pub(super) fn ancestor(root: &Path, ancestor: &str, head: &str) -> Result<bool> {
    let output = git_output(root, &["merge-base", "--is-ancestor", ancestor, head])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "cannot verify release baseline ancestry: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn file_at(root: &Path, revision: &str, path: &str) -> Result<Option<String>> {
    let listed = git_stdout(root, &["ls-tree", "-z", revision, "--", path])?;
    if listed.is_empty() {
        return Ok(None);
    }
    Ok(Some(git_stdout(
        root,
        &["show", &format!("{revision}:./{path}")],
    )?))
}

pub(super) fn read(workspace: &Workspace, revision: &str) -> Result<Option<State>> {
    let Some(text) = file_at(&workspace.root, revision, &path(workspace))? else {
        return Ok(None);
    };
    let state: State = toml::from_str(&text).context(
        "invalid release intent; use --no-resume with an explicit request to replace it",
    )?;
    ensure!(
        state.schema == SCHEMA,
        "unsupported release intent schema `{}`",
        state.schema
    );
    validate_oid(&state.base_commit)?;
    Overrides::parse(
        &state.intent.bump,
        &state.intent.set,
        state.intent.pre.as_deref(),
    )
    .context("invalid stored semantic release options")?;
    for package in state.packages.values() {
        semver::Version::parse(&package.current)
            .context("invalid source version in release state")?;
        semver::Version::parse(&package.next)
            .context("invalid prepared version in release state")?;
    }
    ensure!(!state.packages.is_empty(), "release state has no packages");
    Ok(Some(state))
}

impl State {
    pub fn new(
        workspace: &Workspace,
        base: &str,
        branch: &str,
        base_commit: &str,
        overrides: &Overrides,
        plan: &[PlanEntry],
    ) -> Result<Self> {
        validate_location(workspace)?;
        Ok(Self {
            schema: SCHEMA.to_string(),
            base: base.to_string(),
            branch: branch.to_string(),
            base_commit: base_commit.to_string(),
            intent: overrides.intent(),
            packages: plan
                .iter()
                .map(|entry| {
                    (
                        entry.name.clone(),
                        Package {
                            path: workspace.members[entry.member].rel_file("gleam.toml"),
                            current: entry.current.to_string(),
                            next: entry.next.to_string(),
                        },
                    )
                })
                .collect(),
            inputs: inputs(workspace, base_commit)?,
        })
    }

    pub fn resume(
        &self,
        workspace: &Workspace,
        base: &str,
        branch: &str,
        base_commit: &str,
        head: &str,
        explicit: &Overrides,
    ) -> Result<Overrides> {
        ensure!(
            self.base == base && self.branch == branch,
            "pending release intent targets a different base or branch; use --no-resume with an explicit request"
        );
        ensure!(
            ancestor(&workspace.root, &self.base_commit, base_commit)?,
            "stale release intent: original base is not an ancestor of the selected base"
        );
        ensure!(
            ancestor(&workspace.root, &self.base_commit, head)?,
            "stale release intent: pending head does not descend from its recorded base"
        );
        ensure!(
            !ancestor(&workspace.root, head, base_commit)?,
            "stale release intent: pending head is already merged; close its PR before continuing"
        );
        self.validate_source(workspace, base_commit)?;
        for (name, package) in &self.packages {
            let idx = workspace
                .member_index(name)
                .with_context(|| format!("stale release intent: package `{name}` disappeared"))?;
            let member = &workspace.members[idx];
            ensure!(
                member.releasable() && member.rel_file("gleam.toml") == package.path,
                "stale release intent: package `{name}` moved or is no longer releasable"
            );
            ensure!(
                member.version() == package.current,
                "stale release intent: `{name}` is now {}, expected {}; use --no-resume with an explicit request",
                member.version(),
                package.current
            );
            let manifest = file_at(&workspace.root, head, &package.path)?
                .with_context(|| format!("pending release head has no `{}`", package.path))?;
            let document: toml::Value = toml::from_str(&manifest)?;
            ensure!(
                document.get("version").and_then(toml::Value::as_str)
                    == Some(package.next.as_str()),
                "stale release intent: pending version for `{name}` was edited"
            );
        }
        let stored = Overrides::parse(
            &self.intent.bump,
            &self.intent.set,
            self.intent.pre.as_deref(),
        )?;
        let candidates: Vec<&str> = workspace
            .members
            .iter()
            .filter(|m| m.releasable())
            .filter(|m| semver::Version::parse(m.version()).is_ok_and(|v| !v.pre.is_empty()))
            .map(|m| m.name.as_str())
            .collect();
        Ok(stored.merged(explicit, &candidates))
    }

    fn validate_source(&self, workspace: &Workspace, restore: &str) -> Result<()> {
        let original = Overrides::parse(
            &self.intent.bump,
            &self.intent.set,
            self.intent.pre.as_deref(),
        )?;
        let (source, original_plan, original_inputs) =
            with_workspace_at(&workspace.root, &self.base_commit, restore, |source| {
                let plan = super::version::compute_plan(&source, &original)?;
                let inputs = inputs(&source, &self.base_commit)?;
                Ok((source, plan, inputs))
            })
            .context("release intent base is unavailable or invalid; fetch full history")?;
        ensure!(
            original_plan.len() == self.packages.len()
                && original_plan
                    .iter()
                    .all(|entry| self
                        .packages
                        .get(&entry.name)
                        .is_some_and(|p| p.current == entry.current.to_string()
                            && p.next == entry.next.to_string()
                            && p.path == source.members[entry.member].rel_file("gleam.toml"))),
            "stale release intent: semantic options do not match the recorded plan"
        );
        ensure!(
            original_inputs == self.inputs,
            "stale release intent: recorded inputs do not match its original base"
        );
        Ok(())
    }

    pub fn write(&self, workspace: &Workspace) -> Result<()> {
        validate_location(workspace)?;
        let file = workspace.root.join(path(workspace));
        let repo = crate::git::repo_root(&workspace.root)
            .context("release state requires a git repository")?;
        ensure!(
            file.starts_with(&repo)
                && !Path::new(&workspace.config.changelog.dir)
                    .components()
                    .any(|c| matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::RootDir
                    )),
            "release state must be stored in a repository-relative changelog directory"
        );
        std::fs::create_dir_all(file.parent().context("release state has no directory")?)?;
        std::fs::write(file, toml::to_string_pretty(self)?)
            .context("failed to persist release intent")
    }
}

pub(super) fn candidate_label(workspace: &Workspace) -> Result<Option<String>> {
    let mut labels: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for member in workspace.members.iter().filter(|m| m.releasable()) {
        let version = semver::Version::parse(member.version())?;
        if version.pre.is_empty() {
            continue;
        }
        let (label, counter) = version.pre.as_str().rsplit_once('.').with_context(|| {
            format!(
                "`{}` has unsupported candidate {version}; pass --pre explicitly",
                member.name
            )
        })?;
        ensure!(
            !label.contains('.') && counter.parse::<u64>().is_ok(),
            "`{}` has unsupported candidate {version}; pass --pre explicitly",
            member.name
        );
        labels
            .entry(label.to_string())
            .or_default()
            .push(format!("{}={version}", member.name));
    }
    ensure!(
        labels.len() <= 1,
        "conflicting candidate labels: {}; pass --pre <label> explicitly to choose the cycle",
        labels
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(labels.into_keys().next())
}

/// Post-application trees exclude archive/version generation from new content.
pub(super) fn baselines(
    workspace: &Workspace,
    base_commit: &str,
) -> Result<BTreeMap<String, String>> {
    if let Some(state) = read(workspace, base_commit)? {
        state.validate_source(workspace, base_commit)?;
        for (name, package) in &state.packages {
            let idx = workspace
                .member_index(name)
                .with_context(|| format!("stale candidate baseline: `{name}` disappeared"))?;
            ensure!(
                workspace.members[idx].version() == package.next,
                "stale candidate baseline: `{name}` no longer matches prepared version {}; pass --pre explicitly",
                package.next
            );
        }
        let range = format!("{}..{base_commit}", state.base_commit);
        let log = git_stdout(
            &workspace.root,
            &[
                "log",
                "--first-parent",
                "--format=%H",
                &range,
                "--",
                &path(workspace),
            ],
        )?;
        let mut commit = None;
        for revision in log.lines() {
            if read(workspace, revision)?.as_ref() != Some(&state) {
                break;
            }
            commit = Some(revision);
        }
        let commit = commit.context("release state has no committed baseline")?;
        ensure!(
            ancestor(&workspace.root, &state.base_commit, commit)?,
            "candidate baseline has inconsistent recorded ancestry"
        );
        return Ok(workspace
            .members
            .iter()
            .filter(|m| m.releasable())
            .filter(|m| semver::Version::parse(m.version()).is_ok_and(|v| !v.pre.is_empty()))
            .map(|m| (m.name.clone(), commit.to_string()))
            .collect());
    }
    let mut baselines = BTreeMap::new();
    for member in workspace.members.iter().filter(|m| m.releasable()) {
        let current = semver::Version::parse(member.version())?;
        if current.pre.is_empty() {
            continue;
        }
        let manifest_path = member.rel_file("gleam.toml");
        let log = git_stdout(
            &workspace.root,
            &[
                "log",
                "--first-parent",
                "--format=%H",
                base_commit,
                "--",
                &manifest_path,
            ],
        )?;
        let mut baseline = None;
        for commit in log.lines() {
            let Some(manifest) = file_at(&workspace.root, commit, &manifest_path)? else {
                break;
            };
            let manifest: toml::Value = toml::from_str(&manifest)?;
            if manifest.get("version").and_then(toml::Value::as_str) != Some(member.version()) {
                break;
            }
            baseline = Some(commit.to_string());
        }
        let commit = baseline.with_context(|| format!("no reliable candidate baseline for `{}`; fetch full history or pass --pre explicitly", member.name))?;
        let section = slash_path(
            &Path::new(&workspace.config.changelog.dir)
                .join(&member.name)
                .join(format!("v{current}.md")),
        );
        ensure!(
            file_at(&workspace.root, &commit, &section)?.is_some(),
            "candidate baseline for `{}` has no release section; fetch full history or pass --pre explicitly",
            member.name
        );
        let parents = git_stdout(
            &workspace.root,
            &["rev-list", "--parents", "-n", "1", &commit],
        )?;
        ensure!(
            parents.split_whitespace().count() > 1,
            "candidate history is incomplete for `{}`; fetch full history or pass --pre explicitly",
            member.name
        );
        baselines.insert(member.name.clone(), commit);
    }
    Ok(baselines)
}

pub(super) fn candidate_changed(
    workspace: &Workspace,
    base_commit: &str,
    plan: &[PlanEntry],
) -> Result<bool> {
    let baselines = baselines(workspace, base_commit)?;
    ensure!(
        !baselines.is_empty(),
        "no candidate baseline; fetch full history or pass --pre explicitly"
    );
    let mut newest: Option<&str> = None;
    for commit in baselines.values() {
        if let Some(previous) = newest {
            if ancestor(&workspace.root, previous, commit)? {
                newest = Some(commit);
            } else {
                ensure!(
                    ancestor(&workspace.root, commit, previous)?,
                    "candidate baselines are on conflicting histories; fetch full history or pass --pre explicitly"
                );
            }
        } else {
            newest = Some(commit);
        }
    }
    let newest = newest.context("no candidate baseline")?;
    let mut snapshots = BTreeMap::new();
    for commit in baselines.values() {
        if !snapshots.contains_key(commit) {
            let previous = workspace_at(&workspace.root, commit, base_commit)?;
            snapshots.insert(commit.clone(), owned_inputs(&previous, commit)?);
        }
    }
    let (current, config) = owned_inputs(workspace, base_commit)?;
    let mut changed = false;
    for member in workspace.members.iter().filter(|m| m.releasable()) {
        let commit = baselines
            .get(&member.name)
            .map(String::as_str)
            .unwrap_or(newest);
        let before = snapshots[commit].0.get(&member.name);
        let after = current.get(&member.name);
        if before != after {
            ensure!(
                plan.iter().any(|p| p.name == member.name),
                "release content changed for `{}` but it has no release plan; add a changelog fragment",
                member.name
            );
            changed = true;
        }
        if plan
            .iter()
            .any(|p| p.name == member.name && p.current.pre.is_empty())
        {
            changed = true;
        }
    }
    changed |= snapshots[newest].1 != config;
    Ok(changed)
}

type OwnedInputs = BTreeMap<String, BTreeMap<String, String>>;

fn owned_inputs(workspace: &Workspace, revision: &str) -> Result<(OwnedInputs, Option<String>)> {
    let bundles = crate::bundle::resolve(workspace)?;
    let owners: BTreeMap<usize, BTreeSet<usize>> = workspace
        .members
        .iter()
        .enumerate()
        .filter(|(_, m)| m.releasable())
        .map(|(idx, _)| {
            (
                idx,
                std::iter::once(idx)
                    .chain(bundles.get(&idx).into_iter().flatten().copied())
                    .collect(),
            )
        })
        .collect();
    let unreleased = slash_path(&Path::new(&workspace.config.changelog.dir).join("unreleased"));
    let mut result: OwnedInputs = owners
        .keys()
        .map(|&idx| (workspace.members[idx].name.clone(), BTreeMap::new()))
        .collect();
    let mut all = inputs(workspace, revision)?;
    let config = all.remove("@release_config");
    for (file, identity) in all {
        if file.starts_with(&format!("{unreleased}/")) {
            let text = file_at(&workspace.root, revision, &file)?
                .context("fragment disappeared from git tree")?;
            let fragment: toml::Value =
                toml::from_str(&text).context("invalid baseline fragment")?;
            if let Some(owner) = fragment
                .get("package")
                .and_then(toml::Value::as_str)
                .and_then(|name| result.get_mut(name))
            {
                owner.insert(file, identity);
            }
        } else {
            for (&owner, sources) in &owners {
                if sources.iter().any(|&idx| {
                    let member = &workspace.members[idx];
                    if member.rel_path == "." {
                        file == "gleam.toml"
                            || file.starts_with("src/")
                            || file.starts_with("priv/")
                    } else {
                        file.starts_with(&format!("{}/", member.rel_path))
                    }
                }) {
                    result
                        .get_mut(&workspace.members[owner].name)
                        .expect("owner was initialized")
                        .insert(file.clone(), identity.clone());
                }
            }
        }
    }
    Ok((result, config))
}

pub(super) fn workspace_at(root: &Path, revision: &str, restore: &str) -> Result<Workspace> {
    with_workspace_at(root, revision, restore, Ok)
}

fn with_workspace_at<T>(
    root: &Path,
    revision: &str,
    restore: &str,
    operation: impl FnOnce(Workspace) -> Result<T>,
) -> Result<T> {
    git_stdout(root, &["checkout", "--detach", revision])?;
    let loaded = Workspace::load(root).and_then(operation);
    let restored = git_stdout(root, &["checkout", "--detach", restore]);
    match (loaded, restored) {
        (Ok(workspace), Ok(_)) => Ok(workspace),
        (Err(error), Ok(_)) => Err(error),
        (_, Err(error)) => {
            Err(error.context("failed to restore base after reading release baseline"))
        }
    }
}

/// Keep byte identity for pending fragments and release-owned source/runtime inputs.
pub(super) fn inputs(workspace: &Workspace, revision: &str) -> Result<BTreeMap<String, String>> {
    let bundles = crate::bundle::resolve(workspace)?;
    let mut members: BTreeSet<usize> = workspace
        .members
        .iter()
        .enumerate()
        .filter(|(_, m)| m.releasable())
        .map(|(idx, _)| idx)
        .collect();
    members.extend(bundles.values().flatten().copied());
    let unreleased = slash_path(&Path::new(&workspace.config.changelog.dir).join("unreleased"));
    let tree = git_stdout(
        &workspace.root,
        &["ls-tree", "-r", "-z", "--full-tree", revision],
    )?;
    let prefix = git_stdout(&workspace.root, &["rev-parse", "--show-prefix"])?;
    let mut result = BTreeMap::new();
    for record in tree.split('\0').filter(|r| !r.is_empty()) {
        let (meta, file) = record.split_once('\t').context("invalid git tree record")?;
        let Some(file) = file.strip_prefix(prefix.trim_end()) else {
            continue;
        };
        let object = meta
            .split_whitespace()
            .nth(2)
            .context("git tree record has no object")?;
        let fragment = file.starts_with(&format!("{unreleased}/")) && file.ends_with(".toml");
        let source = members.iter().any(|&idx| {
            let member = &workspace.members[idx];
            let relative = if member.rel_path == "." {
                Some(file)
            } else {
                file.strip_prefix(&format!("{}/", member.rel_path))
            };
            relative.is_some_and(|p| {
                p == "gleam.toml" || p.starts_with("src/") || p.starts_with("priv/")
            })
        });
        if fragment || source {
            result.insert(file.to_string(), object.to_string());
        }
    }
    // Release configuration can change ownership and version planning, unlike unrelated root docs.
    if let Some(manifest) = file_at(&workspace.root, revision, "gleam.toml")? {
        let document: toml::Value = toml::from_str(&manifest)?;
        if let Some(config) = document
            .get("tools")
            .and_then(|t| t.get("trellis"))
            .and_then(toml::Value::as_table)
        {
            let mut config = config.clone();
            config.retain(|key, _| ["members", "exclude", "publish", "changelog"].contains(&key));
            for (table, keys) in [
                ("exclude", &["@release", "@members"][..]),
                (
                    "publish",
                    &[
                        "bundle",
                        "lifecycle",
                        "path_dep_requirement",
                        "exact_tag_format",
                        "series_tag_format",
                        "package_tags",
                        "package_tags_overrides",
                        "repository_tag_package",
                        "repository_tag_format",
                        "repository_tags",
                    ][..],
                ),
                (
                    "changelog",
                    &[
                        "dir",
                        "kinds",
                        "categories",
                        "header_format",
                        "version_format",
                        "category_format",
                        "uncategorized_label",
                        "kind_format",
                        "change_format",
                        "dependency_kind",
                        "dependency_body",
                    ][..],
                ),
            ] {
                if let Some(settings) = config.get_mut(table).and_then(toml::Value::as_table_mut) {
                    settings.retain(|key, _| keys.contains(&key));
                }
            }
            result.insert("@release_config".to_string(), toml::to_string(&config)?);
        }
    }
    Ok(result)
}
