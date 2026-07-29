//! The `--json` output contract.
//!
//! Every payload trellis emits for machine consumption is defined here, once,
//! as a `Serialize` struct — so the wire format is reviewable in one file
//! instead of inferred from `json!` literals spread across seven commands.
//!
//! Each *document* carries a `schema` identifier of the form
//! `trellis.<payload>/<major>`. Consumers assert on it. Adding a field leaves
//! the identifier alone; renaming, removing, or retyping one bumps the major.
//! `website/src/content/docs/docs/json-output.mdx` states the full policy, and
//! `tests/json_contract.rs` snapshots every shape below so a breaking change
//! fails here rather than in a consumer's repository.
//!
//! Key naming is snake_case — as is every identifier trellis controls, from
//! `[tools.trellis]` keys to the enum values below. `#[serde(rename_all =
//! "snake_case")]` sits on every struct: a no-op for single-word fields, but it
//! means a later multi-word field cannot silently arrive as kebab-case.
//!
//! Two payloads deliberately carry no `schema` field; see [`CiMatrix`] and
//! `commands::ci::outputs`.

use crate::workspace::Workspace;
use serde::Serialize;

/// Which invariant a [`Finding`] came from — a stable identifier a workflow can
/// branch on, rather than a substring of the message.
///
/// This is a documented enum, so adding a variant is a compatible change but
/// renaming or removing one bumps `trellis.doctor`'s major. Keep the set
/// aligned with the `checked:` lines `doctor` prints in text mode: a check the
/// preamble claims to run should be nameable here.
///
/// Unlike [`crate::commands::tag::TagKind`], this lives in `json.rs` rather
/// than in its command module, because `workspace::Diagnostics` produces
/// findings too and must not depend on `commands::doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// A `members` glob is invalid, unreadable, or matches nothing.
    MemberGlob,
    /// A package's `gleam.toml` is missing, unparseable, carries a
    /// `[tools.trellis]` table, or repeats another package's name.
    PackageManifest,
    /// A path dependency escapes the workspace or names no package in it.
    PathDependency,
    /// The dependency graph has a cycle.
    DependencyCycle,
    /// The root `[tools.trellis]` table itself is wrong.
    WorkspaceConfig,
    /// A task-exclusion or tag-mode-override glob matches no member.
    ExclusionGlob,
    /// A releasable package depends on one excluded from release.
    ReleaseBoundary,
    /// Two releasable packages produce the same tag.
    TagCollision,
    /// A `manifest.toml` locks a workspace-internal dep at a stale version.
    LockfileDrift,
    /// A releasable package has no `CHANGELOG.md`.
    ChangelogMissing,
    /// A `CHANGELOG.md` exists but could not be read.
    ChangelogUnreadable,
    /// A package's version is behind the newest one in its changelog.
    ChangelogBehind,
    /// A package has pre-trellis changelog history not yet batched into
    /// `.changes/<pkg>/`, which the next release will adopt.
    ChangelogAdoption,
    /// A package's version is not valid semver, or no header could be rendered
    /// for it.
    PackageVersion,
    /// An unreleased changelog fragment does not parse, or names an unknown
    /// package or kind.
    ChangelogFragment,
    /// The gleam on PATH disagrees with the `.tool-versions` pin.
    Toolchain,
    /// Packages require different versions of the same external dependency.
    SharedDependency,
}

impl Check {
    /// The serialized identifier, for renderings that are not serde — the
    /// `title=` of a GitHub annotation, say. Kept beside the `Serialize` derive
    /// so the two cannot disagree; `check_names_match_serde` asserts it.
    pub fn as_str(self) -> &'static str {
        match self {
            Check::MemberGlob => "member_glob",
            Check::PackageManifest => "package_manifest",
            Check::PathDependency => "path_dependency",
            Check::DependencyCycle => "dependency_cycle",
            Check::WorkspaceConfig => "workspace_config",
            Check::ExclusionGlob => "exclusion_glob",
            Check::ReleaseBoundary => "release_boundary",
            Check::TagCollision => "tag_collision",
            Check::LockfileDrift => "lockfile_drift",
            Check::ChangelogMissing => "changelog_missing",
            Check::ChangelogUnreadable => "changelog_unreadable",
            Check::ChangelogBehind => "changelog_behind",
            Check::ChangelogAdoption => "changelog_adoption",
            Check::PackageVersion => "package_version",
            Check::ChangelogFragment => "changelog_fragment",
            Check::Toolchain => "toolchain",
            Check::SharedDependency => "shared_dependency",
        }
    }

    /// Every variant, so `check_names_match_serde` can cover the whole enum.
    #[cfg(test)]
    const ALL: &'static [Check] = &[
        Check::MemberGlob,
        Check::PackageManifest,
        Check::PathDependency,
        Check::DependencyCycle,
        Check::WorkspaceConfig,
        Check::ExclusionGlob,
        Check::ReleaseBoundary,
        Check::TagCollision,
        Check::LockfileDrift,
        Check::ChangelogMissing,
        Check::ChangelogUnreadable,
        Check::ChangelogBehind,
        Check::ChangelogAdoption,
        Check::PackageVersion,
        Check::ChangelogFragment,
        Check::Toolchain,
        Check::SharedDependency,
    ];
}

