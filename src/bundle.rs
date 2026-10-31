//! Prepare an isolated Hex package from explicitly bundled workspace source.

use crate::gleam::Requirement;
use crate::rewrite;
use crate::workspace::{Member, Workspace};
use anyhow::{Context, Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, value};

/// Publisher index -> bundled member indices, in workspace topological order.
pub fn resolve(workspace: &Workspace) -> Result<BTreeMap<usize, Vec<usize>>> {
    let mut bundles = BTreeMap::new();
    for (name, roots) in &workspace.config.publish.bundle {
        let publisher = workspace
            .member_index(name)
            .with_context(|| format!("publish.bundle: unknown publisher `{name}`"))?;
        ensure!(
            workspace.members[publisher].publishes_to_hex(),
            "publish.bundle: publisher `{name}` must have lifecycle `hex`"
        );
        ensure!(!roots.is_empty(), "publish.bundle.{name} must not be empty");
        let mut selected = BTreeSet::new();
        let mut pending = Vec::new();
        for root in roots {
            ensure!(
                selected.insert(root),
                "publish.bundle.{name}: duplicate dependency `{root}`"
            );
            let dep = workspace
                .member_index(root)
                .with_context(|| format!("publish.bundle.{name}: unknown dependency `{root}`"))?;
            ensure!(
                workspace.runtime_deps_of(publisher).contains(&dep),
                "publish.bundle.{name}: `{root}` is not a direct runtime path dependency"
            );
            ensure!(
                !workspace.members[dep].publishes_to_hex(),
                "publish.bundle.{name}: `{root}` has lifecycle `hex`; keep it as a dependency"
            );
            pending.push(dep);
        }
        for &idx in workspace.runtime_deps_of(publisher) {
            let dep = &workspace.members[idx];
            ensure!(
                dep.publishes_to_hex() || selected.contains(&dep.name),
                "publish.bundle.{name}: direct non-Hex dependency `{}` must be listed",
                dep.name
            );
        }
        let mut closure = BTreeSet::new();
        while let Some(dep) = pending.pop() {
            if closure.insert(dep) {
                pending.extend(
                    workspace
                        .runtime_deps_of(dep)
                        .iter()
                        .copied()
                        .filter(|&idx| !workspace.members[idx].publishes_to_hex()),
                );
            }
        }
        bundles.insert(publisher, closure.into_iter().collect());
    }
    Ok(bundles)
}

pub struct Prepared {
    pub manifest: String,
    pub packages: Vec<String>,
    pub requirements: BTreeMap<String, String>,
    files: BTreeMap<PathBuf, PathBuf>,
    notices: BTreeMap<PathBuf, String>,
    name: String,
}

pub struct Staged {
    _directory: tempfile::TempDir,
    pub path: PathBuf,
}

