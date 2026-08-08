//! The workspace model: member discovery, `gleam.toml` parsing, and the
//! dependency graph. Every command starts here; the topological order is
//! computed once and consumed everywhere.

use crate::config::{ConfigFile, ReleaseLifecycle, TagLevel};
use crate::gleam::GleamManifest;
use crate::json::{Check, Finding};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

pub const GLEAM_TOML: &str = "gleam.toml";

#[derive(Debug)]
pub struct Member {
    pub name: String,
    /// Absolute path to the member directory.
    pub path: PathBuf,
    /// Path relative to the workspace root, with forward slashes.
    pub rel_path: String,
    pub manifest: GleamManifest,
    /// Resolved release lifecycle: `publish.lifecycle.default`, overridden by
    /// a legacy `exclude.@release` match, overridden in turn by an explicit
    /// `publish.lifecycle.packages` match. See [`Workspace::load_with_diagnostics`].
    pub lifecycle: ReleaseLifecycle,
    /// Which tags a release maintains for this member: the workspace
    /// `package_tags`, unless a `package_tags_overrides` glob claims it.
    pub tags: Vec<TagLevel>,
}

impl Member {
    /// True when this member is published to Hex (`lifecycle == hex`).
    pub fn publishes_to_hex(&self) -> bool {
        self.lifecycle == ReleaseLifecycle::Hex
    }

    /// `true` for both `git_only` and `hex` — changelog, version, and
    /// tag/release commands select on this; `publish` alone needs
    /// `lifecycle == hex` specifically.
    pub fn releasable(&self) -> bool {
        self.lifecycle != ReleaseLifecycle::Workspace
    }

    pub fn version(&self) -> &str {
        &self.manifest.version
    }
}

#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub config: ConfigFile,
    /// True when no `[tools.trellis]` table exists anywhere: the root was
    /// inferred from git and the configuration is entirely defaulted.
    pub configless: bool,
    /// Members in topological order (dependencies before dependents).
    pub members: Vec<Member>,
    /// Index of the repository tag anchor member. `None` when the feature
    /// is unconfigured — or, under doctor's lenient load, when the configured
    /// anchor is missing or unreleasable (an error diagnostic either way, so
    /// strict-loading commands never run without it).
    pub repository_series_anchor: Option<usize>,
    /// Direct workspace dependencies, indexed like `members`.
    deps: Vec<Vec<usize>>,
    /// Direct workspace dependents, indexed like `members`.
    dependents: Vec<Vec<usize>>,
    /// Direct workspace dependencies from *runtime* (`[dependencies]`) path
    /// deps only, indexed like `members`. Subset of `deps`, used by the
    /// `release_boundary` lifecycle-availability check — a dev-only path dep
    /// never ships in a distribution, so it never constrains it.
    runtime_deps: Vec<Vec<usize>>,
}

/// Problems collected while loading. `Workspace::load` turns any error into a
/// failure; `trellis doctor` reports them all instead — which is why these are
/// [`Finding`]s and not strings: doctor's structured output needs the check
/// identity and file attribution that prose would have dissolved.
#[derive(Debug, Default)]
pub struct Diagnostics {
    pub findings: Vec<Finding>,
}

impl Diagnostics {
    fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// Error messages only, in the order they were found.
    pub fn errors(&self) -> impl Iterator<Item = &str> {
        self.findings
            .iter()
            .filter(|f| f.is_error())
            .map(|f| f.message.as_str())
    }

    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(Finding::is_error)
    }
}