/// Whether a [`Finding`] fails the run. Warnings are advisory; `doctor` exits
/// non-zero only on an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// One problem `doctor` found.
///
/// Owned rather than borrowed, unlike the rest of this module: a finding
/// outlives the workspace load that produced it, and `doctor --fix` re-inspects
/// from disk, discarding the `Workspace` a borrowed finding would point into.
///
/// `message` is prose written for a person, like `changelog check`'s `preview`.
/// The field is contractual; its wording is not. Branch on `check`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Finding {
    pub check: Check,
    pub severity: Severity,
    pub message: String,
    /// Workspace-relative, forward slashes, when the finding is attributable to
    /// one file. Absent — not null — when it is a property of the workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Whether `doctor --fix` would remediate this. One fix can clear several
    /// findings, so this is not a one-to-one map onto `fixes`.
    pub fixable: bool,
}

impl Finding {
    pub fn error(check: Check, message: impl Into<String>) -> Self {
        Self {
            check,
            severity: Severity::Error,
            message: message.into(),
            file: None,
            package: None,
            fixable: false,
        }
    }

    pub fn warning(check: Check, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(check, message)
        }
    }

    /// `AsRef<str>` rather than `Into<String>` so a caller can pass a `&String`
    /// it still needs — several checks attribute many findings to one path.
    #[must_use]
    pub fn at(mut self, file: impl AsRef<str>) -> Self {
        self.file = Some(file.as_ref().to_string());
        self
    }

    #[must_use]
    pub fn in_package(mut self, package: impl AsRef<str>) -> Self {
        self.package = Some(package.as_ref().to_string());
        self
    }

    #[must_use]
    pub fn fixable(self) -> Self {
        self.fixable_if(true)
    }

    #[must_use]
    pub fn fixable_if(mut self, fixable: bool) -> Self {
        self.fixable = fixable;
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// A mechanical remedy `doctor --fix` can apply, as `doctor --format json`
/// reports it. `kind` is the stable identifier; `description` is the same prose
/// text mode prints after `would fix:`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FixRecord<'a> {
    pub kind: &'static str,
    pub description: String,
    pub file: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<&'a str>,
}

/// `trellis doctor --format json`.
///
/// `configless` and `auto_members` are workspace facts rather than problems —
/// they are the `note:` lines text mode prints — so they sit at the top level
/// instead of masquerading as findings.
///
/// `fixes` is what `--fix` would still apply; after a successful `--fix` it is
/// empty and `applied` holds what was written. Both are always present.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DoctorDocument<'a> {
    pub schema: &'static str,
    /// Mirrors the exit code: false when any finding is an error.
    pub ok: bool,
    /// How many packages the workspace holds.
    pub packages: usize,
    pub configless: bool,
    /// True when `members` is unset and the package list came from git.
    /// Membership, not the packages themselves, is what was inferred.
    pub auto_members: bool,
    pub findings: &'a [Finding],
    pub fixes: Vec<FixRecord<'a>>,
    pub applied: Vec<FixRecord<'a>>,
}

impl DoctorDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.doctor/1";
}

/// How one package's job ended.
///
/// `skipped` means scheduling stopped at an earlier failure and this package
/// never ran — it is not a pass, and [`crate::runner::all_succeeded`] counts it
/// against the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Success,
    Failed,
    Skipped,
}

/// One package's outcome, shared by [`RunDocument`] and [`ExecDocument`].
///
/// A job runs several commands when a custom task sets `needs_deps`, so
/// `exit_code` and `command` describe the one that *failed* rather than a
/// single command per package.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TaskResult<'a> {
    pub package: &'a str,
    pub path: &'a str,
    pub status: TaskStatus,
    /// Absent — not null — when the job succeeded, was skipped, or the process
    /// left no code of its own (killed by a signal, or never started).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// The command that failed, as it was run. Absent unless `status` is
    /// `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<&'a str>,
}

