//! Schema for the `[tools.trellis]` table of the workspace root's
//! `gleam.toml` — the single source of configured (not derived) workspace
//! facts, living in the manifest format the ecosystem already uses.
//! Everything is optional: when `members` is omitted, workspace members are
//! auto-discovered from git (every non-ignored `gleam.toml`), and when the
//! whole table is absent trellis runs configless with the same discovery.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;

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
#[serde(rename_all = "snake_case")]
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
    #[serde(default)]
    pub doctor: DoctorConfig,
    /// Keys under `[tools.trellis]` that no field claimed. Collected rather
    /// than deserialized — see [`ConfigFile::from_document`].
    #[serde(skip)]
    pub unknown_keys: Vec<String>,
    /// Keys spelled in the pre-0.8 kebab-case style. Accepted, then reported —
    /// see [`collect_deprecated_keys`].
    #[serde(skip)]
    pub deprecated_keys: Vec<DeprecatedKey>,
}

/// A key under `[tools.trellis]` still spelled the pre-0.8 kebab-case way.
///
/// Goal #5 in the design is "fail loudly on drift". The old spelling is a
/// [`serde`] alias, so it still configures what it always did — but a workspace
/// that never migrates is a workspace whose config quietly diverges from every
/// example in the documentation, so `doctor` says so.
#[derive(Debug, Clone)]
pub struct DeprecatedKey {
    /// Dotted path beneath `[tools.trellis]`, e.g. `publish.tag-format`.
    pub path: String,
    /// The same path in the spelling trellis documents, e.g.
    /// `publish.tag_format`.
    pub replacement: String,
}

/// What a check that is a judgment call does about what it finds: fail the
/// run, report it and carry on, or not look at all. Shared by `doctor` and by
/// `changelog check`, which differ only in which end they default to.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Strictness {
    #[default]
    Warn,
    Error,
    Off,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DoctorConfig {
    /// Whether members disagreeing on a shared external dependency's
    /// requirement is a warning (the default), an error, or unchecked.
    /// Divergence is sometimes intended, so this does not fail CI by default.
    #[serde(default)]
    pub shared_dependencies: Strictness,
}

/// Tables under `[tools.trellis]` whose *keys* are chosen by the user rather
/// than by trellis: task names, `exclude` selectors, and the member-path globs
/// of `publish.package_tags_overrides` and `publish.lifecycle.packages`. A
/// hyphen in one of those is the user's own naming (or a directory name inside
/// a glob), not a stale spelling. Entries are full snake-cased dotted paths,
/// so a schema table that happens to share a segment name (`packages`, say) is
/// not silently exempted.
const FREE_FORM_TABLES: [&str; 4] = [
    "exclude",
    "tasks",
    "publish.package_tags_overrides",
    "publish.lifecycle.packages",
];

/// Find the keys still spelled the pre-0.8 kebab-case way.
///
/// Every key trellis defines is snake_case, and the old spellings are [`serde`]
/// aliases — so serde consumes them and `serde_ignored` never sees them. This
/// walks the raw table instead. A hyphenated key that is *not* in `ignored`
/// deserialized into some field, which at a schema position can only mean an
/// alias fired. Deriving it this way rather than from a hand-listed set of old
/// names keeps the two from drifting apart.
fn collect_deprecated_keys(trellis: &toml::Value, ignored: &[String]) -> Vec<DeprecatedKey> {
    let mut found = Vec::new();
    walk_schema_keys(trellis, &mut String::new(), &mut |path, key| {
        if key.contains('-') && !ignored.iter().any(|ignored| ignored == path) {
            found.push(DeprecatedKey {
                path: path.to_string(),
                replacement: snake_case_path(path),
            });
        }
    });
    found
}

/// Visit every key that trellis itself names, in dotted-path form, skipping the
/// key level of each [`FREE_FORM_TABLES`] entry — a task named `check-all` is
/// not a deprecated key. The tables *beneath* those keys are still visited, so
/// `tasks.check-all.needs-deps` is.
fn walk_schema_keys(value: &toml::Value, path: &mut String, visit: &mut impl FnMut(&str, &str)) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, value) in table {
        let restore = path.len();
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(key);
        visit(path, key);
        if FREE_FORM_TABLES.contains(&snake_case(path).as_str()) {
            // The next level down is user-named; the one after that is ours.
            if let Some(entries) = value.as_table() {
                for (name, value) in entries {
                    let restore = path.len();
                    path.push('.');
                    path.push_str(name);
                    walk_schema_keys(value, path, visit);
                    path.truncate(restore);
                }
            }
        } else {
            walk_schema_keys(value, path, visit);
        }
        path.truncate(restore);
    }
}

fn snake_case(key: &str) -> String {
    key.replace('-', "_")
}

/// Snake-case only the *last* segment of a dotted path: the segments above it
/// may be user-chosen names ([`FREE_FORM_TABLES`]) that must be quoted back
/// unchanged.
fn snake_case_path(path: &str) -> String {
    match path.rsplit_once('.') {
        Some((parent, key)) => format!("{parent}.{}", snake_case(key)),
        None => snake_case(path),
    }
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
#[serde(rename_all = "snake_case")]
pub struct TaskConfig {
    /// Shell command run in each member directory.
    pub command: String,
    /// Run `gleam deps download` first if the package's deps aren't cached.
    #[serde(default, alias = "needs-deps")]
    pub needs_deps: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishConfig {
    /// Naming scheme for the immutable per-version tag — the format
    /// [`TagLevel::Exact`] substitutes into. `{name}` and `{version}`.
    #[serde(default = "default_exact_tag_format")]
    pub exact_tag_format: String,
    /// Naming scheme for the moving series tags — the format every series
    /// [`TagLevel`] substitutes into. `{name}` and `{series}`.
    #[serde(default = "default_series_tag_format", alias = "series-tag-format")]
    pub series_tag_format: String,
    /// Which tags a release maintains for each package, one entry per tag, for
    /// members without an override. See [`TagLevel`].
    #[serde(default = "default_package_tags")]
    pub package_tags: Vec<TagLevel>,
    /// Per-member overrides of [`PublishConfig::package_tags`], keyed by a
    /// member-path glob. A member matched by globs resolving to different
    /// lists is an error; matches agreeing on the same list are fine.
    #[serde(default)]
    pub package_tags_overrides: BTreeMap<String, Vec<TagLevel>>,
    /// Package whose manifest version drives the repository tag, enabling it.
    #[serde(default)]
    pub repository_tag_package: Option<String>,
    /// Repository tag template; `{series}` is substituted, and `{name}` would
    /// be written literally, so it is rejected.
    #[serde(default)]
    pub repository_tag_format: Option<String>,
    /// Which repository tags to maintain. Empty by default and required
    /// alongside the other two repository keys: the repository tag is opt-in,
    /// and inferring its levels from `package_tags` would mean a list written
    /// for packages silently deciding what the repository publishes.
    #[serde(default)]
    pub repository_tags: Vec<TagLevel>,
    /// How a path dep is rewritten to a Hex requirement at publish time.
    #[serde(default, alias = "path-dep-requirement")]
    pub path_dep_requirement: PathDepRequirement,
    /// Retry/backoff policy for Hex-touching steps.
    #[serde(default)]
    pub retry: RetryConfig,
    /// Per-package release lifecycle: which of changelog/version, git tags,
    /// and Hex publishing a member participates in. See
    /// [`ReleaseLifecycle`].
    #[serde(default)]
    pub lifecycle: LifecycleConfig,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            exact_tag_format: default_exact_tag_format(),
            series_tag_format: default_series_tag_format(),
            package_tags: default_package_tags(),
            package_tags_overrides: BTreeMap::new(),
            repository_tag_package: None,
            repository_tag_format: None,
            repository_tags: Vec::new(),
            path_dep_requirement: PathDepRequirement::default(),
            retry: RetryConfig::default(),
            lifecycle: LifecycleConfig::default(),
        }
    }
}

