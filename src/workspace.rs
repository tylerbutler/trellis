//! The workspace model: member discovery, `gleam.toml` parsing, and the
//! dependency graph. Every command starts here; the topological order is
//! computed once and consumed everywhere.

use crate::config::ConfigFile;
use crate::gleam::GleamManifest;
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
    /// False when the member matches an `@release` exclusion glob.
    pub releasable: bool,
}

impl Member {
    pub fn version(&self) -> &str {
        &self.manifest.version
    }
}

#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub config: ConfigFile,
    /// Members in topological order (dependencies before dependents).
    pub members: Vec<Member>,
    /// Direct workspace dependencies, indexed like `members`.
    deps: Vec<Vec<usize>>,
    /// Direct workspace dependents, indexed like `members`.
    dependents: Vec<Vec<usize>>,
}

/// Problems collected while loading. `Workspace::load` turns any error into a
/// failure; `trellis doctor` reports them all instead.
#[derive(Debug, Default)]
pub struct Diagnostics {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Diagnostics {
    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(message.into());
    }
    fn warning(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

impl Workspace {
    /// Walk up from `start` looking for a `gleam.toml` with a
    /// `[tools.trellis]` table — the workspace root marker. Member manifests
    /// (gleam.toml without the table) are skipped, so commands work from
    /// inside a package, like `git` or `cargo`.
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
        let mut message = format!(
            "no {GLEAM_TOML} with a [tools.trellis] table found in {} or any parent directory",
            start.display()
        );
        if !unparseable.is_empty() {
            message.push_str(&format!(
                " (note: {} could not be parsed and may be the missing workspace root)",
                unparseable
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        bail!(message)
    }

    /// Strict load: any diagnostic error is fatal.
    pub fn load(start: &Path) -> Result<Self> {
        let root = Self::find_root(start)?;
        let (workspace, diagnostics) = Self::load_with_diagnostics(&root)?;
        if !diagnostics.errors.is_empty() {
            bail!(
                "workspace is invalid:\n  - {}\n(run `trellis doctor` for details)",
                diagnostics.errors.join("\n  - ")
            );
        }
        workspace.context("workspace could not be loaded")
    }

    /// Lenient load for `doctor`: collects every problem it can find and
    /// returns a best-effort model. The workspace is `None` only when no
    /// coherent model exists (unreadable config or a dependency cycle).
    pub fn load_with_diagnostics(root: &Path) -> Result<(Option<Self>, Diagnostics)> {
        let mut diagnostics = Diagnostics::default();
        let config = match ConfigFile::load(&root.join(GLEAM_TOML)) {
            Ok(config) => config,
            Err(err) => {
                diagnostics.error(format!("{err:#}"));
                return Ok((None, diagnostics));
            }
        };

        let member_dirs = expand_member_globs(root, &config.members, &mut diagnostics);

        // Parse each member manifest; unparseable members are reported and dropped.
        for (task, patterns) in &config.exclude {
            if let Err(err) = build_globset(patterns) {
                diagnostics.error(format!("invalid `{task}` exclusion glob: {err:#}"));
            }
        }

        let release_exclusions = config
            .exclude
            .get(crate::config::RELEASE_EXCLUDE_KEY)
            .cloned()
            .unwrap_or_default();
        let release_excludes = build_globset(&release_exclusions)
            .map_err(|err| {
                diagnostics.error(format!("invalid release exclusion glob: {err:#}"));
            })
            .ok();
        let mut members = Vec::new();
        for dir in member_dirs {
            let rel_path = rel_path_string(root, &dir);
            let manifest_path = dir.join("gleam.toml");
            if !manifest_path.is_file() {
                diagnostics.error(format!("member `{rel_path}` has no gleam.toml"));
                continue;
            }
            match GleamManifest::load(&manifest_path) {
                Ok(manifest) => {
                    // A member manifest with its own [tools.trellis] would
                    // hijack root discovery for commands run inside it.
                    if manifest.has_trellis_config && dir != root {
                        diagnostics.error(format!(
                            "member `{rel_path}` has a [tools.trellis] table; only the workspace \
                             root's gleam.toml may have one"
                        ));
                    }
                    let releasable = release_excludes
                        .as_ref()
                        .map(|set| !set.is_match(&rel_path))
                        .unwrap_or(true);
                    members.push(Member {
                        name: manifest.name.clone(),
                        path: dir,
                        rel_path,
                        manifest,
                        releasable,
                    });
                }
                Err(err) => diagnostics.error(format!("{err:#}")),
            }
        }

        // Duplicate names would make every name-keyed operation ambiguous.
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for member in &members {
            if let Some(other) = seen.insert(&member.name, &member.rel_path) {
                diagnostics.error(format!(
                    "duplicate package name `{}` in `{}` and `{}`",
                    member.name, other, member.rel_path
                ));
            }
        }

        // Resolve path dependencies between members into graph edges.
        let path_to_idx: HashMap<PathBuf, usize> = members
            .iter()
            .enumerate()
            .map(|(idx, member)| (member.path.clone(), idx))
            .collect();
        let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (idx, member) in members.iter().enumerate() {
            for (dep_name, dep_path, _dev) in member.manifest.path_deps() {
                let resolved = normalize_path(&member.path.join(dep_path));
                if !resolved.starts_with(root) {
                    diagnostics.error(format!(
                        "package `{}`: path dependency `{dep_name}` ({dep_path}) points outside the workspace",
                        member.name
                    ));
                    continue;
                }
                match path_to_idx.get(&resolved) {
                    Some(&dep_idx) => {
                        if members[dep_idx].name != dep_name {
                            diagnostics.error(format!(
                                "package `{}`: path dependency `{dep_name}` resolves to `{}`, which is named `{}`",
                                member.name, members[dep_idx].rel_path, members[dep_idx].name
                            ));
                        }
                        if dep_idx == idx {
                            diagnostics.error(format!(
                                "package `{}` path-depends on itself",
                                member.name
                            ));
                        } else {
                            edges.insert((dep_idx, idx)); // dependency -> dependent
                        }
                    }
                    None => diagnostics.error(format!(
                        "package `{}`: path dependency `{dep_name}` ({dep_path}) is not a workspace member",
                        member.name
                    )),
                }
            }
        }

        let names: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
        let edge_list: Vec<(usize, usize)> = edges.iter().copied().collect();
        let order = match toposort(members.len(), &names, &edge_list) {
            Ok(order) => order,
            Err(cycle) => {
                diagnostics.error(format!(
                    "dependency cycle between workspace members: {}",
                    cycle.join(" -> ")
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
        for list in deps.iter_mut().chain(dependents.iter_mut()) {
            list.sort_unstable();
        }

        let workspace = Workspace {
            root: root.to_path_buf(),
            config,
            members,
            deps,
            dependents,
        };
        Ok((Some(workspace), diagnostics))
    }

    pub fn member_index(&self, name: &str) -> Option<usize> {
        self.members.iter().position(|m| m.name == name)
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
            selected.retain(|&idx| self.members[idx].releasable);
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
            diagnostics.error(format!("member glob `{pattern}` is not valid UTF-8"));
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
                Err(err) => diagnostics.error(format!("invalid member glob `{pattern}`: {err}")),
            }
            continue;
        }

        let path = Path::new(full);
        if path.is_dir() {
            dirs.insert(normalize_path(path));
        } else {
            diagnostics.error(format!("member glob `{pattern}` matches no packages"));
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
                    diagnostics.warning(format!("while expanding member globs: {err}"));
                }
            }
        }
    }

    for (pattern, _, matched) in wildcard_patterns {
        if matched == 0 {
            diagnostics.error(format!("member glob `{pattern}` matches no packages"));
        }
    }

    dirs.into_iter().collect()
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
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
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