impl<'a> TaskResult<'a> {
    pub fn new(workspace: &'a Workspace, result: &'a crate::runner::JobResult) -> Self {
        let member = &workspace.members[result.member];
        Self {
            package: &member.name,
            path: &member.rel_path,
            status: match result.status {
                crate::runner::JobStatus::Success => TaskStatus::Success,
                crate::runner::JobStatus::Failed(_) => TaskStatus::Failed,
                crate::runner::JobStatus::Skipped => TaskStatus::Skipped,
            },
            exit_code: result.exit_code,
            // Saturating rather than wrapping: a run long enough to overflow a
            // u64 of milliseconds is not one anybody is reading a payload for.
            duration_ms: u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX),
            command: result.failed_command.as_deref(),
        }
    }
}

/// `trellis run <task> --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RunDocument<'a> {
    pub schema: &'static str,
    /// Mirrors the exit code: false unless every package succeeded.
    pub ok: bool,
    pub task: &'a str,
    /// The `--target` value as given, including `all`. Absent when the flag was
    /// omitted and each package built for its own default target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'static str>,
    pub results: Vec<TaskResult<'a>>,
}

impl RunDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.run/1";
}

/// `trellis exec -- <command...> --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ExecDocument<'a> {
    pub schema: &'static str,
    /// Mirrors the exit code: false unless every package succeeded.
    pub ok: bool,
    /// The command as invoked, one element per argv entry, so a consumer never
    /// has to re-split a quoted string.
    pub command: &'a [String],
    pub results: Vec<TaskResult<'a>>,
}

impl ExecDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.exec/1";
}

/// A workspace member, as `list` and `info` report it.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Package<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub path: &'a str,
    pub releasable: bool,
    pub dependencies: Vec<&'a str>,
    pub dependents: Vec<&'a str>,
}

impl<'a> Package<'a> {
    pub fn new(workspace: &'a Workspace, idx: usize) -> Self {
        let member = &workspace.members[idx];
        Self {
            name: &member.name,
            version: member.version(),
            path: &member.rel_path,
            releasable: member.releasable,
            dependencies: member_names(workspace, workspace.deps_of(idx)),
            dependents: member_names(workspace, workspace.dependents_of(idx)),
        }
    }
}

fn member_names<'a>(workspace: &'a Workspace, indices: &[usize]) -> Vec<&'a str> {
    indices
        .iter()
        .map(|&idx| workspace.members[idx].name.as_str())
        .collect()
}

/// `trellis list --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ListDocument<'a> {
    pub schema: &'static str,
    pub packages: Vec<Package<'a>>,
}

impl<'a> ListDocument<'a> {
    pub const SCHEMA: &'static str = "trellis.list/1";

    pub fn new(workspace: &'a Workspace, selected: &[usize]) -> Self {
        Self {
            schema: Self::SCHEMA,
            packages: selected
                .iter()
                .map(|&idx| Package::new(workspace, idx))
                .collect(),
        }
    }
}

/// `trellis info <package> --json`. The package's fields are flattened to the
/// top level, so this is `list`'s element shape plus a `schema`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct InfoDocument<'a> {
    pub schema: &'static str,
    #[serde(flatten)]
    pub package: Package<'a>,
}

impl<'a> InfoDocument<'a> {
    pub const SCHEMA: &'static str = "trellis.info/1";

    pub fn new(workspace: &'a Workspace, idx: usize) -> Self {
        Self {
            schema: Self::SCHEMA,
            package: Package::new(workspace, idx),
        }
    }
}

/// One node in `trellis graph --format json`. Deliberately not [`Package`]:
/// the edges carry the dependency relation, so repeating it per node would
/// state the same fact twice.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GraphNode<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub path: &'a str,
    pub releasable: bool,
}

/// A dependency edge, pointing from dependent to dependency.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GraphEdge<'a> {
    pub from: &'a str,
    pub to: &'a str,
}

/// `trellis graph --format json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct GraphDocument<'a> {
    pub schema: &'static str,
    pub nodes: Vec<GraphNode<'a>>,
    pub edges: Vec<GraphEdge<'a>>,
}

impl<'a> GraphDocument<'a> {
    pub const SCHEMA: &'static str = "trellis.graph/1";

    pub fn new(workspace: &'a Workspace) -> Self {
        let nodes = workspace
            .members
            .iter()
            .map(|member| GraphNode {
                name: &member.name,
                version: member.version(),
                path: &member.rel_path,
                releasable: member.releasable,
            })
            .collect();
        let mut edges = Vec::new();
        for (idx, member) in workspace.members.iter().enumerate() {
            for &dep in workspace.deps_of(idx) {
                edges.push(GraphEdge {
                    from: &member.name,
                    to: &workspace.members[dep].name,
                });
            }
        }
        Self {
            schema: Self::SCHEMA,
            nodes,
            edges,
        }
    }
}

/// One changed package in `trellis changelog check --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ChangelogPackage<'a> {
    pub name: &'a str,
    /// Always true — the list only contains packages the diff touched. Kept so
    /// a future `--all` can report unchanged packages without a schema bump.
    pub changed: bool,
    pub has_entry: bool,
    pub fragments: usize,
}

