//! Schema for the `[tools.trellis]` table of the workspace root's
//! `gleam.toml` — the single source of configured (not derived) workspace
//! facts, living in the manifest format the ecosystem already uses.
//! Everything except `members` is optional.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Prefix reserved for special `exclude` keys (currently just
/// [`RELEASE_EXCLUDE_KEY`]) so they can never collide with a task name —
/// built-in verbs and `[tools.trellis.tasks]` entries may not start with it.
pub const RESERVED_PREFIX: &str = "@";

/// The `exclude` key whose globs exclude members from changelog, versioning,
/// tagging, and publishing, rather than from a single task.
pub const RELEASE_EXCLUDE_KEY: &str = "@release";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConfigFile {
    /// Glob array matched against directories relative to the workspace root.
    pub members: Vec<String>,
    /// Per-task glob arrays matched against member paths. Keys are task names
    /// (built-in verbs or `[tools.trellis.tasks]` entries), except the
    /// reserved [`RELEASE_EXCLUDE_KEY`] (`@release`), whose globs exclude
    /// members from changelog, versioning, tagging, and publishing instead.
    #[serde(default)]
    pub exclude: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskConfig>,
    #[serde(default)]
    pub publish: PublishConfig,
    #[serde(default)]
    pub changelog: ChangelogConfig,
}

