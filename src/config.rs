//! Schema for `workspace.toml`, the single source of configured (not derived)
//! workspace facts. Everything except `[workspace] members` is optional.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConfigFile {
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskConfig>,
    #[serde(default)]
    pub publish: PublishConfig,
    // Consumed by the changelog/version layer (rollout phase 2); parsed and
    // validated now so configs can be written ahead of it.
    #[serde(default)]
    #[allow(dead_code)]
    pub changelog: ChangelogConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkspaceSection {
    /// Glob array matched against directories relative to the workspace root.
    pub members: Vec<String>,
    /// Members matching these globs participate in task fan-out but are
    /// excluded from changelog, versioning, tagging, and publishing.
    #[serde(default)]
    pub ignore_release: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TaskConfig {
    /// Shell command run in each member directory.
    pub command: String,
    /// Run `gleam deps download` first if the package's deps aren't cached.
    #[serde(default)]
    pub needs_deps: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PublishConfig {
    /// Tag naming scheme; `{name}` and `{version}` are substituted.
    #[serde(default = "default_tag_format")]
    pub tag_format: String,
    /// How a path dep is rewritten to a Hex requirement at publish time.
    /// Consumed by `trellis publish` (rollout phase 3).
    #[serde(default)]
    #[allow(dead_code)]
    pub path_dep_requirement: PathDepRequirement,
    /// Retry/backoff policy for Hex-touching steps (rollout phase 3).
    #[serde(default)]
    #[allow(dead_code)]
    pub retry: RetryConfig,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            tag_format: default_tag_format(),
            path_dep_requirement: PathDepRequirement::default(),
            retry: RetryConfig::default(),
        }
    }
}

fn default_tag_format() -> String {
    "{name}-v{version}".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathDepRequirement {
    /// `>= X.Y.Z and < (X+1).0.0`
    #[default]
    Caret,
    /// `== X.Y.Z`
    Exact,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)] // consumed by `trellis publish` (rollout phase 3)
pub struct RetryConfig {
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default = "default_initial_delay")]
    pub initial_delay: String,
    #[serde(default = "default_multiplier")]
    pub multiplier: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            attempts: default_attempts(),
            initial_delay: default_initial_delay(),
            multiplier: default_multiplier(),
        }
    }
}

fn default_attempts() -> u32 {
    5
}
fn default_initial_delay() -> String {
    "30s".to_string()
}
fn default_multiplier() -> u32 {
    2
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)] // consumed by the changelog layer (rollout phase 2)
pub struct ChangelogConfig {
    #[serde(default = "default_changelog_tool")]
    pub tool: String,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        Self {
            tool: default_changelog_tool(),
        }
    }
}

fn default_changelog_tool() -> String {
    "changie".to_string()
}

impl ConfigFile {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: ConfigFile =
            toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    }

    pub fn format_tag(&self, name: &str, version: &str) -> String {
        self.publish
            .tag_format
            .replace("{name}", name)
            .replace("{version}", version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let text = r#"
            [workspace]
            members = ["packages/lattice_*", "examples"]
            ignore-release = ["examples"]

            [tasks.lint]
            command = "gleam run -m glinter"
            needs-deps = true

            [publish]
            tag-format = "{name}-v{version}"
            path-dep-requirement = "caret"
            retry = { attempts = 5, initial-delay = "30s", multiplier = 2 }

            [changelog]
            tool = "changie"
        "#;
        let config: ConfigFile = toml::from_str(text).unwrap();
        assert_eq!(config.workspace.members.len(), 2);
        assert_eq!(config.workspace.ignore_release, vec!["examples"]);
        assert!(config.tasks["lint"].needs_deps);
        assert_eq!(config.publish.retry.attempts, 5);
        assert_eq!(config.changelog.tool, "changie");
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let config: ConfigFile = toml::from_str("[workspace]\nmembers = [\"packages/*\"]").unwrap();
        assert!(config.workspace.ignore_release.is_empty());
        assert_eq!(config.publish.tag_format, "{name}-v{version}");
        assert_eq!(
            config.publish.path_dep_requirement,
            PathDepRequirement::Caret
        );
        assert_eq!(config.format_tag("core", "1.2.3"), "core-v1.2.3");
    }
}
