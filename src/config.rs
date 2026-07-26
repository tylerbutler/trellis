//! Schema for the `[tools.trellis]` table of the workspace root's
//! `gleam.toml` — the single source of configured (not derived) workspace
//! facts, living in the manifest format the ecosystem already uses.
//! Everything is optional: when `members` is omitted, workspace members are
//! auto-discovered from git (every non-ignored `gleam.toml`), and when the
//! whole table is absent trellis runs configless with the same discovery.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Prefix reserved for special `exclude` keys ([`RELEASE_EXCLUDE_KEY`] and
/// [`MEMBERS_EXCLUDE_KEY`]) so they can never collide with a task name —
/// built-in verbs and `[tools.trellis.tasks]` entries may not start with it.
pub const RESERVED_PREFIX: &str = "@";

/// The `exclude` key whose globs exclude members from changelog, versioning,
/// tagging, and publishing, rather than from a single task.
pub const RELEASE_EXCLUDE_KEY: &str = "@release";

/// The `exclude` key whose globs remove directories from workspace membership
/// entirely — they are never parsed, graphed, or touched by any command.
/// Applies to explicit `members` globs and to auto-discovered members alike.
pub const MEMBERS_EXCLUDE_KEY: &str = "@members";

/// All special `exclude` keys, for validation and error messages.
pub const RESERVED_EXCLUDE_KEYS: [&str; 2] = [RELEASE_EXCLUDE_KEY, MEMBERS_EXCLUDE_KEY];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConfigFile {
    /// Glob array matched against directories relative to the workspace root.
    /// When omitted, members are auto-discovered: every non-gitignored
    /// `gleam.toml` in the repository (outside `build/`) marks a member.
    pub members: Option<Vec<String>>,
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
    /// Naming scheme for the moving series tag; `{name}` and `{series}` are
    /// substituted. Omit `{name}` for a single repository-wide series tag.
    #[serde(default = "default_series_tag_format")]
    pub series_tag_format: String,
    /// Which tags a release creates, for members without an override.
    #[serde(default)]
    pub tag_mode: TagMode,
    /// Per-member overrides of [`PublishConfig::tag_mode`], keyed by mode name
    /// ([`TagMode::key`]); values are globs matched against member paths. A
    /// member matching globs under two different modes is an error.
    #[serde(default)]
    pub tag_mode_overrides: BTreeMap<String, Vec<String>>,
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
            series_tag_format: default_series_tag_format(),
            tag_mode: TagMode::default(),
            tag_mode_overrides: BTreeMap::new(),
            path_dep_requirement: PathDepRequirement::default(),
            retry: RetryConfig::default(),
        }
    }
}

fn default_tag_format() -> String {
    "{name}-v{version}".to_string()
}

fn default_series_tag_format() -> String {
    "{name}-v{series}".to_string()
}

/// Which git tags a release creates for a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagMode {
    /// Immutable per-version tags only (`{name}-v1.2.0`).
    #[default]
    Exact,
    /// Moving series tags only (`{name}-v0.0`, re-pointed at each release).
    Series,
    /// Both an immutable per-version tag and a moving series tag.
    Both,
}

impl TagMode {
    /// Every mode, in the order error messages list them.
    pub const ALL: [TagMode; 3] = [TagMode::Exact, TagMode::Series, TagMode::Both];

    /// The configuration key naming this mode.
    pub fn key(self) -> &'static str {
        match self {
            TagMode::Exact => "exact",
            TagMode::Series => "series",
            TagMode::Both => "both",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }

    /// True when releases create an immutable `tag-format` tag.
    pub fn includes_exact(self) -> bool {
        matches!(self, TagMode::Exact | TagMode::Both)
    }

    /// True when releases move a `series-tag-format` tag.
    pub fn includes_series(self) -> bool {
        matches!(self, TagMode::Series | TagMode::Both)
    }
}

