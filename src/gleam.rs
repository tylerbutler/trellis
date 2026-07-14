//! Parsing of member `gleam.toml` manifests — the derived source of truth for
//! names, versions, and the path-dependency graph.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct GleamManifest {
    pub name: String,
    pub version: String,
    pub dependencies: Vec<Dependency>,
    /// True when this manifest carries its own `[tools.trellis]` table —
    /// legitimate only at the workspace root, since root discovery walks up
    /// to the first manifest that has one.
    pub has_trellis_config: bool,
}

#[derive(Debug, Clone)]
pub struct Dependency {
    pub name: String,
    pub requirement: Requirement,
    /// True if declared under `[dev-dependencies]`.
    pub dev: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    /// A Hex version requirement, e.g. `">= 1.0.0 and < 2.0.0"`.
    Hex(String),
    /// A path dependency, relative to the package directory.
    Path(String),
    /// A git dependency, e.g. `{ git = "https://...", ref = "..." }`.
    /// External to the workspace, like a Hex dependency, but not
    /// publishable to Hex as a runtime requirement.
    Git(String),
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    name: String,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDep>,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: BTreeMap<String, RawDep>,
    #[serde(default)]
    tools: Option<toml::Value>,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawDep {
    Requirement(String),
    Detailed {
        path: Option<String>,
        version: Option<String>,
        git: Option<String>,
    },
}

impl GleamManifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawManifest = toml::from_str(text)?;
        let mut dependencies = Vec::new();
        for (deps, dev) in [(&raw.dependencies, false), (&raw.dev_dependencies, true)] {
            for (name, dep) in deps {
                let requirement = match dep {
                    RawDep::Requirement(req) => Requirement::Hex(req.clone()),
                    RawDep::Detailed {
                        path: Some(path), ..
                    } => Requirement::Path(path.clone()),
                    RawDep::Detailed {
                        version: Some(version),
                        ..
                    } => Requirement::Hex(version.clone()),
                    RawDep::Detailed { git: Some(git), .. } => Requirement::Git(git.clone()),
                    RawDep::Detailed { .. } => {
                        anyhow::bail!(
                            "dependency `{name}` has neither a version, a path, nor a git source"
                        )
                    }
                };
                dependencies.push(Dependency {
                    name: name.clone(),
                    requirement,
                    dev,
                });
            }
        }
        let has_trellis_config = raw
            .tools
            .as_ref()
            .and_then(|tools| tools.get("trellis"))
            .is_some();
        Ok(Self {
            name: raw.name,
            version: raw.version,
            dependencies,
            has_trellis_config,
        })
    }

    /// Names of path dependencies together with their relative paths.
    pub fn path_deps(&self) -> impl Iterator<Item = (&str, &str, bool)> {
        self.dependencies
            .iter()
            .filter_map(|dep| match &dep.requirement {
                Requirement::Path(path) => Some((dep.name.as_str(), path.as_str(), dep.dev)),
                Requirement::Hex(_) | Requirement::Git(_) => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_and_path_deps() {
        let manifest = GleamManifest::parse(
            r#"
            name = "lattice_cli"
            version = "0.3.1"

            [dependencies]
            gleam_stdlib = ">= 0.34.0 and < 2.0.0"
            lattice_core = { path = "../lattice_core" }

            [dev-dependencies]
            gleeunit = ">= 1.0.0 and < 2.0.0"
            lattice_testing = { path = "../lattice_testing" }
            "#,
        )
        .unwrap();
        assert_eq!(manifest.name, "lattice_cli");
        assert_eq!(manifest.version, "0.3.1");
        let paths: Vec<_> = manifest.path_deps().collect();
        assert_eq!(
            paths,
            vec![
                ("lattice_core", "../lattice_core", false),
                ("lattice_testing", "../lattice_testing", true),
            ]
        );
    }

    #[test]
    fn parses_git_deps_as_external() {
        let manifest = GleamManifest::parse(
            r#"
            name = "beryl_mist"
            version = "0.0.1"

            [dependencies]
            beryl = { path = "../beryl" }

            [dev-dependencies]
            aquamarine = { git = "https://github.com/tylerbutler/aquamarine.git", ref = "main" }
            "#,
        )
        .unwrap();
        let git_dep = manifest
            .dependencies
            .iter()
            .find(|dep| dep.name == "aquamarine")
            .unwrap();
        assert_eq!(
            git_dep.requirement,
            Requirement::Git("https://github.com/tylerbutler/aquamarine.git".to_string())
        );
        assert!(git_dep.dev);
        // Git deps are external: they never join the path-dependency graph.
        let paths: Vec<_> = manifest.path_deps().collect();
        assert_eq!(paths, vec![("beryl", "../beryl", false)]);
    }

    #[test]
    fn version_defaults_when_missing() {
        let manifest = GleamManifest::parse("name = \"foo\"").unwrap();
        assert_eq!(manifest.version, "0.0.0");
    }

    #[test]
    fn rejects_dep_with_no_source() {
        let err = GleamManifest::parse("name = \"foo\"\n[dependencies]\nbar = {}").unwrap_err();
        assert!(err.to_string().contains("bar"));
    }
}