impl Prepared {
    pub fn stage(&self) -> Result<Staged> {
        let directory = tempfile::tempdir().context("failed to create bundle staging directory")?;
        let path = directory.path().join(&self.name);
        std::fs::create_dir(&path)?;
        for (relative, source) in &self.files {
            let destination = path.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap())?;
            std::fs::copy(source, &destination).with_context(|| {
                format!(
                    "failed to stage {} as {}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
        for (relative, contents) in &self.notices {
            std::fs::write(path.join(relative), contents)?;
        }
        std::fs::write(path.join("gleam.toml"), &self.manifest)?;
        Ok(Staged {
            _directory: directory,
            path,
        })
    }
}

pub fn prepare(workspace: &Workspace, publisher: usize, bundled: &[usize]) -> Result<Prepared> {
    let host = &workspace.members[publisher];
    ensure!(
        Path::new(&host.name).components().count() == 1
            && matches!(
                Path::new(&host.name).components().next(),
                Some(std::path::Component::Normal(_))
            ),
        "invalid publishing package name `{}`",
        host.name
    );
    let mut doc = manifest(host)?;
    let mut result = Prepared {
        manifest: String::new(),
        packages: bundled
            .iter()
            .map(|&i| workspace.members[i].name.clone())
            .collect(),
        requirements: BTreeMap::new(),
        files: BTreeMap::new(),
        notices: BTreeMap::new(),
        name: host.name.clone(),
    };
    result.packages.sort();
    let mut sources = vec![publisher];
    sources.extend_from_slice(bundled);
    let mut owners = BTreeMap::new();
    let mut patterns = Vec::new();
    let mut modules = Vec::new();
    let mut compiler: Option<(String, String)> = None;
    let mut applications = BTreeSet::new();

    collect_host_files(workspace, host, &mut result.files)?;
    for &idx in &sources {
        let member = &workspace.members[idx];
        let source_doc = manifest(member)?;
        let own_patterns = internal_patterns(&source_doc, &member.name)?;
        patterns.extend(own_patterns.iter().cloned());
        if let Some(constraint) = source_doc.get("gleam") {
            let constraint = constraint.as_str().context("`gleam` must be a string")?;
            if let Some((existing, origin)) = &compiler {
                ensure!(
                    existing == constraint,
                    "bundle compiler requirement conflict between `{origin}` ({existing}) and `{}` ({constraint})",
                    member.name
                );
            } else {
                compiler = Some((constraint.into(), member.name.clone()));
            }
        }
        if let Some(extra) = source_doc
            .get("erlang")
            .and_then(|e| e.get("extra_applications"))
        {
            for app in extra
                .as_array()
                .context("extra_applications must be an array")?
            {
                let app = app
                    .as_str()
                    .context("extra_applications must contain strings")?;
                ensure!(
                    !result.packages.iter().any(|name| name == app),
                    "bundle `{}` requires removed OTP application `{app}`",
                    member.name
                );
                applications.insert(app.to_string());
            }
        }
        if idx != publisher {
            ensure!(
                source_doc
                    .get("erlang")
                    .and_then(|e| e.get("application_start_module"))
                    .is_none(),
                "cannot bundle `{}`: OTP application_start_module requires a separate application",
                member.name
            );
            ensure!(
                !member.path.join("priv").exists(),
                "cannot bundle `{}`: priv resources require a separate package identity",
                member.name
            );
            let mut files = BTreeMap::new();
            collect_files(&member.path, &member.path.join("src"), &[], &mut files)?;
            for (relative, source) in files {
                ensure!(
                    !result.files.contains_key(&relative),
                    "bundle file conflict at {} between {} and {}",
                    relative.display(),
                    result.files.get(&relative).unwrap().display(),
                    source.display()
                );
                result.files.insert(relative, source);
            }
            let notices = licence_notices(member, &source_doc)?;
            if !notices.is_empty() {
                // Gleam exports standard licence filenames, not arbitrary prefixed files.
                let destination = result
                    .files
                    .keys()
                    .find(|path| path.to_string_lossy().eq_ignore_ascii_case("LICENCE"))
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from("LICENCE"));
                ensure!(
                    !result.files.keys().any(|path| path
                        .to_string_lossy()
                        .to_lowercase()
                        .starts_with(&format!(
                            "{}/",
                            destination.to_string_lossy().to_lowercase()
                        ))),
                    "bundle licence file/directory conflict at {}",
                    destination.display()
                );
                if !result.notices.contains_key(&destination) {
                    let original = result
                        .files
                        .get(&destination)
                        .map(std::fs::read_to_string)
                        .transpose()?
                        .unwrap_or_default();
                    result.notices.insert(destination.clone(), original);
                }
                let combined = result.notices.get_mut(&destination).unwrap();
                combined.push('\n');
                combined.push_str(&notices);
            }
        }
        for dep in member.manifest.dependencies.iter().filter(|d| !d.dev) {
            if let Some(declaration) = source_doc
                .get("dependencies")
                .and_then(|d| d.get(&dep.name))
                && let Some(table) = declaration.as_table_like()
            {
                ensure!(
                    table
                        .iter()
                        .all(|(key, _)| matches!(key, "version" | "path" | "git" | "ref")),
                    "cannot bundle `{}`: unsupported dependency settings for `{}`",
                    member.name,
                    dep.name
                );
            }
            if result.packages.contains(&dep.name) {
                ensure!(
                    matches!(dep.requirement, Requirement::Path(_)),
                    "bundle dependency conflict: `{}` references bundled `{}` from a non-path source",
                    member.name,
                    dep.name
                );
                continue;
            }
            let requirement = runtime_requirement(workspace, member, dep)?;
            if let Some((existing, origin)) = owners.get(&dep.name) {
                ensure!(
                    existing == &requirement,
                    "bundle dependency conflict for `{}`: `{origin}` requires `{existing}`, `{}` requires `{requirement}`",
                    dep.name,
                    member.name
                );
            } else {
                owners.insert(dep.name.clone(), (requirement.clone(), member.name.clone()));
            }
            result.requirements.insert(dep.name.clone(), requirement);
        }
        let src = member.path.join("src");
        for (relative, source) in &result.files {
            if relative.extension().is_some_and(|ext| ext == "gleam") && source.starts_with(&src) {
                let name = relative.strip_prefix("src")?.with_extension("");
                let name = name.to_string_lossy().replace('\\', "/");
                let internal = matches_patterns(&own_patterns, &name)?;
                modules.push((name, internal, member.name.clone()));
            }
        }
    }
    patterns.sort();
    patterns.dedup();
    for (module, internal, owner) in modules {
        ensure!(
            internal == matches_patterns(&patterns, &module)?,
            "bundle internal_modules conflict: public module `{module}` from `{owner}` would become internal"
        );
    }
    check_file_conflicts(&result.files)?;
    let mut array = Array::new();
    for pattern in patterns {
        array.push(pattern);
    }
    doc["internal_modules"] = value(array);
    if let Some((constraint, _)) = compiler {
        doc["gleam"] = value(constraint);
    }
    if !applications.is_empty() {
        if doc.get("erlang").is_none() {
            doc["erlang"] = Item::Table(Table::new());
        }
        let mut array = Array::new();
        for app in applications {
            array.push(app);
        }
        doc["erlang"]["extra_applications"] = value(array);
    }
    let mut runtime = Table::new();
    for (name, requirement) in &result.requirements {
        runtime.insert(name, value(requirement));
    }
    doc["dependencies"] = Item::Table(runtime);
    if let Some(dev) = doc
        .get_mut("dev-dependencies")
        .and_then(Item::as_table_like_mut)
    {
        for name in &result.packages {
            dev.remove(name);
        }
        for dep in host.manifest.dependencies.iter().filter(|d| d.dev) {
            if result.packages.contains(&dep.name) {
                continue;
            }
            if let Some(runtime) = result.requirements.get(&dep.name) {
                ensure!(
                    runtime == &runtime_requirement(workspace, host, dep)?,
                    "bundle dependency conflict: dev dependency `{}` differs from its runtime requirement",
                    dep.name
                );
                dev.remove(&dep.name);
            } else if let Requirement::Path(path) = &dep.requirement {
                let idx = workspace
                    .member_index(&dep.name)
                    .context("unknown workspace dependency")?;
                if workspace.members[idx].publishes_to_hex() {
                    dev.insert(&dep.name, value(runtime_requirement(workspace, host, dep)?));
                } else {
                    let absolute = host.path.join(path);
                    let mut table = toml_edit::InlineTable::new();
                    table.insert("path", absolute.to_string_lossy().as_ref().into());
                    dev.insert(&dep.name, value(table));
                }
            }
        }
    }
    result.manifest = doc.to_string();
    Ok(result)
}

fn manifest(member: &Member) -> Result<DocumentMut> {
    let path = member.path.join("gleam.toml");
    ensure!(
        !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
        "cannot bundle symlink {}",
        path.display()
    );
    std::fs::read_to_string(&path)?
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))
}