impl Workspace {
    /// Walk up from `start` looking for a `gleam.toml` with a
    /// `[tools.trellis]` table — the workspace root marker. Member manifests
    /// (gleam.toml without the table) are skipped, so commands work from
    /// inside a package, like `git` or `cargo`.
    ///
    /// When no manifest anywhere up the tree has the table, trellis runs
    /// configless: the enclosing git repository root becomes the workspace
    /// root and members are auto-discovered from git. An unparseable ancestor
    /// manifest blocks the fallback — it may be the intended root hiding
    /// behind a syntax error, and guessing would silently change modes.
    pub fn find_root(start: &Path) -> Result<PathBuf> {
        let start = start
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", start.display()))?;
        let mut unparseable: Vec<PathBuf> = Vec::new();
        for dir in start.ancestors() {
            let manifest = dir.join(GLEAM_TOML);
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            match toml::from_str::<toml::Value>(&text) {
                Ok(document) if crate::config::has_trellis_table(&document) => {
                    return Ok(dir.to_path_buf());
                }
                Ok(_) => {} // a package manifest; keep walking
                Err(_) => unparseable.push(manifest),
            }
        }
        if !unparseable.is_empty() {
            bail!(
                "no {GLEAM_TOML} with a [tools.trellis] table found in {} or any parent \
                 directory, and {} could not be parsed and may be the intended workspace root",
                start.display(),
                unparseable
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(root) = crate::git::repo_root(&start) {
            return Ok(root);
        }
        bail!(
            "no {GLEAM_TOML} with a [tools.trellis] table found in {} or any parent directory, \
             and it is not inside a git repository (configless mode discovers members from git)",
            start.display()
        )
    }

    /// Strict load: any diagnostic error is fatal.
    pub fn load(start: &Path) -> Result<Self> {
        let root = Self::find_root(start)?;
        let (workspace, diagnostics) = Self::load_with_diagnostics(&root)?;
        if diagnostics.has_errors() {
            bail!(
                "workspace is invalid:\n  - {}\n(run `trellis doctor` for details)",
                diagnostics.errors().collect::<Vec<_>>().join("\n  - ")
            );
        }
        workspace.context("workspace could not be loaded")
    }

    /// Lenient load for `doctor`: collects every problem it can find and
    /// returns a best-effort model. The workspace is `None` only when no
    /// coherent model exists (unreadable config or a dependency cycle).
    pub fn load_with_diagnostics(root: &Path) -> Result<(Option<Self>, Diagnostics)> {
        let mut diagnostics = Diagnostics::default();
        // The root's gleam.toml decides the mode: a [tools.trellis] table is
        // configuration, its absence (or a missing manifest — a configless
        // git-root workspace) means everything is defaulted and discovered.
        let manifest_path = root.join(GLEAM_TOML);
        let (configless, root_is_package) = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => match toml::from_str::<toml::Value>(&text) {
                Ok(document) => (
                    !crate::config::has_trellis_table(&document),
                    document.get("name").is_some(),
                ),
                Err(err) => {
                    diagnostics.push(
                        Finding::error(
                            Check::WorkspaceConfig,
                            format!("failed to parse {}: {err}", manifest_path.display()),
                        )
                        .at(GLEAM_TOML),
                    );
                    return Ok((None, diagnostics));
                }
            },
            Err(_) => (true, false),
        };
        let config = if configless {
            ConfigFile::configless()
        } else {
            match ConfigFile::load(&manifest_path) {
                Ok(config) => config,
                Err(err) => {
                    diagnostics.push(
                        Finding::error(Check::WorkspaceConfig, format!("{err:#}")).at(GLEAM_TOML),
                    );
                    return Ok((None, diagnostics));
                }
            }
        };
        report_unknown_config_keys(&config, &mut diagnostics);

        let mut member_dirs = match &config.members {
            Some(globs) => expand_member_globs(root, globs, &mut diagnostics),
            None => discover_member_dirs(root, &mut diagnostics),
        };
        // A config-only root manifest ([tools.trellis] without a `name`) is
        // configuration, not a package — discovery must not sweep it in.
        if config.members.is_none() && !root_is_package {
            member_dirs.retain(|dir| dir != root);
        }

        // Parse each member manifest; unparseable members are reported and dropped.
        for (task, patterns) in &config.exclude {
            if let Err(err) = build_globset(patterns) {
                diagnostics.push(
                    Finding::error(
                        Check::WorkspaceConfig,
                        format!("invalid `{task}` exclusion glob: {err:#}"),
                    )
                    .at(GLEAM_TOML),
                );
            }
        }

        // `@members` removes directories from membership entirely, before
        // their manifests are even parsed. A working exclusion glob can never
        // match anything once it's done its job, so the typo check has to run
        // here, against the pre-filter candidates, not against the survivors.
        if let Some(patterns) = config.exclude.get(crate::config::MEMBERS_EXCLUDE_KEY) {
            check_members_exclude_globs(root, &member_dirs, patterns, &mut diagnostics);
            if let Ok(excludes) = build_globset(patterns) {
                member_dirs.retain(|dir| !excludes.is_match(rel_path_string(root, dir)));
            }
        }
        if member_dirs.is_empty() && !diagnostics.has_errors() {
            diagnostics.push(
                Finding::error(
                    Check::WorkspaceConfig,
                    format!(
                        "no workspace members left after `{}` exclusions",
                        crate::config::MEMBERS_EXCLUDE_KEY
                    ),
                )
                .at(GLEAM_TOML),
            );
        }

        let release_exclusions = config
            .exclude
            .get(crate::config::RELEASE_EXCLUDE_KEY)
            .cloned()
            .unwrap_or_default();
        let release_excludes = build_globset(&release_exclusions)
            .map_err(|err| {
                diagnostics.push(
                    Finding::error(
                        Check::WorkspaceConfig,
                        format!("invalid release exclusion glob: {err:#}"),
                    )
                    .at(GLEAM_TOML),
                );
            })
            .ok();
        // Keyed by one member-path glob each, like `publish.lifecycle.packages`
        // below, so a member can match several with different lists — the case
        // `resolve_package_tags` must reject.
        let mut package_tags_overrides: Vec<(Vec<TagLevel>, globset::GlobMatcher)> = Vec::new();
        for (pattern, levels) in &config.publish.package_tags_overrides {
            match globset::Glob::new(pattern) {
                Ok(glob) => package_tags_overrides.push((levels.clone(), glob.compile_matcher())),
                Err(err) => {
                    diagnostics.push(
                        Finding::error(
                            Check::WorkspaceConfig,
                            format!("invalid `package_tags_overrides` glob `{pattern}`: {err:#}"),
                        )
                        .at(GLEAM_TOML),
                    );
                }
            }
        }
        // `publish.lifecycle.packages` globs, compiled individually — each key
        // names exactly one glob, so a member can match several with different
        // targets, which is the case `resolve_lifecycle` must reject.
        let mut lifecycle_overrides: Vec<(ReleaseLifecycle, globset::GlobMatcher)> = Vec::new();
        for (pattern, lifecycle) in &config.publish.lifecycle.packages {
            match globset::Glob::new(pattern) {
                Ok(glob) => lifecycle_overrides.push((*lifecycle, glob.compile_matcher())),
                Err(err) => {
                    diagnostics.push(
                        Finding::error(
                            Check::WorkspaceConfig,
                            format!(
                                "invalid `publish.lifecycle.packages` glob `{pattern}`: {err:#}"
                            ),
                        )
                        .at(GLEAM_TOML),
                    );
                }
            }
        }
        let mut members = Vec::new();
        for dir in member_dirs {
            let rel_path = rel_path_string(root, &dir);
            let manifest_path = dir.join("gleam.toml");
            if !manifest_path.is_file() {
                diagnostics.push(
                    Finding::error(
                        Check::PackageManifest,
                        format!("member `{rel_path}` has no gleam.toml"),
                    )
                    .at(format!("{rel_path}/{GLEAM_TOML}")),
                );
                continue;
            }
            match GleamManifest::load(&manifest_path) {
                Ok(manifest) => {
                    // A member manifest with its own [tools.trellis] would
                    // hijack root discovery for commands run inside it.
                    if manifest.has_trellis_config && dir != root {
                        let message = if configless {
                            format!(
                                "`{rel_path}/gleam.toml` has a [tools.trellis] table but the \
                                 workspace root was inferred as `{}`; run trellis from \
                                 `{rel_path}`, or move the table to the repository root",
                                root.display()
                            )
                        } else {
                            format!(
                                "member `{rel_path}` has a [tools.trellis] table; only the \
                                 workspace root's gleam.toml may have one"
                            )
                        };
                        diagnostics.push(
                            Finding::error(Check::PackageManifest, message)
                                .at(format!("{rel_path}/{GLEAM_TOML}"))
                                .in_package(manifest.name.clone()),
                        );
                    }
                    let lifecycle = resolve_lifecycle(
                        &lifecycle_overrides,
                        release_excludes.as_ref(),
                        &rel_path,
                        config.publish.lifecycle.default,
                        &mut diagnostics,
                    );
                    let tags = resolve_package_tags(
                        &package_tags_overrides,
                        &rel_path,
                        &config.publish.package_tags,
                        &mut diagnostics,
                    );
                    members.push(Member {
                        name: manifest.name.clone(),
                        path: dir,
                        rel_path,
                        manifest,
                        lifecycle,
                        tags,
                    });
                }
                Err(err) => diagnostics.push(
                    Finding::error(Check::PackageManifest, format!("{err:#}"))
                        .at(format!("{rel_path}/{GLEAM_TOML}")),
                ),
            }
        }

        // Duplicate names would make every name-keyed operation ambiguous.
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for member in &members {
            if let Some(other) = seen.insert(&member.name, &member.rel_path) {
                diagnostics.push(
                    Finding::error(
                        Check::PackageManifest,
                        format!(
                            "duplicate package name `{}` in `{}` and `{}`",
                            member.name, other, member.rel_path
                        ),
                    )
                    .at(format!("{}/{GLEAM_TOML}", member.rel_path))
                    .in_package(member.name.clone()),
                );
            }
        }

        if let Some(anchor) = &config.publish.repository_tag_package {
            match members.iter().find(|member| &member.name == anchor) {
                None => diagnostics.push(
                    Finding::error(
                        Check::WorkspaceConfig,
                        format!("`repository_tag_package` `{anchor}` is not a workspace member"),
                    )
                    .at(GLEAM_TOML),
                ),
                Some(member) if !member.releasable() => diagnostics.push(
                    Finding::error(
                        Check::ReleaseBoundary,
                        format!("`repository_tag_package` `{anchor}` is excluded from release"),
                    )
                    .at(GLEAM_TOML)
                    .in_package(&member.name),
                ),
                Some(_) => {}
            }
        }

        // Resolve path dependencies between members into graph edges. Runtime
        // edges (non-dev) are also tracked separately: dev-only path deps
        // never ship in a distribution, so the lifecycle-availability check
        // must not see them.
        let path_to_idx: HashMap<PathBuf, usize> = members
            .iter()
            .enumerate()
            .map(|(idx, member)| (member.path.clone(), idx))
            .collect();
        let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
        let mut runtime_edges: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (idx, member) in members.iter().enumerate() {
            // Every problem here is a claim about this member's manifest, so
            // they all annotate the same file.
            let blame = |message: String| {
                Finding::error(Check::PathDependency, message)
                    .at(format!("{}/{GLEAM_TOML}", member.rel_path))
                    .in_package(member.name.clone())
            };
            for (dep_name, dep_path, dev) in member.manifest.path_deps() {
                let resolved = normalize_path(&member.path.join(dep_path));
                if !resolved.starts_with(root) {
                    diagnostics.push(blame(format!(
                        "package `{}`: path dependency `{dep_name}` ({dep_path}) points outside the workspace",
                        member.name
                    )));
                    continue;
                }
                match path_to_idx.get(&resolved) {
                    Some(&dep_idx) => {
                        if members[dep_idx].name != dep_name {
                            diagnostics.push(blame(format!(
                                "package `{}`: path dependency `{dep_name}` resolves to `{}`, which is named `{}`",
                                member.name, members[dep_idx].rel_path, members[dep_idx].name
                            )));
                        }
                        if dep_idx == idx {
                            diagnostics.push(blame(format!(
                                "package `{}` path-depends on itself",
                                member.name
                            )));
                        } else {
                            edges.insert((dep_idx, idx)); // dependency -> dependent
                            if !dev {
                                runtime_edges.insert((dep_idx, idx));
                            }
                        }
                    }
                    None => diagnostics.push(blame(format!(
                        "package `{}`: path dependency `{dep_name}` ({dep_path}) is not a workspace member",
                        member.name
                    ))),
                }
            }
        }

        let names: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
        let edge_list: Vec<(usize, usize)> = edges.iter().copied().collect();
        let order = match toposort(members.len(), &names, &edge_list) {
            Ok(order) => order,
            Err(cycle) => {
                diagnostics.push(Finding::error(
                    Check::DependencyCycle,
                    format!(
                        "dependency cycle between workspace members: {}",
                        cycle.join(" -> ")
                    ),
                ));
                return Ok((None, diagnostics));
            }
        };

        // Reorder members topologically and remap adjacency.
        let mut new_index = vec![0usize; members.len()];
        for (new, &old) in order.iter().enumerate() {
            new_index[old] = new;
        }
        let mut ordered: Vec<Option<Member>> = members.into_iter().map(Some).collect();
        let members: Vec<Member> = order
            .iter()
            .map(|&old| ordered[old].take().expect("each index appears once"))
            .collect();
        let mut deps = vec![Vec::new(); members.len()];
        let mut dependents = vec![Vec::new(); members.len()];
        for &(dep, dependent) in &edge_list {
            let (dep, dependent) = (new_index[dep], new_index[dependent]);
            deps[dependent].push(dep);
            dependents[dep].push(dependent);
        }
        let mut runtime_deps = vec![Vec::new(); members.len()];
        for &(dep, dependent) in &runtime_edges {
            runtime_deps[new_index[dependent]].push(new_index[dep]);
        }
        for list in deps
            .iter_mut()
            .chain(dependents.iter_mut())
            .chain(runtime_deps.iter_mut())
        {
            list.sort_unstable();
        }

        let repository_series_anchor =
            config
                .publish
                .repository_tag_package
                .as_ref()
                .and_then(|anchor| {
                    members
                        .iter()
                        .position(|member| &member.name == anchor && member.releasable())
                });
        let workspace = Workspace {
            root: root.to_path_buf(),
            config,
            configless,
            members,
            repository_series_anchor,
            deps,
            dependents,
            runtime_deps,
        };
        Ok((Some(workspace), diagnostics))
    }

    pub fn member_index(&self, name: &str) -> Option<usize> {
        self.members.iter().position(|m| m.name == name)
    }

    /// `path` relative to the workspace root, with forward slashes — the form
    /// `doctor`'s findings and GitHub annotations use. Paths outside the root
    /// are returned unchanged rather than rewritten into `../` chains.
    pub fn rel_path_of(&self, path: &Path) -> String {
        rel_path_string(&self.root, path)
    }

    /// Direct *runtime* (`[dependencies]`, not `[dev-dependencies]`) workspace
    /// dependencies of a member — the subset of [`Workspace::deps_of`] that
    /// would actually ship in that member's distribution.
    pub fn runtime_deps_of(&self, idx: usize) -> &[usize] {
        &self.runtime_deps[idx]
    }

    /// Direct workspace dependencies of a member.
    pub fn deps_of(&self, idx: usize) -> &[usize] {
        &self.deps[idx]
    }

    /// Direct workspace dependents of a member.
    pub fn dependents_of(&self, idx: usize) -> &[usize] {
        &self.dependents[idx]
    }

    pub fn transitive_deps(&self, idx: usize) -> HashSet<usize> {
        self.closure(idx, &self.deps)
    }

    pub fn transitive_dependents(&self, idx: usize) -> HashSet<usize> {
        self.closure(idx, &self.dependents)
    }

    fn closure(&self, start: usize, adjacency: &[Vec<usize>]) -> HashSet<usize> {
        let mut seen = HashSet::new();
        let mut stack = adjacency[start].clone();
        while let Some(next) = stack.pop() {
            if seen.insert(next) {
                stack.extend(adjacency[next].iter().copied());
            }
        }
        seen
    }

    /// Resolve a set of member names/filters into topologically ordered indices.
    pub fn select(&self, filter: &SelectionFilter) -> Result<Vec<usize>> {
        let mut selected: HashSet<usize> = if filter.names.is_empty() {
            (0..self.members.len()).collect()
        } else {
            let mut set = HashSet::new();
            for name in &filter.names {
                let idx = self.member_index(name).with_context(|| {
                    format!(
                        "unknown package `{name}` (members: {})",
                        self.members
                            .iter()
                            .map(|m| m.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
                set.insert(idx);
            }
            set
        };

        if let Some(since) = &filter.since {
            let changed = crate::git::changed_members(self, since)?;
            selected.retain(|idx| changed.contains(idx));
        }

        if filter.with_dependents {
            let base: Vec<usize> = selected.iter().copied().collect();
            for idx in base {
                selected.extend(self.transitive_dependents(idx));
            }
        }

        if filter.releasable_only {
            selected.retain(|&idx| self.members[idx].releasable());
        }

        let mut ordered: Vec<usize> = selected.into_iter().collect();
        ordered.sort_unstable(); // member indices are already topological
        Ok(ordered)
    }
}

#[derive(Debug, Default)]
pub struct SelectionFilter {
    /// Explicit package names; empty means all members.
    pub names: Vec<String>,
    /// Restrict to members owning files changed since this git ref.
    pub since: Option<String>,
    /// Add the reverse-dependency closure of the selection.
    pub with_dependents: bool,
    /// Drop members matching the `@release` exclusion glob.
    pub releasable_only: bool,
}

/// Kahn's algorithm with an alphabetical tie-break, so the order is
/// deterministic across runs and platforms. Returns member indices in
/// dependency order, or one cycle (as names) on failure.
pub fn toposort(
    n: usize,
    names: &[String],
    edges: &[(usize, usize)],
) -> Result<Vec<usize>, Vec<String>> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let mut in_degree = vec![0usize; n];
    let mut adjacency = vec![Vec::new(); n];
    for &(from, to) in edges {
        in_degree[to] += 1;
        adjacency[from].push(to);
    }
    let mut ready: BinaryHeap<Reverse<(&str, usize)>> = (0..n)
        .filter(|&idx| in_degree[idx] == 0)
        .map(|idx| Reverse((names[idx].as_str(), idx)))
        .collect();
    let mut order = Vec::with_capacity(n);
    while let Some(Reverse((_, idx))) = ready.pop() {
        order.push(idx);
        for &next in &adjacency[idx] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                ready.push(Reverse((names[next].as_str(), next)));
            }
        }
    }
    if order.len() == n {
        return Ok(order);
    }

    // Extract one concrete cycle for the error message.
    let in_cycle: HashSet<usize> = (0..n).filter(|&idx| in_degree[idx] > 0).collect();
    let start = *in_cycle.iter().min().expect("cycle is non-empty");
    let mut path = vec![start];
    let mut seen = HashSet::from([start]);
    let mut current = start;
    loop {
        let next = adjacency[current]
            .iter()
            .copied()
            .find(|next| in_cycle.contains(next))
            .expect("every cycle node has a successor in the cycle");
        if !seen.insert(next) {
            let cycle_start = path.iter().position(|&idx| idx == next).unwrap_or(0);
            let mut cycle: Vec<String> = path[cycle_start..]
                .iter()
                .map(|&idx| names[idx].clone())
                .collect();
            cycle.push(names[next].clone());
            return Err(cycle);
        }
        path.push(next);
        current = next;
    }
}

/// Report keys under `[tools.trellis]` that trellis does not recognize, and
/// keys still spelled the pre-0.8 kebab-case way.
///
/// Both are **warnings**. An unrecognized key may simply belong to a newer
/// trellis, so a workspace using one still loads under a pinned older one; a
/// deprecated key still configures what it always did, so failing on it would
/// break working repositories for a spelling change.
fn report_unknown_config_keys(config: &ConfigFile, diagnostics: &mut Diagnostics) {
    for key in &config.deprecated_keys {
        diagnostics.push(
            Finding::warning(
                Check::WorkspaceConfig,
                format!(
                    "[tools.trellis] key `{}` is deprecated; rename it to `{}` \
                     (trellis config keys are snake_case)",
                    key.path, key.replacement
                ),
            )
            .at(GLEAM_TOML),
        );
    }
    for path in &config.unknown_keys {
        diagnostics.push(
            Finding::warning(
                Check::WorkspaceConfig,
                format!(
                    "[tools.trellis] key `{path}` is not recognized and is being ignored; \
                     it may belong to a newer trellis"
                ),
            )
            .at(GLEAM_TOML),
        );
    }
}

/// Validates `@members` exclusion globs against the pre-filter candidate set
/// — the same globs are applied as a `retain` right after this runs, so
/// checking them afterward against the survivors would mean a working
/// exclusion could never appear to match anything.
fn check_members_exclude_globs(
    root: &Path,
    member_dirs: &[PathBuf],
    patterns: &[String],
    diagnostics: &mut Diagnostics,
) {
    let rel_paths: Vec<String> = member_dirs
        .iter()
        .map(|dir| rel_path_string(root, dir))
        .collect();
    for pattern in patterns {
        match globset::Glob::new(pattern) {
            Ok(glob) => {
                let matcher = glob.compile_matcher();
                if !rel_paths.iter().any(|rel| matcher.is_match(rel)) {
                    diagnostics.push(
                        Finding::error(
                            Check::ExclusionGlob,
                            format!(
                                "`@members` exclusion glob `{pattern}` matches no member (typo?)"
                            ),
                        )
                        .at(GLEAM_TOML),
                    );
                }
            }
            Err(_) => diagnostics.push(
                Finding::error(
                    Check::ExclusionGlob,
                    format!("`@members` exclusion glob `{pattern}` is invalid"),
                )
                .at(GLEAM_TOML),
            ),
        }
    }
}

fn expand_member_globs(
    root: &Path,
    patterns: &[String],
    diagnostics: &mut Diagnostics,
) -> Vec<PathBuf> {
    let mut dirs = BTreeSet::new();
    let mut wildcard_patterns = Vec::new();

    for pattern in patterns {
        let full = root.join(pattern);
        let Some(full) = full.to_str() else {
            diagnostics.push(Finding::error(
                Check::MemberGlob,
                format!("member glob `{pattern}` is not valid UTF-8"),
            ));
            continue;
        };
        // A literal member path is a promise that a package lives there, so a
        // missing gleam.toml stays a hard error downstream. A wildcard pattern
        // sweeps directories that merely live alongside packages (node_modules,
        // asset dirs), so matches without a gleam.toml are skipped.
        let is_wildcard = pattern.contains(['*', '?', '[']);
        if is_wildcard {
            match glob::Pattern::new(full) {
                Ok(matcher) => wildcard_patterns.push((pattern, matcher, 0usize)),
                Err(err) => diagnostics.push(Finding::error(
                    Check::MemberGlob,
                    format!("invalid member glob `{pattern}`: {err}"),
                )),
            }
            continue;
        }

        let path = Path::new(full);
        if path.is_dir() {
            dirs.insert(normalize_path(path));
        } else {
            diagnostics.push(Finding::error(
                Check::MemberGlob,
                format!("member glob `{pattern}` matches no packages"),
            ));
        }
    }

    if !wildcard_patterns.is_empty() {
        let mut builder = ignore::WalkBuilder::new(root);
        builder
            .hidden(false)
            .ignore(false)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(false)
            .parents(true)
            .require_git(true)
            .follow_links(true)
            .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git");

        let match_options = glob::MatchOptions {
            require_literal_separator: true,
            ..Default::default()
        };
        for entry in builder.build() {
            match entry {
                Ok(entry)
                    if entry
                        .file_type()
                        .is_some_and(|file_type| file_type.is_dir())
                        && entry.path().join(GLEAM_TOML).is_file() =>
                {
                    for (_, matcher, matched) in &mut wildcard_patterns {
                        if matcher.matches_path_with(entry.path(), match_options) {
                            *matched += 1;
                            dirs.insert(normalize_path(entry.path()));
                        }
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    diagnostics.push(Finding::warning(
                        Check::MemberGlob,
                        format!("while expanding member globs: {err}"),
                    ));
                }
            }
        }
    }

    for (pattern, _, matched) in wildcard_patterns {
        if matched == 0 {
            diagnostics.push(Finding::error(
                Check::MemberGlob,
                format!("member glob `{pattern}` matches no packages"),
            ));
        }
    }

    dirs.into_iter().collect()
}

/// Auto-discovery: every directory owning a non-gitignored `gleam.toml` is a
/// member. Gleam's `build/` tree is skipped unconditionally — it holds a
/// manifest for every downloaded dependency, and while it is conventionally
/// gitignored, membership must not hinge on that.
fn discover_member_dirs(root: &Path, diagnostics: &mut Diagnostics) -> Vec<PathBuf> {
    let manifests = match crate::git::ls_gleam_manifests(root) {
        Ok(manifests) => manifests,
        Err(err) => {
            diagnostics.push(Finding::error(
                Check::MemberGlob,
                format!("cannot auto-discover members: {err:#}"),
            ));
            return Vec::new();
        }
    };
    let mut dirs = BTreeSet::new();
    for manifest in &manifests {
        let path = Path::new(manifest);
        if path.components().any(|c| c.as_os_str() == "build") {
            continue;
        }
        let dir = path.parent().unwrap_or(Path::new(""));
        dirs.insert(normalize_path(&root.join(dir)));
    }
    if dirs.is_empty() {
        diagnostics.push(Finding::error(
            Check::MemberGlob,
            format!(
                "no members to auto-discover: no gleam.toml found under {} \
                 (gitignored paths are not searched); add packages, or configure \
                 `members` in a [tools.trellis] table",
                root.display()
            ),
        ));
    }
    dirs.into_iter().collect()
}

/// Auto-discovered member paths relative to `root`, forward-slashed.
///
/// The same discovery a configured workspace performs when `members` is
/// omitted, so `trellis init` reports exactly the list the config it writes
/// will produce, rather than a second approximation of it. Discovery problems
/// are dropped: `init` has no report to put them in, and the `doctor` run it
/// finishes with raises them properly.
pub fn discovered_member_paths(root: &Path) -> Vec<String> {
    let mut diagnostics = Diagnostics::default();
    discover_member_dirs(root, &mut diagnostics)
        .iter()
        .filter(|dir| dir.as_path() != root)
        .map(|dir| rel_path_string(root, dir))
        .collect()
}

/// The member's tag list: `default`, unless an override glob claims it.
///
/// Globs resolving to *different* lists are ambiguous — there is no sensible
/// precedence between `["exact"]` and `["exact", "major"]` — so that is
/// reported rather than silently resolved. Overlapping globs agreeing on the
/// same list are fine, matching how `resolve_lifecycle` treats its rules.
fn resolve_package_tags(
    overrides: &[(Vec<TagLevel>, globset::GlobMatcher)],
    rel_path: &str,
    default: &[TagLevel],
    diagnostics: &mut Diagnostics,
) -> Vec<TagLevel> {
    let mut matched: Vec<&Vec<TagLevel>> = overrides
        .iter()
        .filter(|(_, glob)| glob.is_match(rel_path))
        .map(|(levels, _)| levels)
        .collect();
    matched.dedup_by(|a, b| a == b);
    match matched.as_slice() {
        [] => default.to_vec(),
        [levels] => (*levels).clone(),
        lists => {
            diagnostics.push(
                Finding::error(
                    Check::WorkspaceConfig,
                    format!(
                        "member `{rel_path}` matches `package_tags_overrides` globs resolving \
                         to {}; a member may have only one tag list",
                        lists
                            .iter()
                            .map(|levels| format!(
                                "[{}]",
                                levels
                                    .iter()
                                    .map(|level| format!("`{}`", level.key()))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                            .collect::<Vec<_>>()
                            .join(" and ")
                    ),
                )
                .at(GLEAM_TOML),
            );
            default.to_vec()
        }
    }
}

/// The member's resolved release lifecycle:
///
/// 1. Start from `publish.lifecycle.default`.
/// 2. Apply the legacy `exclude.@release` mapping to `workspace`, when matched.
/// 3. Apply an explicit `publish.lifecycle.packages` rule, when matched —
///    this takes precedence over both of the above.
///
/// A member matched by explicit rules resolving to more than one distinct
/// lifecycle is a deterministic error (falls back to the default); matching
/// several rules that agree on the same lifecycle is fine — that's how a
/// directory and a narrower glob inside it can both claim a member.
fn resolve_lifecycle(
    overrides: &[(ReleaseLifecycle, globset::GlobMatcher)],
    release_excludes: Option<&globset::GlobSet>,
    rel_path: &str,
    default: ReleaseLifecycle,
    diagnostics: &mut Diagnostics,
) -> ReleaseLifecycle {
    let matched: Vec<(ReleaseLifecycle, &str)> = overrides
        .iter()
        .filter(|(_, matcher)| matcher.is_match(rel_path))
        .map(|(lifecycle, matcher)| (*lifecycle, matcher.glob().glob()))
        .collect();
    match matched.split_first() {
        None => {
            if release_excludes.is_some_and(|set| set.is_match(rel_path)) {
                ReleaseLifecycle::Workspace
            } else {
                default
            }
        }
        Some((&(first, _), rest)) if rest.iter().all(|&(lifecycle, _)| lifecycle == first) => first,
        Some(_) => {
            diagnostics.push(
                Finding::error(
                    Check::WorkspaceConfig,
                    format!(
                        "member `{rel_path}` matches `publish.lifecycle.packages` globs for \
                         conflicting lifecycles: {}",
                        matched
                            .iter()
                            .map(|(lifecycle, pattern)| format!(
                                "`{pattern}` => `{}`",
                                lifecycle.key()
                            ))
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                )
                .at(GLEAM_TOML),
            );
            default
        }
    }
}

fn build_globset(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(globset::Glob::new(pattern)?);
    }
    Ok(builder.build()?)
}

/// Lexically normalize a path (resolve `.` and `..`) without touching the
/// filesystem, so paths to missing directories still compare cleanly.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    result.push(Component::ParentDir);
                }
            }
            other => result.push(other),
        }
    }
    result
}

