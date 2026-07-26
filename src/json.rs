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
//! Key naming is kebab-case, matching the config module's deserialize side.
//! `#[serde(rename_all = "kebab-case")]` sits on every struct — a no-op for
//! single-word fields, but it means a later multi-word field cannot silently
//! arrive as snake_case.
//!
//! Two payloads deliberately carry no `schema` field; see [`CiMatrix`] and
//! `commands::ci::outputs`.

use crate::workspace::Workspace;
use serde::Serialize;

/// A workspace member, as `list` and `info` report it.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
pub struct GraphNode<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub path: &'a str,
    pub releasable: bool,
}

/// A dependency edge, pointing from dependent to dependency.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct GraphEdge<'a> {
    pub from: &'a str,
    pub to: &'a str,
}

/// `trellis graph --format json`.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
    pub const SCHEMA: &'static str = "trellis.changelog-check/1";
}

/// One planned or applied version bump. Shared by `version plan` and
/// `version apply` so the two never drift.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Bump<'a> {
    pub name: &'a str,
    pub current: &'a str,
    pub next: &'a str,
    pub fragments: usize,
}

/// `trellis version plan --json`.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct VersionPlanDocument<'a> {
    pub schema: &'static str,
    pub bumped: Vec<Bump<'a>>,
}

impl VersionPlanDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.version-plan/1";
}

/// `trellis version apply --json`. `bumped` repeats `version plan`'s shape;
/// `lockfiles` names the manifests that were rewritten.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct VersionApplyDocument<'a> {
    pub schema: &'static str,
    pub bumped: Vec<Bump<'a>>,
    pub lockfiles: Vec<&'a str>,
}

impl VersionApplyDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.version-apply/1";
}

/// One pending tag in `trellis tag plan --json`.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlannedTag<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub tag: &'a str,
    pub kind: crate::commands::tag::TagKind,
    pub action: crate::commands::tag::TagAction,
}

/// `trellis tag plan --json`.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TagPlanDocument<'a> {
    pub schema: &'static str,
    pub tags: Vec<PlannedTag<'a>>,
}

impl TagPlanDocument<'_> {
    pub const SCHEMA: &'static str = "trellis.tag-plan/1";
}

/// `trellis ci tag-package --json`.
///
/// A series tag identifies the package but no version, so exactly one of
/// `tag-version` and `tag-series` is present, keyed by `tag-kind`.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
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
    pub const SCHEMA: &'static str = "trellis.ci-tag-package/1";
}

/// One job in `trellis ci matrix`.
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
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
#[serde(rename_all = "kebab-case")]
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