/// How much of the release pipeline a member participates in.
///
/// Replaces the old binary `@release` boundary with three states, so a
/// monorepo can hold packages at different maturity levels without moving
/// directories: build-and-test-only packages, packages versioned and tagged
/// in git but never published, and fully published packages. The states form
/// a capability ladder — [`ReleaseLifecycle::available_to`] is the rule
/// [`crate::commands::doctor`]'s `release_boundary` check enforces for
/// runtime path dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseLifecycle {
    /// No changelog/version, no git tags, no Hex publish — build and test
    /// only. The legacy `exclude.@release` mapping resolves here.
    Workspace,
    /// Changelog, version, and git tags/releases, but never published to Hex.
    GitOnly,
    /// The full pipeline: changelog, version, git tags/releases, and Hex
    /// publish. The default lifecycle.
    #[default]
    Hex,
}

impl ReleaseLifecycle {
    /// Every lifecycle, least to most capable — the order summaries display.
    pub const ALL: [ReleaseLifecycle; 3] = [
        ReleaseLifecycle::Workspace,
        ReleaseLifecycle::GitOnly,
        ReleaseLifecycle::Hex,
    ];

    /// The configuration spelling, for messages that quote it back.
    pub fn key(self) -> &'static str {
        match self {
            ReleaseLifecycle::Workspace => "workspace",
            ReleaseLifecycle::GitOnly => "git_only",
            ReleaseLifecycle::Hex => "hex",
        }
    }

    /// Position on the capability ladder; higher reaches further through the
    /// release pipeline.
    fn rank(self) -> u8 {
        match self {
            ReleaseLifecycle::Workspace => 0,
            ReleaseLifecycle::GitOnly => 1,
            ReleaseLifecycle::Hex => 2,
        }
    }

    /// True when a package at this lifecycle is present in the distribution of
    /// a dependent at `dependent`'s lifecycle — a dependency must be at least
    /// as capable as its dependent.
    pub fn available_to(self, dependent: ReleaseLifecycle) -> bool {
        self.rank() >= dependent.rank()
    }
}

/// `[tools.trellis.publish.lifecycle]`: the workspace default plus per-package
/// overrides matched by member-path glob. `packages` is free-form — see
/// [`FREE_FORM_TABLES`] — since its keys are globs, not schema names.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LifecycleConfig {
    /// Lifecycle for a member matched by no `packages` glob and no legacy
    /// `exclude.@release` glob.
    #[serde(default)]
    pub default: ReleaseLifecycle,
    /// Member-path glob -> lifecycle. A member matched by globs resolving to
    /// different lifecycles is a deterministic error; matches agreeing on the
    /// same lifecycle are fine. Takes precedence over `exclude.@release`.
    #[serde(default)]
    pub packages: BTreeMap<String, ReleaseLifecycle>,
}

fn default_exact_tag_format() -> String {
    "{name}-v{version}".to_string()
}

fn default_series_tag_format() -> String {
    "{name}-v{series}".to_string()
}

fn default_package_tags() -> Vec<TagLevel> {
    vec![TagLevel::Exact]
}

/// One tag a release maintains, named by how much of the version it keeps.
///
/// [`TagLevel::Exact`] keeps the whole version and names one immutable tag;
/// the rest truncate it and name a moving series tag. That split is also which
/// format the entry substitutes into — `exact_tag_format` for `Exact`,
/// `series_tag_format` for every other level — and which lifecycle it gets:
/// exact tags are written once and never rewritten, series tags are
/// force-moved on each release in the series.
///
/// Values are levels, not templates: a closed vocabulary is what keeps a tag
/// invertible back to its package ([`crate::commands::tag::resolve_tag`]) and
/// two entries from colliding at a version nobody has released yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagLevel {
    /// `X` — one moving tag per major version: `v1`, `v0`.
    Major,
    /// `X.Y` — one moving tag per minor version: `v1.2`, `v0.10`.
    Minor,
    /// The whole version — one immutable tag per release: `v1.2.3`,
    /// `v1.0.0-rc.1`. The only level that tags a prerelease.
    Exact,
}

impl TagLevel {
    /// Every level, coarsest first — the order error messages list them.
    pub const ALL: [TagLevel; 3] = [TagLevel::Major, TagLevel::Minor, TagLevel::Exact];

