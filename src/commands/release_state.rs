use super::version::PlanEntry;
use super::version_override::Overrides;
use crate::git::{git_output, git_stdout, slash_path};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug)]
pub(super) struct State {
    pub packages: BTreeMap<String, semver::Version>,
    pub inputs: BTreeMap<String, String>,
}

fn path(workspace: &Workspace) -> String {
    slash_path(&Path::new(&workspace.config.changelog.dir).join("release.toml"))
}

pub(super) fn validate_location(workspace: &Workspace) -> Result<()> {
    let dir = Path::new(&workspace.config.changelog.dir);
    ensure!(
        !dir.is_absolute()
            && !dir
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "release metadata cleanup requires a repository-relative changelog directory"
    );
    let mut location = workspace.root.clone();
    for component in Path::new(&path(workspace)).components() {
        location.push(component);
        match std::fs::symlink_metadata(&location) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "release metadata path contains a symlink: {}",
                location.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("cannot inspect release metadata location"),
        }
    }
    Ok(())
}

pub(super) fn remove(workspace: &Workspace) -> Result<()> {
    validate_location(workspace)?;
    match std::fs::remove_file(workspace.root.join(path(workspace))) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("cannot remove obsolete release metadata"),
    }
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

fn manifest_version(workspace: &Workspace, text: &str) -> Result<String> {
    let field = workspace
        .adapter()
        .map_or("version", |adapter| adapter.version.as_str());
    crate::manifest::read_string(
        text,
        crate::manifest::Format::from_path(workspace.manifest_rel())?,
        field,
    )?
    .context("package manifest has no version")
}

impl State {
    pub fn from_pr(workspace: &Workspace, base_commit: &str, head: &str) -> Result<Self> {
        ensure!(
            !ancestor(&workspace.root, head, base_commit)?,
            "stale pending release: head is already merged; close its PR before continuing"
        );
        let bases = git_stdout(&workspace.root, &["merge-base", "--all", base_commit, head])
            .context("cannot find pending release source; fetch full history")?;
        ensure!(
            bases.lines().count() == 1,
            "pending release has no unambiguous source baseline; fetch full history"
        );
        let source_commit = bases.trim();
        let source = workspace_at(&workspace.root, source_commit, base_commit)?;
        let proposed = workspace_at(&workspace.root, head, base_commit)?;
        for member in proposed.members.iter().filter(|m| m.releasable()) {
            ensure!(
                source.member_index(&member.name).is_some(),
                "pending release package `{}` has no source baseline; use --no-resume with an explicit request",
                member.name
            );
        }
        let mut packages = BTreeMap::new();
        for member in source.members.iter().filter(|m| m.releasable()) {
            let name = &member.name;
            let pending_idx = proposed
                .member_index(name)
                .with_context(|| format!("pending release package `{name}` disappeared"))?;
            let pending = &proposed.members[pending_idx];
            let current = semver::Version::parse(member.version())?;
            let next = semver::Version::parse(pending.version())?;
            if next == current {
                continue;
            }
            let idx = workspace
                .member_index(name)
                .with_context(|| format!("stale pending release: package `{name}` disappeared"))?;
            let selected = &workspace.members[idx];
            ensure!(
                next > current,
                "pending release version for `{name}` is not forward"
            );
            ensure!(
                pending.releasable()
                    && selected.releasable()
                    && pending.rel_file(proposed.manifest_rel())
                        == member.rel_file(source.manifest_rel())
                    && selected.rel_file(workspace.manifest_rel())
                        == member.rel_file(source.manifest_rel()),
                "stale pending release: package `{name}` moved or is no longer releasable"
            );
            ensure!(
                selected.version() == member.version(),
                "stale pending release: `{name}` is now {}, expected {}; use --no-resume with an explicit request",
                selected.version(),
                member.version()
            );
            packages.insert(name.clone(), next);
        }
        ensure!(
            !packages.is_empty(),
            "pending PR has no proposed release versions"
        );
        Ok(Self {
            packages,
            inputs: inputs(&source, source_commit)?,
        })
    }

    pub(super) fn validate_pending_series(
        workspace: &Workspace,
        head: &str,
        plan: &[PlanEntry],
    ) -> Result<()> {
        for entry in plan {
            let path = workspace.members[entry.member].rel_file(workspace.manifest_rel());
            let Some(text) = file_at(&workspace.root, head, &path)? else {
                continue;
            };
            let version =
                manifest_version(workspace, &text).context("invalid pending package manifest")?;
            let version =
                semver::Version::parse(&version).context("invalid pending package version")?;
            super::version_override::validate_series(&entry.name, &version, &entry.next)?;
        }
        Ok(())
    }

    pub fn resume(&self, workspace: &Workspace, explicit: &Overrides) -> Result<Overrides> {
        let candidates: Vec<&str> = workspace
            .members
            .iter()
            .filter(|m| m.releasable())
            .filter(|m| semver::Version::parse(m.version()).is_ok_and(|v| !v.pre.is_empty()))
            .map(|m| m.name.as_str())
            .collect();
        let labels: BTreeSet<&str> = self
            .packages
            .values()
            .filter(|p| !p.pre.is_empty())
            .map(|p| {
                let (label, counter) = p
                    .pre
                    .as_str()
                    .rsplit_once('.')
                    .context("pending candidate has no counter")?;
                ensure!(
                    !label.contains('.') && counter.parse::<u64>().is_ok(),
                    "unsupported pending candidate `{p}`; use --no-resume with an explicit request"
                );
                Ok(label)
            })
            .collect::<Result<_>>()?;
        ensure!(
            labels.len() <= 1,
            "conflicting candidate labels in pending release; use --no-resume with an explicit request"
        );
        let pre = labels.first().copied().or_else(|| {
            self.packages
                .keys()
                .any(|name| candidates.contains(&name.as_str()))
                .then_some("none")
        });
        Ok(
            Overrides::from_proposed(self.packages.clone(), pre, &candidates)?
                .merged(explicit, &candidates),
        )
    }

    pub fn matches_plan(&self, plan: &[PlanEntry]) -> bool {
        self.packages.len() == plan.len()
            && plan.iter().all(|p| {
                self.packages
                    .get(&p.name)
                    .is_some_and(|stored| *stored == p.next)
            })
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
    let mut baselines = BTreeMap::new();
    for member in workspace.members.iter().filter(|m| m.releasable()) {
        let current = semver::Version::parse(member.version())?;
        if current.pre.is_empty() {
            continue;
        }
        let manifest_path = member.rel_file(workspace.manifest_rel());
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
            if manifest_version(workspace, &manifest)? != member.version() {
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
                        workspace.adapter().is_some()
                            || file == workspace.manifest_rel()
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
                if workspace.adapter().is_some() {
                    p != "CHANGELOG.md"
                        && !file.starts_with(&format!("{}/", workspace.config.changelog.dir))
                } else {
                    p == workspace.manifest_rel() || p.starts_with("src/") || p.starts_with("priv/")
                }
            })
        });
        if fragment || source {
            result.insert(file.to_string(), object.to_string());
        }
    }
    // Release configuration can change ownership and version planning, unlike unrelated root docs.
    if let Some(manifest) = file_at(&workspace.root, revision, workspace.config_rel)? {
        let document: toml::Value = toml::from_str(&manifest)?;
        if let Some(config) = document
            .get("tools")
            .and_then(|t| t.get("trellis"))
            .and_then(toml::Value::as_table)
        {
            let mut config = config.clone();
            config.retain(|key, _| {
                ["members", "exclude", "adapter", "publish", "changelog"].contains(&key)
            });
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