fn rel_path_string(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let joined = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    // The root itself can be a member (a single-package repo under
    // auto-discovery); "." keeps `{rel_path}/...` displays working.
    if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn toposort_orders_dependencies_first() {
        // cli -> mid -> core (edges are dependency -> dependent)
        let names = names(&["cli", "core", "mid"]);
        let order = toposort(3, &names, &[(1, 2), (2, 0)]).unwrap();
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn toposort_breaks_ties_alphabetically() {
        let names = names(&["zebra", "apple", "mango"]);
        let order = toposort(3, &names, &[]).unwrap();
        assert_eq!(order, vec![1, 2, 0]);
    }

    #[test]
    fn toposort_reports_a_cycle() {
        let names = names(&["a", "b", "c"]);
        let cycle = toposort(3, &names, &[(0, 1), (1, 2), (2, 0)]).unwrap_err();
        assert_eq!(cycle.first(), cycle.last());
        assert!(
            cycle.len() == 4,
            "cycle should name all three members: {cycle:?}"
        );
    }

    fn globs(patterns: &[&str]) -> globset::GlobSet {
        build_globset(&names(patterns)).unwrap()
    }

    fn tag_overrides(
        entries: &[(&str, &[TagLevel])],
    ) -> Vec<(Vec<TagLevel>, globset::GlobMatcher)> {
        entries
            .iter()
            .map(|(pattern, levels)| {
                (
                    levels.to_vec(),
                    globset::Glob::new(pattern).unwrap().compile_matcher(),
                )
            })
            .collect()
    }

    #[test]
    fn package_tags_fall_back_to_the_workspace_default() {
        let mut diagnostics = Diagnostics::default();
        let both: &[TagLevel] = &[TagLevel::Exact, TagLevel::Minor];
        let overrides = tag_overrides(&[("packages/lat_*", both)]);
        let default = [TagLevel::Exact];
        assert_eq!(
            resolve_package_tags(&overrides, "packages/cli", &default, &mut diagnostics),
            default
        );
        assert_eq!(
            resolve_package_tags(&overrides, "packages/lat_core", &default, &mut diagnostics),
            both
        );
        assert!(!diagnostics.has_errors());
    }

    /// Overlapping globs are only a problem when they disagree — the same rule
    /// `resolve_lifecycle` applies.
    #[test]
    fn overlapping_tag_overrides_agreeing_on_one_list_are_fine() {
        let mut diagnostics = Diagnostics::default();
        let minor: &[TagLevel] = &[TagLevel::Minor];
        let overrides = tag_overrides(&[("packages/*", minor), ("packages/lat_*", minor)]);
        assert_eq!(
            resolve_package_tags(
                &overrides,
                "packages/lat_core",
                &[TagLevel::Exact],
                &mut diagnostics
            ),
            minor
        );
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn a_member_matching_two_different_tag_lists_is_an_error() {
        let mut diagnostics = Diagnostics::default();
        let overrides = tag_overrides(&[
            ("packages/*", &[TagLevel::Minor]),
            ("packages/lat_*", &[TagLevel::Exact, TagLevel::Minor]),
        ]);
        let default = [TagLevel::Exact];
        let tags =
            resolve_package_tags(&overrides, "packages/lat_core", &default, &mut diagnostics);
        assert_eq!(tags, default, "falls back to the default");
        let errors: Vec<&str> = diagnostics.errors().collect();
        assert_eq!(errors.len(), 1);
        let error = errors[0];
        assert!(error.contains("packages/lat_core"), "{error}");
        assert!(
            error.contains("`minor`") && error.contains("`exact`"),
            "{error}"
        );
    }

    fn lifecycle_overrides(
        entries: &[(&str, ReleaseLifecycle)],
    ) -> Vec<(ReleaseLifecycle, globset::GlobMatcher)> {
        entries
            .iter()
            .map(|(pattern, lifecycle)| {
                (
                    *lifecycle,
                    globset::Glob::new(pattern).unwrap().compile_matcher(),
                )
            })
            .collect()
    }

    #[test]
    fn lifecycle_falls_back_to_the_workspace_default_with_no_matches() {
        let mut diagnostics = Diagnostics::default();
        let lifecycle = resolve_lifecycle(
            &[],
            None,
            "packages/core",
            ReleaseLifecycle::Hex,
            &mut diagnostics,
        );
        assert_eq!(lifecycle, ReleaseLifecycle::Hex);
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn legacy_release_exclude_maps_to_workspace_lifecycle() {
        let mut diagnostics = Diagnostics::default();
        let release_excludes = globs(&["examples/*"]);
        let lifecycle = resolve_lifecycle(
            &[],
            Some(&release_excludes),
            "examples/demo",
            ReleaseLifecycle::Hex,
            &mut diagnostics,
        );
        assert_eq!(lifecycle, ReleaseLifecycle::Workspace);
        // A member the legacy glob doesn't match keeps the default.
        let lifecycle = resolve_lifecycle(
            &[],
            Some(&release_excludes),
            "packages/core",
            ReleaseLifecycle::Hex,
            &mut diagnostics,
        );
        assert_eq!(lifecycle, ReleaseLifecycle::Hex);
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn explicit_lifecycle_rule_overrides_the_legacy_mapping() {
        let mut diagnostics = Diagnostics::default();
        let release_excludes = globs(&["examples/*"]);
        let overrides = lifecycle_overrides(&[("examples/**", ReleaseLifecycle::GitOnly)]);
        let lifecycle = resolve_lifecycle(
            &overrides,
            Some(&release_excludes),
            "examples/demo",
            ReleaseLifecycle::Hex,
            &mut diagnostics,
        );
        // Legacy alone would say `workspace`; the explicit rule wins.
        assert_eq!(lifecycle, ReleaseLifecycle::GitOnly);
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn overlapping_rules_agreeing_on_the_same_lifecycle_are_fine() {
        let mut diagnostics = Diagnostics::default();
        let overrides = lifecycle_overrides(&[
            ("packages/**", ReleaseLifecycle::GitOnly),
            ("packages/providers/**", ReleaseLifecycle::GitOnly),
        ]);
        let lifecycle = resolve_lifecycle(
            &overrides,
            None,
            "packages/providers/aws",
            ReleaseLifecycle::Hex,
            &mut diagnostics,
        );
        assert_eq!(lifecycle, ReleaseLifecycle::GitOnly);
        assert!(!diagnostics.has_errors());
    }

    #[test]
    fn conflicting_explicit_rules_are_a_deterministic_error() {
        let mut diagnostics = Diagnostics::default();
        let overrides = lifecycle_overrides(&[
            ("packages/**", ReleaseLifecycle::GitOnly),
            ("packages/special/**", ReleaseLifecycle::Workspace),
        ]);
        let lifecycle = resolve_lifecycle(
            &overrides,
            None,
            "packages/special/thing",
            ReleaseLifecycle::Hex,
            &mut diagnostics,
        );
        assert_eq!(
            lifecycle,
            ReleaseLifecycle::Hex,
            "falls back to the default"
        );
        let errors: Vec<&str> = diagnostics.errors().collect();
        assert_eq!(errors.len(), 1);
        let error = errors[0];
        assert!(error.contains("packages/special/thing"), "{error}");
        assert!(
            error.contains("`git_only`") && error.contains("`workspace`"),
            "{error}"
        );
    }

    #[test]
    fn normalize_resolves_parent_components() {
        assert_eq!(
            normalize_path(Path::new("/ws/packages/cli/../core")),
            PathBuf::from("/ws/packages/core")
        );
        assert_eq!(
            normalize_path(Path::new("/ws/./examples")),
            PathBuf::from("/ws/examples")
        );
    }
}