/// The moving series a version belongs to: `0.Y` while the major is 0 (where
/// every minor bump is a breaking change), `X` afterward. Prereleases belong
/// to no series — a release candidate must never move the tag consumers pin.
pub fn series_of(version: &str) -> Option<String> {
    let version = semver::Version::parse(version).ok()?;
    if !version.pre.is_empty() {
        return None;
    }
    Some(if version.major == 0 {
        format!("0.{}", version.minor)
    } else {
        version.major.to_string()
    })
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
/// minijinja templates.
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
    /// Context: `name`.
    #[serde(default = "default_header_format")]
    pub header_format: String,
    /// Template for a version heading. Context: `name`, `version`, `date`,
    /// `tag`, `series`.
    #[serde(default = "default_version_format")]
    pub version_format: String,
    /// Template for a kind heading within a version. Context: `kind`, `name`,
    /// `version`.
    #[serde(default = "default_kind_format")]
    pub kind_format: String,
    /// Template for one change entry. Context: `body`, `kind`, `name`,
    /// `version`.
    #[serde(default = "default_change_format")]
    pub change_format: String,
    /// Kind used for the entries generated when a workspace dependency bumps.
    /// Must name one of `kinds`; that kind's `bump` is what a package bumps by
    /// when a dependency bump is the only reason it is being released.
    #[serde(default = "default_dependency_kind")]
    pub dependency_kind: String,
    /// Template for the *body* of one such entry — it still goes through
    /// `change-format`. Context: `dependency`, `dependency_version`, `project`.
    #[serde(default = "default_dependency_body")]
    pub dependency_body: String,
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
            dependency_kind: default_dependency_kind(),
            dependency_body: default_dependency_body(),
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
        ("Dependencies", Bump::Patch),
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

fn default_dependency_kind() -> String {
    "Dependencies".to_string()
}

fn default_dependency_body() -> String {
    "Updated {{ dependency }} to {{ dependency_version }}".to_string()
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

    /// The synthesized configuration for a workspace with no `[tools.trellis]`
    /// table anywhere: members auto-discovered, everything else defaulted.
    pub fn configless() -> Self {
        Self {
            members: None,
            exclude: BTreeMap::new(),
            tasks: BTreeMap::new(),
            publish: PublishConfig::default(),
            changelog: ChangelogConfig::default(),
        }
    }

    /// Reject an explicitly empty `members` list (omit it to auto-discover),
    /// task names that could collide with a reserved `exclude` key, and any
    /// `exclude` key that misuses the reserved prefix without being one.
    fn validate(&self) -> Result<()> {
        if self.members.as_ref().is_some_and(Vec::is_empty) {
            bail!(
                "`members` is empty; remove the key entirely to auto-discover members, \
                 or list at least one glob"
            );
        }
        for name in self.tasks.keys() {
            if name.starts_with(RESERVED_PREFIX) {
                bail!(
                    "task name `{name}` may not start with `{RESERVED_PREFIX}`; \
                     that prefix is reserved for special `exclude` keys like `{RELEASE_EXCLUDE_KEY}`"
                );
            }
        }
        for key in self.exclude.keys() {
            if key.starts_with(RESERVED_PREFIX) && !RESERVED_EXCLUDE_KEYS.contains(&key.as_str()) {
                bail!(
                    "unknown reserved `exclude` key `{key}`; the special keys are {}",
                    RESERVED_EXCLUDE_KEYS
                        .map(|reserved| format!("`{reserved}`"))
                        .join(" and ")
                );
            }
        }
        for key in self.publish.tag_mode_overrides.keys() {
            if TagMode::from_key(key).is_none() {
                bail!(
                    "unknown `tag-mode-overrides` key `{key}`; the tag modes are {}",
                    TagMode::ALL
                        .map(|mode| format!("`{}`", mode.key()))
                        .join(", ")
                );
            }
        }
        if !self.publish.series_tag_format.contains("{series}") {
            bail!(
                "`series-tag-format` `{}` has no {{series}} placeholder",
                self.publish.series_tag_format
            );
        }
        let dependency_kind = &self.changelog.dependency_kind;
        if !self
            .changelog
            .kinds
            .iter()
            .any(|kind| &kind.label == dependency_kind)
        {
            bail!(
                "`dependency-kind` `{dependency_kind}` is not one of `kinds`; add it, e.g. \
                 `{{ label = \"{dependency_kind}\", bump = \"patch\" }}`, or point \
                 `dependency-kind` at an existing kind ({})",
                crate::changelog::kind_labels(&self.changelog.kinds)
            );
        }
        Ok(())
    }

    pub fn format_tag(&self, name: &str, version: &str) -> String {
        self.publish
            .tag_format
            .replace("{name}", name)
            .replace("{version}", version)
    }

    /// The moving tag for `version`'s series, or `None` when the version has
    /// no series (a prerelease, or an unparseable version).
    pub fn format_series_tag(&self, name: &str, version: &str) -> Option<String> {
        series_of(version).map(|series| {
            self.publish
                .series_tag_format
                .replace("{name}", name)
                .replace("{series}", &series)
        })
    }

    /// True when the series tag is repository-wide — one tag shared by every
    /// series-mode member rather than one per package.
    pub fn series_tag_is_repo_wide(&self) -> bool {
        !self.publish.series_tag_format.contains("{name}")
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
            dependency-kind = "Docs"
            kinds = [
                { label = "Boom", bump = "major" },
                { label = "Docs", bump = "patch" },
            ]
        "###;
        let config = ConfigFile::from_gleam_toml(text).unwrap();
        assert_eq!(config.members.as_deref().unwrap().len(), 2);
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
        assert_eq!(config.changelog.dependency_kind, "Docs");
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
        // Ripple entries need a kind of their own, so the defaults ship one.
        assert_eq!(config.changelog.dependency_kind, "Dependencies");
        assert_eq!(
            config.changelog.dependency_body,
            "Updated {{ dependency }} to {{ dependency_version }}"
        );
        assert!(
            config
                .changelog
                .kinds
                .iter()
                .any(|k| k.label == "Dependencies" && k.bump == Bump::Patch)
        );
    }

    #[test]
    fn dependency_kind_must_name_a_configured_kind() {
        let err = ConfigFile::from_gleam_toml(
            r###"
            [tools.trellis.changelog]
            kinds = [{ label = "Docs", bump = "patch" }]
        "###,
        )
        .unwrap_err();
        let message = format!("{err:#}");
        // The error names the kind, the fix, and what is currently available.
        assert!(
            message.contains("`dependency-kind` `Dependencies`"),
            "{message}"
        );
        assert!(
            message.contains(r#"{ label = "Dependencies", bump = "patch" }"#),
            "{message}"
        );
        assert!(message.contains("Docs"), "{message}");
    }

    #[test]
    fn dependency_kind_may_point_at_an_existing_kind() {
        let config = ConfigFile::from_gleam_toml(
            r###"
            [tools.trellis.changelog]
            dependency-kind = "Docs"
            dependency-body = "{{ dependency }} is now {{ dependency_version }}"
            kinds = [{ label = "Docs", bump = "patch" }]
        "###,
        )
        .unwrap();
        assert_eq!(config.changelog.dependency_kind, "Docs");
        assert_eq!(
            config.changelog.dependency_body,
            "{{ dependency }} is now {{ dependency_version }}"
        );
    }

    #[test]
    fn omitted_members_means_auto_discovery() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis]\nexclude = { \"@members\" = [\"tests/fixtures/*\"] }",
        )
        .unwrap();
        assert!(config.members.is_none());
        assert_eq!(
            config.exclude[MEMBERS_EXCLUDE_KEY],
            vec!["tests/fixtures/*"]
        );
    }

    #[test]
    fn empty_members_is_a_clear_error() {
        let err = ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = []").unwrap_err();
        assert!(err.to_string().contains("auto-discover"), "{err:#}");
    }

    #[test]
    fn configless_config_has_defaults_and_no_members() {
        let config = ConfigFile::configless();
        assert!(config.members.is_none());
        assert!(config.exclude.is_empty());
        assert!(config.tasks.is_empty());
        assert_eq!(config.format_tag("core", "1.2.3"), "core-v1.2.3");
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
    fn series_is_derived_from_the_version() {
        // Pre-1.0 packages get a major.minor series; 1.0 and later get the
        // major alone.
        assert_eq!(series_of("0.0.1").as_deref(), Some("0.0"));
        assert_eq!(series_of("0.0.17").as_deref(), Some("0.0"));
        assert_eq!(series_of("0.1.0").as_deref(), Some("0.1"));
        assert_eq!(series_of("0.12.3").as_deref(), Some("0.12"));
        assert_eq!(series_of("1.2.3").as_deref(), Some("1"));
        assert_eq!(series_of("10.0.0").as_deref(), Some("10"));
        // A prerelease belongs to no series, and so never moves a tag.
        assert_eq!(series_of("0.0.1-rc.1"), None);
        assert_eq!(series_of("1.0.0-beta"), None);
        // Build metadata is not a prerelease.
        assert_eq!(series_of("1.0.0+build.5").as_deref(), Some("1"));
        assert_eq!(series_of("not-a-version"), None);
    }

    #[test]
    fn formats_series_tags() {
        let config =
            ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = [\"packages/*\"]").unwrap();
        assert_eq!(config.publish.series_tag_format, "{name}-v{series}");
        assert_eq!(
            config.format_series_tag("core", "0.0.3").as_deref(),
            Some("core-v0.0")
        );
        assert_eq!(config.format_series_tag("core", "0.0.3-rc.1"), None);

        let repo_wide = ConfigFile::from_gleam_toml(
            "[tools.trellis]\n[tools.trellis.publish]\nseries-tag-format = \"v{series}\"\n",
        )
        .unwrap();
        assert_eq!(
            repo_wide.format_series_tag("core", "0.0.3").as_deref(),
            Some("v0.0")
        );
    }

    #[test]
    fn parses_tag_modes_and_overrides() {
        let config = ConfigFile::from_gleam_toml(
            r###"
            [tools.trellis]
            members = ["packages/*"]

            [tools.trellis.publish]
            tag-mode = "series"
            tag-mode-overrides = { both = ["packages/lat_*"], exact = ["packages/old"] }
        "###,
        )
        .unwrap();
        assert_eq!(config.publish.tag_mode, TagMode::Series);
        assert_eq!(
            config.publish.tag_mode_overrides["both"],
            vec!["packages/lat_*"]
        );
        assert_eq!(
            config.publish.tag_mode_overrides["exact"],
            vec!["packages/old"]
        );
    }

    #[test]
    fn tag_mode_defaults_to_exact_only() {
        let config =
            ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = [\"packages/*\"]").unwrap();
        assert_eq!(config.publish.tag_mode, TagMode::Exact);
        assert!(config.publish.tag_mode_overrides.is_empty());
        assert!(config.publish.tag_mode.includes_exact());
        assert!(!config.publish.tag_mode.includes_series());
        assert!(TagMode::Both.includes_exact() && TagMode::Both.includes_series());
        assert!(TagMode::Series.includes_series() && !TagMode::Series.includes_exact());
    }

    #[test]
    fn unknown_tag_mode_override_key_is_a_clear_error() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis]\n[tools.trellis.publish]\ntag-mode-overrides = { seires = [\"x\"] }\n",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("seires"), "{message}");
        assert!(message.contains("series"), "{message}");
    }

    #[test]
    fn series_tag_format_must_carry_the_series_placeholder() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis]\n[tools.trellis.publish]\nseries-tag-format = \"{name}-latest\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("{series}"), "{err:#}");
    }

    #[test]
    fn bump_ordering_supports_max() {
        assert!(Bump::Major > Bump::Minor);
        assert!(Bump::Minor > Bump::Patch);
    }
}
