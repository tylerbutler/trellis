//! `trellis new <name>` — scaffold a workspace member. The gleam.toml is
//! pre-filled from workspace metadata (gleam constraint, licences,
//! repository copied from a sibling) and a stub module and test are created.
//! Nothing needs registering anywhere: membership, the graph, and the
//! changelog engine all derive from the files written here.

use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;
use toml_edit::{DocumentMut, Item, value};

pub struct NewOptions {
    pub name: String,
    pub template: String,
    /// Parent directory relative to the workspace root; derived from the
    /// existing members when omitted.
    pub path: Option<String>,
}

pub fn run(workspace: &Workspace, options: &NewOptions) -> Result<()> {
    if options.template != "lib" {
        bail!("unknown template `{}` (available: lib)", options.template);
    }
    let name = options.name.as_str();
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        bail!("`{name}` is not a valid gleam package name (lowercase letters, digits, and _)");
    }
    if workspace.member_index(name).is_some() {
        bail!("a package named `{name}` already exists in the workspace");
    }

    let parent = match &options.path {
        Some(path) => {
            let path = Path::new(path);
            if path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                bail!("--path must be a relative path inside the workspace");
            }
            path.components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(part) => Some(part.to_string_lossy()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/")
        }
        None => default_parent(workspace).context(
            "cannot derive a package directory from the current members; pass --path <dir>",
        )?,
    };
    let rel_path = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    };

    // A directory that discovery won't pick up would be silently invisible to
    // every other command — exactly the drift trellis exists to prevent. With
    // explicit `members`, a glob must match; with auto-discovery, any
    // gleam.toml is found, so only an `@members` exclusion can hide it.
    if let Some(member_globs) = &workspace.config.members {
        let matched = member_globs.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .map(|p| {
                    p.matches_with(
                        &rel_path,
                        glob::MatchOptions {
                            require_literal_separator: true,
                            ..Default::default()
                        },
                    )
                })
                .unwrap_or(false)
        });
        if !matched {
            bail!(
                "`{rel_path}` does not match any members glob in [tools.trellis] ({}); \
                 pass a different --path or add a glob first",
                member_globs.join(", ")
            );
        }
    } else if let Some(patterns) = workspace
        .config
        .exclude
        .get(crate::config::MEMBERS_EXCLUDE_KEY)
    {
        let excluded = patterns.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .map(|p| p.matches(&rel_path))
                .unwrap_or(false)
        });
        if excluded {
            bail!(
                "`{rel_path}` matches an `{}` exclusion glob, so the new package would be \
                 invisible to trellis; pass a different --path or adjust the exclusion",
                crate::config::MEMBERS_EXCLUDE_KEY
            );
        }
    }

    let dir = crate::workspace::normalize_path(&workspace.root.join(&rel_path));
    validate_destination(&workspace.root, &dir)?;
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }

    let sibling = pick_sibling(workspace, &parent);
    let manifest = render_manifest(name, sibling)?;

    write(&dir.join("gleam.toml"), &manifest)?;
    write(
        &dir.join("src").join(format!("{name}.gleam")),
        &format!("pub fn hello() -> String {{\n  \"Hello from {name}!\"\n}}\n"),
    )?;
    write(
        &dir.join("test").join(format!("{name}_test.gleam")),
        &format!(
            "import gleeunit\nimport {name}\n\npub fn main() -> Nil {{\n  gleeunit.main()\n}}\n\npub fn hello_test() {{\n  assert {name}.hello() == \"Hello from {name}!\"\n}}\n"
        ),
    )?;
    let header = crate::changelog::render_header(&workspace.config.changelog, name)?;
    write(
        &dir.join("CHANGELOG.md"),
        &format!("{}\n", header.trim_end()),
    )?;
    write(&dir.join("README.md"), &format!("# {name}\n"))?;

    println!("created {rel_path}/ (gleam.toml, src, test, CHANGELOG.md, README.md)");
    if let Some(sibling) = sibling {
        println!("metadata copied from {}", sibling.rel_path);
    }
    // Nothing to register anywhere else: membership, the graph, and the
    // changelog engine are all derived from what was just written.
    Ok(())
}