fn runtime_requirement(
    workspace: &Workspace,
    member: &Member,
    dep: &crate::gleam::Dependency,
) -> Result<String> {
    match &dep.requirement {
        Requirement::Hex(requirement) => Ok(requirement.clone()),
        Requirement::Path(_) => {
            let idx = workspace
                .member_index(&dep.name)
                .context("unknown path dependency")?;
            let target = &workspace.members[idx];
            ensure!(
                target.publishes_to_hex(),
                "bundle `{}`: non-Hex runtime dependency `{}` is not selected in publish.bundle",
                member.name,
                dep.name
            );
            rewrite::hex_requirement(
                target.version(),
                workspace.config.publish.path_dep_requirement,
            )
        }
        Requirement::Git(_) => bail!(
            "cannot bundle `{}`: runtime git dependency `{}` cannot be published to Hex",
            member.name,
            dep.name
        ),
    }
}

fn internal_patterns(doc: &DocumentMut, name: &str) -> Result<Vec<String>> {
    match doc.get("internal_modules") {
        Some(item) => item
            .as_array()
            .context("internal_modules must be an array")?
            .iter()
            .map(|pattern| {
                Ok(pattern
                    .as_str()
                    .context("internal_modules must contain strings")?
                    .to_owned())
            })
            .collect(),
        None => Ok(vec![
            format!("{name}/internal"),
            format!("{name}/internal/*"),
        ]),
    }
}