/// True when a parsed `gleam.toml` carries a `[tools.trellis]` table — the
/// marker that makes a directory the workspace root.
pub fn has_trellis_table(document: &toml::Value) -> bool {
    document
        .get("tools")
        .and_then(|tools| tools.get("trellis"))
        .is_some_and(toml::Value::is_table)
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
    #[serde(default)]
    pub path_dep_requirement: PathDepRequirement,
    /// Retry/backoff policy for Hex-touching steps.
    #[serde(default)]
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
    /// `>= X.Y.Z and < (X+1).0.0` — allows minor and patch bumps.
    #[default]
    Minor,
    /// `>= X.Y.Z and < X.(Y+1).0` — allows patch bumps only.
    Patch,
    /// `== X.Y.Z`
    Exact,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
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

/// The native changelog engine's configuration. Fragments are TOML files in
/// `<dir>/unreleased/`; batched version sections live in `<dir>/<package>/`;
/// each package's CHANGELOG.md is assembled from those. All formats are
/// minijinja templates and share the same context: `name`, `version`,
/// `date`, `tag`, `kind`, `body` — fields not meaningful for a given
/// template (e.g. `kind`/`body` in `header-format`) render as empty.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChangelogConfig {
    /// Directory (relative to the workspace root) holding fragments and
    /// batched version sections.
    #[serde(default = "default_changelog_dir")]
    pub dir: String,
    /// Change kinds and the semver bump each implies. The order here is the
    /// order kinds appear in rendered changelog sections.
    #[serde(default = "default_kinds")]
    pub kinds: Vec<KindConfig>,
    /// Template for the first line of a package's CHANGELOG.md.
    #[serde(default = "default_header_format")]
    pub header_format: String,
    /// Template for a version heading.
    #[serde(default = "default_version_format")]
    pub version_format: String,
    /// Template for a kind heading within a version.
    #[serde(default = "default_kind_format")]
    pub kind_format: String,
    /// Template for one change entry.
    #[serde(default = "default_change_format")]
    pub change_format: String,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        Self {
            dir: default_changelog_dir(),
            kinds: default_kinds(),
            header_format: default_header_format(),
            version_format: default_version_format(),
            kind_format: default_kind_format(),
            change_format: default_change_format(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KindConfig {
    pub label: String,
    pub bump: Bump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bump {
    Patch,
    Minor,
    Major,
}

fn default_changelog_dir() -> String {
    ".changes".to_string()
}

fn default_kinds() -> Vec<KindConfig> {
    [
        ("Initial Release", Bump::Major),
        ("Breaking", Bump::Major),
        ("Removed", Bump::Major),
        ("Added", Bump::Minor),
        ("Changed", Bump::Minor),
        ("Deprecated", Bump::Minor),
        ("Fixed", Bump::Patch),
        ("Performance", Bump::Patch),
        ("Security", Bump::Patch),
    ]
    .into_iter()
    .map(|(label, bump)| KindConfig {
        label: label.to_string(),
        bump,
    })
    .collect()
}

fn default_header_format() -> String {
    "# {{ name }} changelog".to_string()
}

fn default_version_format() -> String {
    "## v{{ version }} - {{ date }}".to_string()
}

fn default_kind_format() -> String {
    "### {{ kind }}".to_string()
}

fn default_change_format() -> String {
    "- {{ body }}".to_string()
}

impl ConfigFile {
    /// Load from the workspace root's `gleam.toml`, reading the
    /// `[tools.trellis]` table.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::from_gleam_toml(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn from_gleam_toml(text: &str) -> Result<Self> {
        let document: toml::Value = toml::from_str(text).context("failed to parse gleam.toml")?;
        let Some(trellis) = document.get("tools").and_then(|tools| tools.get("trellis")) else {
            bail!("gleam.toml has no [tools.trellis] table");
        };
        let config: Self = trellis
            .clone()
            .try_into()
            .context("invalid [tools.trellis] configuration")?;
        config.validate()?;
        Ok(config)
    }

    /// Reject task names that could collide with a reserved `exclude` key
    /// (currently just `@release`), and any `exclude` key that misuses the
    /// reserved prefix without being one.
    fn validate(&self) -> Result<()> {
        for name in self.tasks.keys() {
            if name.starts_with(RESERVED_PREFIX) {
                bail!(
                    "task name `{name}` may not start with `{RESERVED_PREFIX}`; \
                     that prefix is reserved for special `exclude` keys like `{RELEASE_EXCLUDE_KEY}`"
                );
            }
        }
        for key in self.exclude.keys() {
            if key.starts_with(RESERVED_PREFIX) && key != RELEASE_EXCLUDE_KEY {
                bail!(
                    "unknown reserved `exclude` key `{key}`; did you mean `{RELEASE_EXCLUDE_KEY}`?"
                );
            }
        }
        Ok(())
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
    fn parses_full_config_from_tools_trellis() {
        let text = r###"
            # The root gleam.toml may also be a regular package manifest;
            # trellis only reads [tools.trellis].
            name = "lattice_root"
            version = "0.0.0"

            [tools.trellis]
            members = ["packages/lattice_*", "examples/*"]
            exclude = { docs = ["examples/*"], "@release" = ["packages/private-*"] }

            [tools.trellis.tasks.lint]
            command = "gleam run -m glinter"
            needs-deps = true

            [tools.trellis.publish]
            tag-format = "{name}-v{version}"
            path-dep-requirement = "minor"
            retry = { attempts = 5, initial-delay = "30s", multiplier = 2 }

            [tools.trellis.changelog]
            dir = "changes"
            version-format = "## {{ name }} {{ version }} ({{ date }})"
            kinds = [
                { label = "Boom", bump = "major" },
                { label = "Docs", bump = "patch" },
            ]
        "###;
        let config = ConfigFile::from_gleam_toml(text).unwrap();
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.exclude["docs"], vec!["examples/*"]);
        assert_eq!(
            config.exclude[RELEASE_EXCLUDE_KEY],
            vec!["packages/private-*"]
        );
        assert!(config.tasks["lint"].needs_deps);
        assert_eq!(config.publish.retry.attempts, 5);
        assert_eq!(config.changelog.dir, "changes");
        assert_eq!(config.changelog.kinds.len(), 2);
        assert_eq!(config.changelog.kinds[0].bump, Bump::Major);
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let config =
            ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = [\"packages/*\"]").unwrap();
        assert!(config.exclude.is_empty());
        assert_eq!(config.publish.tag_format, "{name}-v{version}");
        assert_eq!(
            config.publish.path_dep_requirement,
            PathDepRequirement::Minor
        );
        assert_eq!(config.format_tag("core", "1.2.3"), "core-v1.2.3");
        assert_eq!(config.changelog.dir, ".changes");
        assert!(config.changelog.kinds.iter().any(|k| k.label == "Added"));
        assert!(
            config
                .changelog
                .kinds
                .iter()
                .any(|k| k.label == "Initial Release" && k.bump == Bump::Major)
        );
        assert_eq!(
            config.changelog.version_format,
            "## v{{ version }} - {{ date }}"
        );
    }

    #[test]
    fn task_name_may_not_use_the_reserved_prefix() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis]\nmembers = [\"packages/*\"]\n[tools.trellis.tasks.\"@lint\"]\ncommand = \"x\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("@lint"));
    }

    #[test]
    fn unknown_reserved_exclude_key_is_a_clear_error() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis]\nmembers = [\"packages/*\"]\nexclude = { \"@relase\" = [\"x\"] }\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains(RELEASE_EXCLUDE_KEY));
    }

    #[test]
    fn missing_tools_trellis_is_a_clear_error() {
        let err = ConfigFile::from_gleam_toml("name = \"pkg\"\nversion = \"1.0.0\"").unwrap_err();
        assert!(err.to_string().contains("[tools.trellis]"));
    }

    #[test]
    fn detects_the_trellis_table() {
        let with: toml::Value = toml::from_str("[tools.trellis]\nmembers = []").unwrap();
        assert!(has_trellis_table(&with));
        let without: toml::Value = toml::from_str("name = \"pkg\"").unwrap();
        assert!(!has_trellis_table(&without));
        let not_table: toml::Value = toml::from_str("[tools]\ntrellis = true").unwrap();
        assert!(!has_trellis_table(&not_table));
    }

    #[test]
    fn bump_ordering_supports_max() {
        assert!(Bump::Major > Bump::Minor);
        assert!(Bump::Minor > Bump::Patch);
    }
}