/// `trellis changelog check --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ChangelogCheckDocument<'a> {
    pub schema: &'static str,
    pub has_entries: bool,
    pub needs_entry: bool,
    pub invalid_fragments: &'a [String],
    pub packages: Vec<ChangelogPackage<'a>>,
    /// Markdown for a PR sticky comment. The field is contractual; its prose
    /// is not, and changes without a schema bump.
    pub preview: String,
}

impl ChangelogCheckDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.changelog_check/1";
}

/// A workspace dependency that bumped in the same plan. Its dependents bump
/// with it, so a published version and the requirement it carries stay in step.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UpdatedDependency<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// One planned or applied version bump. Shared by `version plan` and
/// `version apply` so the two never drift.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Bump<'a> {
    pub name: &'a str,
    pub current: &'a str,
    pub next: &'a str,
    /// Fragments the package owns on disk. Zero for a package bumping only
    /// because a dependency did.
    pub fragments: usize,
    /// Empty unless this bump was caused, wholly or partly, by a workspace
    /// dependency bumping in the same plan.
    pub updated_dependencies: Vec<UpdatedDependency<'a>>,
}

/// `trellis version plan --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct VersionPlanDocument<'a> {
    pub schema: &'static str,
    pub bumped: Vec<Bump<'a>>,
    /// True under `--pre <label>`: the fragments behind this release stay
    /// unreleased, and the final version will render them again. False for
    /// every ordinary release and for a `--pre none` promotion.
    pub fragments_retained: bool,
}

impl VersionPlanDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.version_plan/1";
}

/// `trellis version apply --json`. `bumped` repeats `version plan`'s shape;
/// `lockfiles` names the manifests that were rewritten; `adopted` names the
/// version sections captured from pre-existing CHANGELOG.md files.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct VersionApplyDocument<'a> {
    pub schema: &'static str,
    pub bumped: Vec<Bump<'a>>,
    pub lockfiles: Vec<&'a str>,
    pub adopted: Vec<&'a str>,
    /// True under `--pre <label>`: the fragments behind this release were left
    /// in place, so a follow-up release will render them again. A workflow that
    /// gates on "are there unreleased changes" needs this to tell a cut RC from
    /// an incomplete release.
    pub fragments_retained: bool,
}

impl VersionApplyDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.version_apply/1";
}

/// One pending tag in `trellis tag plan --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct PlannedTag<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub tag: &'a str,
    pub kind: crate::commands::tag::TagKind,
    pub action: crate::commands::tag::TagAction,
}

/// `trellis tag plan --json`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TagPlanDocument<'a> {
    pub schema: &'static str,
    pub tags: Vec<PlannedTag<'a>>,
}

impl TagPlanDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.tag_plan/1";
}

/// `trellis ci tag-package --json`.
///
/// A series tag identifies the package but no version, so exactly one of
/// `tag_version` and `tag_series` is present, keyed by `tag_kind`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CiTagPackageDocument<'a> {
    pub schema: &'static str,
    pub name: &'a str,
    pub path: &'a str,
    pub version: &'a str,
    pub tag_kind: crate::commands::tag::TagKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_version: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_series: Option<&'a str>,
}

impl CiTagPackageDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.ci_tag_package/1";
}

/// One job in `trellis ci matrix`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MatrixEntry<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub version: &'a str,
}

/// `trellis ci matrix`.
///
/// **This document has no `schema` field, on purpose.** GitHub Actions feeds it
/// straight to `strategy.matrix` via `fromJSON()`, and every top-level key
/// other than `include` becomes an additional matrix axis — a `schema` sibling
/// would multiply the job matrix by one. The shape is dictated by GitHub, so
/// its stability is GitHub's contract, not ours.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CiMatrix<'a> {
    pub include: Vec<MatrixEntry<'a>>,
}

impl<'a> CiMatrix<'a> {
    pub fn new(workspace: &'a Workspace, selected: &[usize]) -> Self {
        Self {
            include: selected
                .iter()
                .map(|&idx| {
                    let member = &workspace.members[idx];
                    MatrixEntry {
                        name: &member.name,
                        path: &member.rel_path,
                        version: member.version(),
                    }
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Check;

    /// `Check::as_str` is hand-written; serde derives its own name from the
    /// variant. A GitHub annotation's `title=` and the JSON payload's `check`
    /// must be the same string, so assert they are.
    #[test]
    fn check_names_match_serde() {
        for &check in Check::ALL {
            let serialized = serde_json::to_string(&check).unwrap();
            assert_eq!(serialized, format!("\"{}\"", check.as_str()));
        }
    }
}