    /// The configuration spelling naming this level.
    pub fn key(self) -> &'static str {
        match self {
            TagLevel::Major => "major",
            TagLevel::Minor => "minor",
            TagLevel::Exact => "exact",
        }
    }

    /// The levels quoted for an error message that lists the vocabulary.
    pub fn all_keys() -> String {
        Self::ALL
            .map(|level| format!("`{}`", level.key()))
            .join(", ")
    }

    /// True for the levels that name a moving series tag — everything but
    /// [`TagLevel::Exact`].
    pub fn is_series(self) -> bool {
        !matches!(self, TagLevel::Exact)
    }

    /// The series `version` belongs to at this level, or `None` for
    /// [`TagLevel::Exact`] (which names a version, not a series). Prereleases
    /// belong to no series — a release candidate must never move the tag
    /// consumers pin — and neither does an unparseable version.
    pub fn series_of(self, version: &str) -> Option<String> {
        let parsed = semver::Version::parse(version).ok()?;
        if !parsed.pre.is_empty() {
            return None;
        }
        match self {
            TagLevel::Major => Some(parsed.major.to_string()),
            TagLevel::Minor => Some(format!("{}.{}", parsed.major, parsed.minor)),
            TagLevel::Exact => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathDepRequirement {
    /// `>= X.Y.Z and < (X+1).0.0` — allows minor and patch bumps.
    #[default]
    Minor,
    /// `>= X.Y.Z and < X.(Y+1).0` — allows patch bumps only.
    Patch,
    /// `== X.Y.Z`
    Exact,
}

impl PathDepRequirement {
    /// Whether the requirement generated from `current` can select `candidate`.
    pub fn allows(self, current: &semver::Version, candidate: &semver::Version) -> bool {
        if candidate < current {
            return false;
        }
        match self {
            Self::Minor => {
                let Some(major) = current.major.checked_add(1) else {
                    return true;
                };
                candidate < &semver::Version::new(major, 0, 0)
            }
            Self::Patch => {
                let Some(minor) = current.minor.checked_add(1) else {
                    return true;
                };
                candidate < &semver::Version::new(current.major, minor, 0)
            }
            Self::Exact => candidate == current,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RetryConfig {
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default = "default_initial_delay", alias = "initial-delay")]
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
#[serde(rename_all = "snake_case")]
pub struct ChangelogConfig {
    /// Directory (relative to the workspace root) holding fragments and
    /// batched version sections.
    #[serde(default = "default_changelog_dir")]
    pub dir: String,
    /// Change kinds and the semver bump each implies. The order here is the
    /// order kinds appear in rendered changelog sections.
    #[serde(default = "default_kinds")]
    pub kinds: Vec<KindConfig>,
    /// An optional second grouping axis, rendered *above* `kinds` — sections
    /// for the CLI commands or subsystems a package is made of. Unlike a kind,
    /// a category implies no version bump; it only groups.
    ///
    /// The order here is the render order. Entries naming no category render
    /// last, under `uncategorized_label`. Empty (the default) switches the
    /// axis off entirely.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Template for the first line of a package's CHANGELOG.md.
    /// Context: `name`.
    #[serde(default = "default_header_format", alias = "header-format")]
    pub header_format: String,
    /// Template for a version heading. Context: `name`, `version`, `date`,
    /// `tag`, `series`.
    #[serde(default = "default_version_format", alias = "version-format")]
    pub version_format: String,
    /// Template for a category heading within a version. Context: `category`,
    /// `name`, `version`. Also renders the `uncategorized_label` block, so a
    /// custom template shapes every category heading alike.
    #[serde(default = "default_category_format")]
    pub category_format: String,
    /// Heading text for entries naming no category. Read only when
    /// `categories` is non-empty.
    #[serde(default = "default_uncategorized_label")]
    pub uncategorized_label: String,
    /// Template for a kind heading within a version. Context: `kind`,
    /// `category`, `name`, `version`.
    ///
    /// `None` means "unset", which is not the same as the default: kinds sit
    /// at `###` normally, but one level deeper once categories occupy that
    /// level. Read it through [`ChangelogConfig::kind_format`], never
    /// directly.
    #[serde(default, rename = "kind_format", alias = "kind-format")]
    pub kind_format_override: Option<String>,
    /// Template for one change entry. Context: `body`, `kind`, `category`,
    /// `name`, `version`.
    #[serde(default = "default_change_format", alias = "change-format")]
    pub change_format: String,
    /// Kind used for the entries generated when a workspace dependency bumps.
    /// Must name one of `kinds`; that kind's `bump` is what a package bumps by
    /// when a dependency bump is the only reason it is being released.
    #[serde(default = "default_dependency_kind")]
    pub dependency_kind: String,
    /// Template for the *body* of one such entry — it still goes through
    /// `change_format`. Context: `dependency`, `dependency_version`, `project`.
    #[serde(default = "default_dependency_body")]
    pub dependency_body: String,
    /// What `changelog check` does when a changed releasable package owns no
    /// unreleased fragment: fail (the default), report it advisorily, or not
    /// check at all. Only that verdict is a judgment call — an *invalid*
    /// fragment fails at every setting.
    ///
    /// Spelled with its own default rather than `#[serde(default)]`: the
    /// derived [`Strictness::default`] is `warn`, which is right for `doctor`
    /// and would silently unlatch every existing PR gate here.
    #[serde(default = "default_changelog_strictness")]
    pub strictness: Strictness,
}

impl Default for ChangelogConfig {
    fn default() -> Self {
        Self {
            dir: default_changelog_dir(),
            kinds: default_kinds(),
            categories: Vec::new(),
            header_format: default_header_format(),
            version_format: default_version_format(),
            category_format: default_category_format(),
            uncategorized_label: default_uncategorized_label(),
            kind_format_override: None,
            change_format: default_change_format(),
            dependency_kind: default_dependency_kind(),
            dependency_body: default_dependency_body(),
            strictness: default_changelog_strictness(),
        }
    }
}

impl ChangelogConfig {
    /// Whether the category axis is on. One predicate drives both the extra
    /// grouping level and the kind heading depth, so the two cannot disagree.
    ///
    /// Config-wide rather than per-section: a release carrying no categorized
    /// entry still renders the `uncategorized_label` wrapper, so heading depth
    /// never flips between adjacent versions of a generated file.
    pub fn categories_enabled(&self) -> bool {
        !self.categories.is_empty()
    }

    /// The kind heading template. An explicit `kind_format` always wins;
    /// otherwise kinds sit at `###`, or at `####` once categories hold `###`.
    pub fn kind_format(&self) -> &str {
        match &self.kind_format_override {
            Some(explicit) => explicit,
            None if self.categories_enabled() => NESTED_KIND_FORMAT,
            None => DEFAULT_KIND_FORMAT,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct KindConfig {
    pub label: String,
    pub bump: Bump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bump {
    Patch,
    Minor,
    Major,
}

fn default_changelog_dir() -> String {
    ".changes".to_string()
}

fn default_changelog_strictness() -> Strictness {
    Strictness::Error
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

/// Kind headings sit directly under the version heading…
const DEFAULT_KIND_FORMAT: &str = "### {{ kind }}";
/// …unless categories hold that level, in which case kinds drop one deeper.
/// A `##` heading here would be read as a version by the release-notes
/// extractor in `commands::tag`, so neither level may use one.
const NESTED_KIND_FORMAT: &str = "#### {{ kind }}";

fn default_category_format() -> String {
    "### {{ category }}".to_string()
}

fn default_uncategorized_label() -> String {
    "Other".to_string()
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
    #[cfg(test)]
    pub fn from_gleam_toml(text: &str) -> Result<Self> {
        let document: toml::Value = toml::from_str(text).context("failed to parse gleam.toml")?;
        Self::from_document(&document)
    }

    /// Load from an already-parsed `gleam.toml`, reading the `[tools.trellis]`
    /// table. Unknown keys are recorded (see [`ConfigFile::unknown_keys`])
    /// rather than rejected, so a workspace using a key from a newer trellis
    /// still loads under a pinned older one; `doctor` reports them.
    pub fn from_document(document: &toml::Value) -> Result<Self> {
        let Some(trellis) = document.get("tools").and_then(|tools| tools.get("trellis")) else {
            bail!("gleam.toml has no [tools.trellis] table");
        };
        let mut ignored = Vec::new();
        let mut config: Self =
            serde_ignored::deserialize(trellis.clone(), |path| ignored.push(path.to_string()))
                .context("invalid [tools.trellis] configuration")?;
        config.deprecated_keys = collect_deprecated_keys(trellis, &ignored);
        config.unknown_keys = ignored;
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
            doctor: DoctorConfig::default(),
            // There is no table, so there is nothing in it to misspell.
            unknown_keys: Vec::new(),
            deprecated_keys: Vec::new(),
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
        self.reject_removed_keys()?;
        if !self.publish.exact_tag_format.contains("{version}") {
            bail!(
                "`exact_tag_format` `{}` has no {{version}} placeholder",
                self.publish.exact_tag_format
            );
        }
        if !self.publish.series_tag_format.contains("{series}") {
            bail!(
                "`series_tag_format` `{}` has no {{series}} placeholder",
                self.publish.series_tag_format
            );
        }
        if self.publish.package_tags.is_empty() {
            bail!(
                "`package_tags` is empty; list at least one of {}, or set the package's \
                 `publish.lifecycle` to `workspace` so it is not released at all",
                TagLevel::all_keys()
            );
        }
        for (glob, levels) in &self.publish.package_tags_overrides {
            if levels.is_empty() {
                bail!("`package_tags_overrides` entry `{glob}` is empty; see `package_tags`");
            }
        }
        // All three or none. The repository tag is opt-in and each key answers
        // a question the other two cannot: which package signals a release,
        // what the tag is called, and which series it tracks. Defaulting any
        // of them would let a half-written config publish a tag nobody asked
        // for, or configure one that silently produces nothing.
        let keys = [
            (
                "repository_tag_package",
                self.publish.repository_tag_package.is_some(),
            ),
            (
                "repository_tag_format",
                self.publish.repository_tag_format.is_some(),
            ),
            ("repository_tags", !self.publish.repository_tags.is_empty()),
        ];
        let missing: Vec<String> = keys
            .iter()
            .filter(|(_, present)| !present)
            .map(|(key, _)| format!("`{key}`"))
            .collect();
        if !missing.is_empty() && missing.len() < keys.len() {
            bail!(
                "the repository tag needs `repository_tag_package`, `repository_tag_format`, \
                 and a non-empty `repository_tags`; missing {}",
                missing.join(" and ")
            );
        }
        if let Some(format) = &self.publish.repository_tag_format {
            if !format.contains("{series}") {
                bail!("`repository_tag_format` `{format}` has no {{series}} placeholder");
            }
            // `{series}` is the template's only placeholder; a `{name}` would
            // be written into the tag literally.
            if format.contains("{name}") {
                bail!(
                    "`repository_tag_format` `{format}` cannot contain {{name}}; \
                     the tag is repository-wide, not per-package"
                );
            }
        }
        if self.publish.repository_tags.contains(&TagLevel::Exact) {
            bail!(
                "`repository_tags` cannot contain `exact`; the repository tag names a \
                 series, not one version"
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
                "`dependency_kind` `{dependency_kind}` is not one of `kinds`; add it, e.g. \
                 `{{ label = \"{dependency_kind}\", bump = \"patch\" }}`, or point \
                 `dependency_kind` at an existing kind ({})",
                crate::changelog::kind_labels(&self.changelog.kinds)
            );
        }
        // Both blocks render through `category_format`, so a collision would
        // emit the same heading twice in one section.
        let label = &self.changelog.uncategorized_label;
        if self.changelog.categories.iter().any(|c| c == label) {
            bail!(
                "`uncategorized_label` `{label}` is also one of `categories`; \
                 entries with and without a category would render under the \
                 same heading. Rename one of them."
            );
        }
        Ok(())
    }

    /// The immutable tag naming `version`, from `exact_tag_format`.
    pub fn exact_tag(&self, name: &str, version: &str) -> String {
        self.publish
            .exact_tag_format
            .replace("{name}", name)
            .replace("{version}", version)
    }

    /// Every moving tag `version` calls for at `levels`, in the order given.
    /// Empty when the version has no series (a prerelease, or an unparseable
    /// version) or `levels` holds no series level.
    pub fn series_tags(&self, name: &str, version: &str, levels: &[TagLevel]) -> Vec<String> {
        format_series_tags(levels, version, |series| {
            self.publish
                .series_tag_format
                .replace("{name}", name)
                .replace("{series}", series)
        })
    }

    /// True when the series tag is repository-wide — one tag shared by every
    /// series-mode member rather than one per package. Deprecated; see
    /// `doctor`'s tag checks.
    pub fn series_tag_is_repo_wide(&self) -> bool {
        !self.publish.series_tag_format.contains("{name}")
    }

    /// The anchored repository tags for `version`. Empty when the feature is
    /// unconfigured or the anchor is on a prerelease.
    pub fn repository_tags(&self, version: &str) -> Vec<String> {
        let Some(format) = self.publish.repository_tag_format.as_ref() else {
            return Vec::new();
        };
        format_series_tags(&self.publish.repository_tags, version, |series| {
            format.replace("{series}", series)
        })
    }
}

/// Keys removed by the tag-config redesign, each with the replacement its
/// error names.
///
/// These are rejected rather than reported as unrecognized. Trellis is lenient
/// about unknown keys on purpose — one may belong to a newer trellis — but
/// leniency is wrong here: silently ignoring a workspace's `tag_format` or
/// `tag_mode` falls back to a default that writes *different tags*, and a
/// series tag that stops moving or an exact tag under a new name is not
/// something to discover after the release.
const REMOVED_KEYS: [(&str, &str); 4] = [
    (
        "publish.tag_format",
        "renamed to `exact_tag_format`, to pair with `series_tag_format`",
    ),
    (
        "publish.tag_mode",
        "replaced by `package_tags`, which lists the tags directly — \
         `exact` becomes package_tags = [\"exact\"], `series` becomes \
         [\"minor\"], `both` becomes [\"exact\", \"minor\"]",
    ),
    (
        "publish.tag_mode_overrides",
        "replaced by `package_tags_overrides`, keyed by member-path glob \
         rather than by mode: { 'packages/cli' = ['exact', 'major'] }",
    ),
    (
        "publish.repository_series",
        "flattened into `repository_tag_package`, `repository_tag_format`, \
         and `repository_tags` alongside the other publish keys",
    ),
];

impl ConfigFile {
    /// Fail on any key the redesign removed, naming its replacement.
    ///
    /// Reads `unknown_keys`, which is where a removed key lands once no field
    /// claims it — including a nested one under a removed table, matched by
    /// prefix, and a pre-0.8 kebab spelling, matched after snake-casing.
    fn reject_removed_keys(&self) -> Result<()> {
        for path in &self.unknown_keys {
            let path = snake_case(path);
            for (removed, replacement) in REMOVED_KEYS {
                if path == removed || path.starts_with(&format!("{removed}.")) {
                    bail!("`{removed}` has been removed; it is {replacement}");
                }
            }
        }
        Ok(())
    }
}

/// Render one tag per level, skipping levels that name no series. `major` and
/// `minor` never name the same series, so the dedup only absorbs a level
/// listed twice.
fn format_series_tags(
    levels: &[TagLevel],
    version: &str,
    format: impl Fn(&str) -> String,
) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for series in levels.iter().filter_map(|level| level.series_of(version)) {
        let tag = format(&series);
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_dep_requirement_allows_only_versions_inside_its_range() {
        let current = semver::Version::parse("1.2.3").unwrap();
        let patch = semver::Version::parse("1.2.4").unwrap();
        let minor = semver::Version::parse("1.3.0").unwrap();
        let major = semver::Version::parse("2.0.0").unwrap();

        assert!(PathDepRequirement::Minor.allows(&current, &patch));
        assert!(PathDepRequirement::Minor.allows(&current, &minor));
        assert!(!PathDepRequirement::Minor.allows(&current, &major));
        assert!(PathDepRequirement::Patch.allows(&current, &patch));
        assert!(!PathDepRequirement::Patch.allows(&current, &minor));
        assert!(!PathDepRequirement::Exact.allows(&current, &patch));
        assert!(PathDepRequirement::Exact.allows(&current, &current));
    }

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
            needs_deps = true

            [tools.trellis.publish]
            exact_tag_format = "{name}-v{version}"
            path_dep_requirement = "minor"
            retry = { attempts = 5, initial_delay = "30s", multiplier = 2 }

            [tools.trellis.changelog]
            dir = "changes"
            version_format = "## {{ name }} {{ version }} ({{ date }})"
            dependency_kind = "Docs"
            kinds = [
                { label = "Boom", bump = "major" },
                { label = "Docs", bump = "patch" },
            ]
            categories = ["build", "publish"]
            uncategorized_label = "Everything else"
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
        assert_eq!(config.changelog.categories, ["build", "publish"]);
        assert_eq!(config.changelog.uncategorized_label, "Everything else");
        assert!(config.deprecated_keys.is_empty());
        assert!(config.unknown_keys.is_empty());
    }

    /// `Strictness::default()` is `warn`, which is right for `doctor` and wrong
    /// here: inheriting it would turn every existing PR gate advisory on
    /// upgrade. The changelog axis defaults the other way, on purpose.
    #[test]
    fn changelog_strictness_defaults_to_error_and_parses() {
        let unset = ConfigFile::from_gleam_toml("[tools.trellis]\n").unwrap();
        assert_eq!(unset.changelog.strictness, Strictness::Error);
        assert_eq!(ChangelogConfig::default().strictness, Strictness::Error);
        assert_eq!(
            DoctorConfig::default().shared_dependencies,
            Strictness::Warn
        );

        for (value, expected) in [
            ("warn", Strictness::Warn),
            ("error", Strictness::Error),
            ("off", Strictness::Off),
        ] {
            let config = ConfigFile::from_gleam_toml(&format!(
                "[tools.trellis.changelog]\nstrictness = \"{value}\"\n"
            ))
            .unwrap();
            assert_eq!(config.changelog.strictness, expected);
            assert!(
                config.unknown_keys.is_empty(),
                "`strictness` is a known key"
            );
        }
    }

    /// Categories are a plain string array, not a table, so `walk_schema_keys`
    /// never descends into the labels — a category named after a hyphenated
    /// CLI command is the user's business, not a deprecated key.
    #[test]
    fn hyphens_in_category_labels_are_not_deprecations() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis.changelog]\ncategories = [\"markdown-help\", \"no-color\"]\n",
        )
        .unwrap();
        assert_eq!(config.changelog.categories, ["markdown-help", "no-color"]);
        assert!(config.deprecated_keys.is_empty());
        assert!(config.unknown_keys.is_empty());
    }

    /// Kinds normally head a section at `###`; with categories holding that
    /// level they drop to `####`, unless the config says otherwise.
    #[test]
    fn kind_headings_demote_only_when_categories_are_in_play() {
        let parse = |table: &str| {
            ConfigFile::from_gleam_toml(&format!("[tools.trellis.changelog]\n{table}"))
                .unwrap()
                .changelog
        };
        assert_eq!(parse("").kind_format(), "### {{ kind }}");
        assert_eq!(
            parse("categories = [\"build\"]\n").kind_format(),
            "#### {{ kind }}"
        );
        // An explicit `kind_format` wins either way.
        assert_eq!(
            parse("kind_format = \"**{{ kind }}**\"\n").kind_format(),
            "**{{ kind }}**"
        );
        assert_eq!(
            parse("categories = [\"build\"]\nkind_format = \"**{{ kind }}**\"\n").kind_format(),
            "**{{ kind }}**"
        );
    }

    #[test]
    fn uncategorized_label_may_not_collide_with_a_category() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.changelog]\ncategories = [\"build\", \"Other\"]\n",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("`uncategorized_label` `Other`"),
            "{message}"
        );

        // Renaming either side settles it.
        ConfigFile::from_gleam_toml(
            "[tools.trellis.changelog]\ncategories = [\"build\", \"Other\"]\nuncategorized_label = \"Misc\"\n",
        )
        .unwrap();
    }

    /// Every key released through v0.7.0 was kebab-case, so the old spelling
    /// still parses to exactly what it always did.
    #[test]
    fn pre_0_8_kebab_case_keys_still_parse() {
        let text = r####"
            [tools.trellis]
            members = ["packages/*"]

            [tools.trellis.tasks.lint]
            command = "gleam run -m glinter"
            needs-deps = true

            [tools.trellis.publish]
            series-tag-format = "{name}@{series}"
            path-dep-requirement = "patch"
            retry = { attempts = 3, initial-delay = "10ms", multiplier = 4 }

            [tools.trellis.changelog]
            header-format = "# {{ name }}"
            version-format = "## {{ version }}"
            kind-format = "### {{ kind }}"
            change-format = "* {{ body }}"
        "####;
        let config = ConfigFile::from_gleam_toml(text).unwrap();
        assert!(config.tasks["lint"].needs_deps);
        assert_eq!(config.publish.series_tag_format, "{name}@{series}");
        assert_eq!(
            config.publish.path_dep_requirement,
            PathDepRequirement::Patch
        );
        assert_eq!(config.publish.retry.initial_delay, "10ms");
        assert_eq!(config.changelog.header_format, "# {{ name }}");
        assert_eq!(config.changelog.version_format, "## {{ version }}");
        assert_eq!(config.changelog.kind_format(), "### {{ kind }}");
        assert_eq!(config.changelog.change_format, "* {{ body }}");
        // Nothing was dropped, and every old spelling is reported once. The
        // pre-0.8 `tag-*` keys are absent because the keys they aliased were
        // removed outright — see `removed_tag_keys_fail_with_their_replacement`.
        assert!(config.unknown_keys.is_empty());
        let deprecated = deprecated_paths(&config);
        assert_eq!(
            deprecated,
            [
                "changelog.change-format",
                "changelog.header-format",
                "changelog.kind-format",
                "changelog.version-format",
                "publish.path-dep-requirement",
                "publish.retry.initial-delay",
                "publish.series-tag-format",
                "tasks.lint.needs-deps",
            ]
        );
    }

    #[test]
    fn a_deprecated_key_names_its_replacement() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis]\n[tools.trellis.publish]\nseries-tag-format = \"v{series}\"\n",
        )
        .unwrap();
        let [key] = &config.deprecated_keys[..] else {
            panic!(
                "expected one deprecated key, got {:?}",
                config.deprecated_keys
            );
        };
        assert_eq!(key.path, "publish.series-tag-format");
        assert_eq!(key.replacement, "publish.series_tag_format");
    }

    /// Kebab aliases exist only for keys that predate the 0.8 rename. A key
    /// introduced after it has no stale spelling to keep loading, so
    /// `series-tags` is an unknown key rather than a deprecated one — quietly
    /// accepting it would invent a migration for a key with no history.
    #[test]
    fn keys_added_after_the_rename_have_no_kebab_spelling() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis]\n[tools.trellis.publish]\nseries-tags = [\"major\"]\n",
        )
        .unwrap();
        assert!(
            config.deprecated_keys.is_empty(),
            "{:?}",
            config.deprecated_keys
        );
        assert_eq!(config.unknown_keys, ["publish.series-tags"]);
        // The typo did not silently configure anything.
        assert_eq!(config.publish.package_tags, [TagLevel::Exact]);
    }

    /// The keys of `exclude`, `tasks`, and `tag_mode_overrides` are the user's
    /// own names. A hyphen in one is not a stale spelling, and saying so would
    /// be a warning nobody can act on.
    #[test]
    fn hyphens_in_free_form_table_keys_are_not_deprecations() {
        let config = ConfigFile::from_gleam_toml(
            r###"
            [tools.trellis]
            members = ["packages/*"]
            exclude = { "check-all" = ["examples/*"] }

            [tools.trellis.tasks.check-all]
            command = "gleam check"
        "###,
        )
        .unwrap();
        assert!(config.tasks.contains_key("check-all"));
        assert!(
            config.deprecated_keys.is_empty(),
            "{:?}",
            config.deprecated_keys
        );
    }

    /// ...but the schema keys *beneath* a user-named task are still trellis's,
    /// so a stale one there is still reported — and reported at a path that
    /// leaves the task's own name alone.
    #[test]
    fn a_stale_key_under_a_hyphenated_task_name_is_still_reported() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis.tasks.check-all]\ncommand = \"gleam check\"\nneeds-deps = true\n",
        )
        .unwrap();
        assert!(config.tasks["check-all"].needs_deps);
        assert_eq!(deprecated_paths(&config), ["tasks.check-all.needs-deps"]);
        let [key] = &config.deprecated_keys[..] else {
            unreachable!()
        };
        assert_eq!(key.replacement, "tasks.check-all.needs_deps");
    }

    /// `dependency-kind`, `dependency-body`, and `shared-dependencies` were
    /// added after v0.7.0, so those spellings never reached a release and get
    /// no alias — they are simply unknown.
    #[test]
    fn keys_added_after_the_last_kebab_release_have_no_alias() {
        let config = ConfigFile::from_gleam_toml(
            r###"
            [tools.trellis.changelog]
            dependency-kind = "Docs"

            [tools.trellis.doctor]
            shared-dependencies = "error"
        "###,
        )
        .unwrap();
        // The defaults stand, and the keys are reported as unrecognized.
        assert_eq!(config.changelog.dependency_kind, "Dependencies");
        assert_eq!(config.doctor.shared_dependencies, Strictness::Warn);
        assert!(config.deprecated_keys.is_empty());
        assert_eq!(
            config.unknown_keys,
            ["changelog.dependency-kind", "doctor.shared-dependencies"]
        );
    }

    fn deprecated_paths(config: &ConfigFile) -> Vec<&str> {
        config
            .deprecated_keys
            .iter()
            .map(|key| key.path.as_str())
            .collect()
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let config =
            ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = [\"packages/*\"]").unwrap();
        assert!(config.exclude.is_empty());
        assert_eq!(config.publish.exact_tag_format, "{name}-v{version}");
        assert_eq!(
            config.publish.path_dep_requirement,
            PathDepRequirement::Minor
        );
        assert_eq!(config.exact_tag("core", "1.2.3"), "core-v1.2.3");
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
    fn lifecycle_defaults_to_hex_with_no_package_overrides() {
        let config =
            ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = [\"packages/*\"]").unwrap();
        assert_eq!(config.publish.lifecycle.default, ReleaseLifecycle::Hex);
        assert!(config.publish.lifecycle.packages.is_empty());
    }

    #[test]
    fn lifecycle_parses_nested_table_and_inline_map() {
        let config = ConfigFile::from_gleam_toml(
            r###"
            [tools.trellis.publish.lifecycle]
            default = "hex"
            packages = { "member/path/**" = "git_only", "examples/**" = "workspace" }
            "###,
        )
        .unwrap();
        assert_eq!(config.publish.lifecycle.default, ReleaseLifecycle::Hex);
        assert_eq!(
            config.publish.lifecycle.packages["member/path/**"],
            ReleaseLifecycle::GitOnly
        );
        assert_eq!(
            config.publish.lifecycle.packages["examples/**"],
            ReleaseLifecycle::Workspace
        );
        // Glob keys carrying `-`/`/`/`*` are user-chosen, not deprecated spellings.
        assert!(config.deprecated_keys.is_empty());
        assert!(config.unknown_keys.is_empty());
    }

    #[test]
    fn lifecycle_default_can_be_overridden() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish.lifecycle]\ndefault = \"workspace\"\n",
        )
        .unwrap();
        assert_eq!(
            config.publish.lifecycle.default,
            ReleaseLifecycle::Workspace
        );
    }

    #[test]
    fn lifecycle_rejects_an_unknown_value() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish.lifecycle]\ndefault = \"published\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("published"));
    }

    #[test]
    fn lifecycle_package_glob_rejects_an_unknown_value() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish.lifecycle]\npackages = { \"pkg/*\" = \"nope\" }\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("nope"));
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
            message.contains("`dependency_kind` `Dependencies`"),
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
            dependency_kind = "Docs"
            dependency_body = "{{ dependency }} is now {{ dependency_version }}"
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
        assert_eq!(config.exact_tag("core", "1.2.3"), "core-v1.2.3");
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
    fn a_series_is_the_version_truncated_at_its_level() {
        let major = |v| TagLevel::Major.series_of(v);
        let minor = |v| TagLevel::Minor.series_of(v);
        assert_eq!(major("0.10.3").as_deref(), Some("0"));
        assert_eq!(minor("0.10.3").as_deref(), Some("0.10"));
        assert_eq!(major("1.2.3").as_deref(), Some("1"));
        assert_eq!(minor("1.2.3").as_deref(), Some("1.2"));
        assert_eq!(major("10.0.0").as_deref(), Some("10"));
        assert_eq!(minor("0.0.17").as_deref(), Some("0.0"));
        // A prerelease belongs to no series, and so never moves a tag.
        assert_eq!(major("1.0.0-beta"), None);
        assert_eq!(minor("0.0.1-rc.1"), None);
        // Build metadata is not a prerelease.
        assert_eq!(major("1.0.0+build.5").as_deref(), Some("1"));
        assert_eq!(minor("not-a-version"), None);
    }

    #[test]
    fn formats_series_tags() {
        let minor = [TagLevel::Minor];
        let config =
            ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = [\"packages/*\"]").unwrap();
        assert_eq!(config.publish.series_tag_format, "{name}-v{series}");
        assert_eq!(config.series_tags("core", "0.0.3", &minor), ["core-v0.0"]);
        assert!(config.series_tags("core", "0.0.3-rc.1", &minor).is_empty());
        // `exact` names a version, not a series, so it contributes no tag here.
        assert!(
            config
                .series_tags("core", "0.0.3", &[TagLevel::Exact])
                .is_empty()
        );

        let repo_wide = ConfigFile::from_gleam_toml(
            "[tools.trellis]\n[tools.trellis.publish]\nseries_tag_format = \"v{series}\"\n",
        )
        .unwrap();
        assert_eq!(repo_wide.series_tags("core", "0.0.3", &minor), ["v0.0"]);
    }

    #[test]
    fn parses_package_tags_and_overrides() {
        let config = ConfigFile::from_gleam_toml(
            r###"
            [tools.trellis]
            members = ["packages/*"]

            [tools.trellis.publish]
            package_tags = ["minor"]
            package_tags_overrides = { "packages/lat_*" = ["exact", "minor"], "packages/old" = ["exact"] }
        "###,
        )
        .unwrap();
        assert_eq!(config.publish.package_tags, [TagLevel::Minor]);
        assert_eq!(
            config.publish.package_tags_overrides["packages/lat_*"],
            [TagLevel::Exact, TagLevel::Minor]
        );
        assert_eq!(
            config.publish.package_tags_overrides["packages/old"],
            [TagLevel::Exact]
        );
    }

    /// The pre-redesign default was `tag_mode = "exact"` — no series tag
    /// unless asked for — and `package_tags` has to preserve it.
    #[test]
    fn package_tags_default_to_exact_only() {
        let config =
            ConfigFile::from_gleam_toml("[tools.trellis]\nmembers = [\"packages/*\"]").unwrap();
        assert_eq!(config.publish.package_tags, [TagLevel::Exact]);
        assert!(config.publish.package_tags_overrides.is_empty());
        assert!(!TagLevel::Exact.is_series());
        assert!(TagLevel::Major.is_series() && TagLevel::Minor.is_series());
    }

    #[test]
    fn an_empty_override_list_is_rejected() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\npackage_tags_overrides = { \"packages/x\" = [] }\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("packages/x"), "{err:#}");
    }

    #[test]
    fn series_tag_format_must_carry_the_series_placeholder() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis]\n[tools.trellis.publish]\nseries_tag_format = \"{name}-latest\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("{series}"), "{err:#}");
    }

    #[test]
    fn parses_and_formats_the_repository_tag() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\n\
             repository_tag_package = \"core\"\nrepository_tag_format = \"v{series}\"\n\
             repository_tags = [\"minor\"]\n",
        )
        .unwrap();
        assert_eq!(
            config.publish.repository_tag_package.as_deref(),
            Some("core")
        );
        assert_eq!(config.repository_tags("0.4.3"), ["v0.4"]);
        assert!(config.repository_tags("0.4.3-rc.1").is_empty());
    }

    #[test]
    fn the_repository_tag_is_optional() {
        let config = ConfigFile::from_gleam_toml("[tools.trellis]").unwrap();
        assert!(config.publish.repository_tag_package.is_none());
        assert!(config.repository_tags("1.2.3").is_empty());
    }

    /// A partly-written repository tag is always a mistake, and the error
    /// names the keys that are missing rather than defaulting them.
    #[test]
    fn the_repository_tag_needs_all_three_keys_or_none() {
        for (partial, missing) in [
            (
                "repository_tag_package = \"core\"",
                ["repository_tag_format", "repository_tags"],
            ),
            (
                "repository_tag_format = \"v{series}\"",
                ["repository_tag_package", "repository_tags"],
            ),
            (
                "repository_tags = [\"minor\"]",
                ["repository_tag_package", "repository_tag_format"],
            ),
        ] {
            let err = ConfigFile::from_gleam_toml(&format!("[tools.trellis.publish]\n{partial}\n"))
                .unwrap_err();
            let message = format!("{err:#}");
            for key in missing {
                assert!(message.contains(key), "{partial} -> {message}");
            }
        }
        // None of the three is the ordinary case: the feature is off.
        let off = ConfigFile::from_gleam_toml("[tools.trellis]").unwrap();
        assert!(off.publish.repository_tags.is_empty());
        assert!(off.repository_tags("1.2.3").is_empty());
    }

    #[test]
    fn both_levels_produce_two_tags_at_every_major() {
        let both = [TagLevel::Major, TagLevel::Minor];
        let config = ConfigFile::from_gleam_toml("[tools.trellis]").unwrap();
        assert_eq!(
            config.series_tags("core", "0.10.3", &both),
            ["core-v0", "core-v0.10"]
        );
        assert_eq!(
            config.series_tags("core", "1.2.3", &both),
            ["core-v1", "core-v1.2"]
        );
        assert!(config.series_tags("core", "1.2.3-rc.1", &both).is_empty());
        // A level listed twice is still one tag.
        assert_eq!(
            config.series_tags("core", "1.2.3", &[TagLevel::Major, TagLevel::Major]),
            ["core-v1"]
        );
    }

    /// The repository tag is independent of what packages publish, so its
    /// levels are stated rather than inferred from `package_tags`.
    #[test]
    fn the_repository_tag_takes_its_own_levels() {
        let config = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\npackage_tags = [\"exact\", \"major\", \"minor\"]\n\
             repository_tag_package = \"core\"\nrepository_tag_format = \"v{series}\"\n\
             repository_tags = [\"major\"]\n",
        )
        .unwrap();
        assert_eq!(config.publish.package_tags.len(), 3);
        assert_eq!(config.repository_tags("0.4.3"), ["v0"], "not inherited");
    }

    #[test]
    fn empty_package_tags_are_rejected_and_the_error_lists_the_levels() {
        let err = ConfigFile::from_gleam_toml("[tools.trellis.publish]\npackage_tags = []\n")
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("package_tags"), "{message}");
        assert!(message.contains("`major`, `minor`, `exact`"), "{message}");

        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\n\
             repository_tag_package = \"core\"\nrepository_tag_format = \"v{series}\"\n\
             repository_tags = []\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("repository_tags"), "{err:#}");
    }

    /// The repository tag names a series; `exact` would ask it to name one
    /// version, which is what package exact tags are for.
    #[test]
    fn the_repository_tag_rejects_the_exact_level() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\n\
             repository_tag_package = \"core\"\nrepository_tag_format = \"v{series}\"\n\
             repository_tags = [\"exact\"]\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("cannot contain `exact`"),
            "{err:#}"
        );
    }

    /// Ignoring a removed key would fall back to a default that writes
    /// different tags, so each one fails the load and names its replacement.
    #[test]
    fn removed_tag_keys_fail_with_their_replacement() {
        let cases = [
            ("tag_format = \"v{version}\"", "exact_tag_format"),
            ("tag_mode = \"both\"", "package_tags"),
            (
                "tag_mode_overrides = { both = [\"packages/x\"] }",
                "package_tags_overrides",
            ),
        ];
        for (removed, replacement) in cases {
            let err = ConfigFile::from_gleam_toml(&format!("[tools.trellis.publish]\n{removed}\n"))
                .unwrap_err();
            let message = format!("{err:#}");
            assert!(message.contains("has been removed"), "{message}");
            assert!(message.contains(replacement), "{message}");
        }
        // The old sub-table is matched by prefix, through its nested keys.
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish.repository_series]\npackage = \"core\"\nformat = \"v{series}\"\n",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("publish.repository_series"), "{message}");
        assert!(message.contains("repository_tag_package"), "{message}");
        // A pre-0.8 kebab spelling of a removed key reports the same way.
        let err = ConfigFile::from_gleam_toml("[tools.trellis.publish]\ntag-mode = \"series\"\n")
            .unwrap_err();
        assert!(format!("{err:#}").contains("package_tags"), "{err:#}");
    }

    #[test]
    fn an_unknown_series_level_names_the_vocabulary() {
        let err =
            ConfigFile::from_gleam_toml("[tools.trellis.publish]\npackage_tags = [\"patch\"]\n")
                .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("major"), "{message}");
        assert!(message.contains("minor"), "{message}");
    }

    #[test]
    fn repository_tag_format_must_carry_the_series_placeholder() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\n\
             repository_tag_package = \"core\"\nrepository_tag_format = \"latest\"\n\
             repository_tags = [\"minor\"]\n",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("`repository_tag_format`"), "{message}");
        assert!(message.contains("{series}"), "{message}");
    }

    #[test]
    fn repository_tag_format_rejects_the_name_placeholder() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\n\
             repository_tag_package = \"core\"\nrepository_tag_format = \"{name}-v{series}\"\n\
             repository_tags = [\"minor\"]\n",
        )
        .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("`repository_tag_format`"), "{message}");
        assert!(message.contains("{name}"), "{message}");
    }

    #[test]
    fn exact_tag_format_must_carry_the_version_placeholder() {
        let err = ConfigFile::from_gleam_toml(
            "[tools.trellis.publish]\nexact_tag_format = \"{name}-latest\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("{version}"), "{err:#}");
    }

    #[test]
    fn bump_ordering_supports_max() {
        assert!(Bump::Major > Bump::Minor);
        assert!(Bump::Minor > Bump::Patch);
    }
}