/// The most common parent directory among current members — "packages" in a
/// `packages/*` workspace. Ties break alphabetically for determinism.
fn default_parent(workspace: &Workspace) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for member in &workspace.members {
        let parent = match member.rel_path.rsplit_once('/') {
            Some((parent, _)) => parent.to_string(),
            None => String::new(),
        };
        *counts.entry(parent).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(parent, _)| parent)
}

/// The member to copy shared metadata from: prefer a releasable sibling in
/// the same parent directory, then any releasable member, then any member.
fn pick_sibling<'a>(
    workspace: &'a Workspace,
    parent: &str,
) -> Option<&'a crate::workspace::Member> {
    let in_parent = |member: &&crate::workspace::Member| {
        member
            .rel_path
            .rsplit_once('/')
            .map(|(p, _)| p)
            .unwrap_or("")
            == parent
    };
    workspace
        .members
        .iter()
        .filter(|m| m.releasable)
        .find(in_parent)
        .or_else(|| workspace.members.iter().find(|m| m.releasable))
        .or_else(|| workspace.members.first())
}

/// Build the new gleam.toml, copying `gleam`, `licences`, and `repository`
/// plus the sibling's gleam_stdlib/gleeunit requirements so the new package
/// matches its neighbors.
fn render_manifest(name: &str, sibling: Option<&crate::workspace::Member>) -> Result<String> {
    let sibling_doc: Option<DocumentMut> = match sibling {
        Some(member) => {
            let text = std::fs::read_to_string(member.path.join("gleam.toml"))
                .with_context(|| format!("failed to read {}/gleam.toml", member.rel_path))?;
            Some(text.parse().context("failed to parse sibling gleam.toml")?)
        }
        None => None,
    };

    let mut doc = DocumentMut::new();
    doc["name"] = value(name);
    doc["version"] = value("0.1.0");
    for key in ["gleam", "licences", "repository"] {
        if let Some(item) = sibling_doc.as_ref().and_then(|d| d.get(key)) {
            doc[key] = item.clone();
        }
    }

    let copy_req = |section: &str, dep: &str| -> Option<Item> {
        sibling_doc
            .as_ref()
            .and_then(|d| d.get(section))
            .and_then(|s| s.get(dep))
            .filter(|item| item.as_str().is_some()) // only Hex requirements
            .cloned()
    };
    let mut deps = toml_edit::Table::new();
    if let Some(req) = copy_req("dependencies", "gleam_stdlib") {
        deps.insert("gleam_stdlib", req);
    }
    doc["dependencies"] = Item::Table(deps);
    let mut dev_deps = toml_edit::Table::new();
    dev_deps.insert(
        "gleeunit",
        copy_req("dev-dependencies", "gleeunit").unwrap_or_else(|| value(">= 1.0.0 and < 2.0.0")),
    );
    doc["dev-dependencies"] = Item::Table(dev_deps);

    Ok(doc.to_string())
}

fn validate_destination(root: &Path, destination: &Path) -> Result<()> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("--path must be a relative path inside the workspace"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "--path resolves through symbolic link {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn write(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn destination_rejects_symlink_parent() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("packages")).unwrap();

        let err = validate_destination(root.path(), &root.path().join("packages/new")).unwrap_err();
        assert!(err.to_string().contains("symbolic link"));
    }

    #[test]
    fn manifest_without_sibling_still_valid() {
        let manifest = render_manifest("fresh", None).unwrap();
        assert!(manifest.contains("name = \"fresh\""));
        assert!(manifest.contains("version = \"0.1.0\""));
        assert!(manifest.contains("[dev-dependencies]"));
        assert!(manifest.contains("gleeunit = \">= 1.0.0 and < 2.0.0\""));
        // Parses as a valid gleam manifest.
        crate::gleam::GleamManifest::parse(&manifest).unwrap();
    }
}