fn matches_patterns(patterns: &[String], name: &str) -> Result<bool> {
    for pattern in patterns {
        if glob::Pattern::new(pattern)
            .with_context(|| format!("invalid internal_modules pattern `{pattern}`"))?
            .matches(name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn collect_host_files(
    workspace: &Workspace,
    host: &Member,
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    let nested: Vec<_> = workspace
        .members
        .iter()
        .filter(|m| m.path != host.path && m.path.starts_with(&host.path))
        .map(|m| m.path.clone())
        .collect();
    collect_files(&host.path, &host.path, &nested, files)
}

fn collect_files(
    root: &Path,
    current: &Path,
    excluded: &[PathBuf],
    files: &mut BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(current) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", current.display()));
        }
    };
    ensure!(
        !metadata.file_type().is_symlink(),
        "cannot bundle symlink {}",
        current.display()
    );
    if excluded.iter().any(|path| path == current) {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(current)? {
            let path = entry?.path();
            let name = path.file_name().unwrap().to_string_lossy();
            let at_root = current == root;
            if name == ".git"
                || (at_root
                    && (name.starts_with('.')
                        || matches!(
                            name.as_ref(),
                            "build" | "node_modules" | "manifest.toml" | "gleam.toml"
                        )))
            {
                continue;
            }
            collect_files(root, &path, excluded, files)?;
        }
    } else {
        ensure!(
            metadata.is_file(),
            "unsupported bundle input {}",
            current.display()
        );
        files.insert(
            current.strip_prefix(root)?.to_path_buf(),
            current.to_path_buf(),
        );
    }
    Ok(())
}

fn licence_notices(member: &Member, doc: &DocumentMut) -> Result<String> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(&member.path)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_uppercase();
        if ["LICENSE", "LICENCE", "NOTICE", "COPYING"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            collect_files(&member.path, &path, &[], &mut files)?;
        }
    }
    let mut notices = format!("Bundled package: {}\n", member.name);
    if let Some(licences) = doc.get("licences") {
        notices.push_str(&format!(
            "Declared licences: {}\n",
            licences.to_string().trim()
        ));
    }
    for (relative, source) in &files {
        notices.push_str(&format!("\n--- {} ---\n", relative.display()));
        notices.push_str(
            &std::fs::read_to_string(source)
                .with_context(|| format!("failed to read licence notice {}", source.display()))?,
        );
        notices.push('\n');
    }
    Ok(notices)
}

fn check_file_conflicts(files: &BTreeMap<PathBuf, PathBuf>) -> Result<()> {
    let mut names = BTreeMap::new();
    let folded: BTreeMap<_, _> = files
        .iter()
        .map(|(relative, source)| (relative.to_string_lossy().to_lowercase(), source))
        .collect();
    for (relative, source) in files {
        let path = relative.to_string_lossy().replace('\\', "/");
        let mut keys = vec![format!("file:{}", path.to_lowercase())];
        if let Some(ext) = relative.extension().and_then(|ext| ext.to_str())
            && let Some(module) = path
                .strip_prefix("src/")
                .or_else(|| path.strip_prefix("test/"))
        {
            let module = module.rsplit_once('.').unwrap().0;
            match ext {
                "gleam" => {
                    keys.push(format!(
                        "erlang:{}",
                        module.replace('/', "@").to_lowercase()
                    ));
                    keys.push(format!("javascript:{}.mjs", module.to_lowercase()));
                }
                "erl" => keys.push(format!(
                    "erlang:{}",
                    relative
                        .file_stem()
                        .unwrap()
                        .to_string_lossy()
                        .to_lowercase()
                )),
                "mjs" => keys.push(format!("javascript:{}.mjs", module.to_lowercase())),
                _ => {}
            }
        }
        for key in keys {
            if let Some(other) = names.insert(key, source) {
                bail!(
                    "bundle module/file conflict between {} and {}",
                    other.display(),
                    source.display()
                );
            }
        }
        for parent in relative
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            if let Some(other) = folded.get(&parent.to_string_lossy().to_lowercase()) {
                bail!(
                    "bundle file/directory conflict between {} and {}",
                    other.display(),
                    source.display()
                );
            }
        }
    }
    Ok(())
}
