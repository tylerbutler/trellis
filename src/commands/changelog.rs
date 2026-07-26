//! `trellis changelog` — fragment management on the native engine. `new`
//! writes a fragment; `check` decides which changed packages still need one.

use crate::changelog;
use crate::json::{ChangelogCheckDocument, ChangelogPackage};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};

// ---- new ---------------------------------------------------------------

/// Write an unreleased fragment. Non-interactive by design: `--kind` and
/// `--body` are explicit, which suits CI and agents as well as shells.
pub fn new_fragment(
    workspace: &Workspace,
    package: Option<&str>,
    kind: &str,
    body: &str,
) -> Result<()> {
    let releasable: Vec<&str> = workspace
        .members
        .iter()
        .filter(|m| m.releasable)
        .map(|m| m.name.as_str())
        .collect();
    let project = match package {
        Some(name) => {
            let idx = workspace
                .member_index(name)
                .with_context(|| format!("unknown package `{name}`"))?;
            if !workspace.members[idx].releasable {
                bail!("package `{name}` is excluded from release by `@release`");
            }
            name
        }
        None => match releasable.as_slice() {
            [only] => only,
            _ => bail!(
                "--package is required in a multi-package workspace (releasable: {})",
                releasable.join(", ")
            ),
        },
    };

    let kinds = &workspace.config.changelog.kinds;
    if !kinds.iter().any(|k| k.label == kind) {
        bail!(
            "unknown kind `{kind}`; configured kinds: {}",
            changelog::kind_labels(kinds)
        );
    }
    if body.trim().is_empty() {
        bail!("--body must not be empty");
    }

    let path = changelog::write_fragment(workspace, project, kind, body.trim())?;
    crate::status!(
        "created {}",
        path.strip_prefix(&workspace.root)
            .unwrap_or(&path)
            .display()
    );
    Ok(())
}

// ---- check -------------------------------------------------------------

pub struct CheckOptions {
    pub base: String,
    pub head: String,
    pub json: bool,
}

struct PackageStatus {
    name: String,
    fragments: usize,
}

/// Map the base...head diff to releasable packages and decide which still
/// need a changelog fragment. Returns false (non-zero exit) when any does,
/// or when any fragment is invalid.
pub fn check(workspace: &Workspace, options: &CheckOptions) -> Result<bool> {
    let changed = crate::git::changed_members_between(workspace, &options.base, &options.head)?;
    let fragments = changelog::load_fragments(workspace)?;

    let statuses: Vec<PackageStatus> = workspace
        .members
        .iter()
        .enumerate()
        .filter(|(idx, member)| changed.contains(idx) && member.releasable)
        .map(|(_, member)| PackageStatus {
            name: member.name.clone(),
            fragments: fragments.count_for(&member.name),
        })
        .collect();

    let needs_entry: Vec<&str> = statuses
        .iter()
        .filter(|status| status.fragments == 0)
        .map(|status| status.name.as_str())
        .collect();
    let ok = needs_entry.is_empty() && fragments.problems.is_empty();
    // `invalid-fragments` is a contract field of `trellis.changelog-check/1`
    // and stays an array of strings; the structured form exists for `doctor`.
    let invalid = fragments.problem_messages();

    if options.json {
        let document = ChangelogCheckDocument {
            schema: ChangelogCheckDocument::SCHEMA,
            has_entries: !fragments.fragments.is_empty(),
            needs_entry: !needs_entry.is_empty(),
            invalid_fragments: &invalid,
            packages: statuses
                .iter()
                .map(|status| ChangelogPackage {
                    name: &status.name,
                    changed: true,
                    has_entry: status.fragments > 0,
                    fragments: status.fragments,
                })
                .collect(),
            preview: preview(&statuses, &invalid),
        };
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else {
        if statuses.is_empty() {
            crate::status!(
                "no releasable packages changed between {} and {}",
                options.base,
                options.head
            );
        }
        for status in &statuses {
            let state = if status.fragments > 0 {
                format!("{} fragment(s)", status.fragments)
            } else {
                "needs a changelog entry".to_string()
            };
            crate::status!("{}: {state}", status.name);
        }
        for problem in &invalid {
            crate::status!("invalid: {problem}");
        }
    }
    Ok(ok)
}

/// Markdown summary for the PR sticky comment.
fn preview(statuses: &[PackageStatus], invalid: &[String]) -> String {
    let mut out = String::from("### Changelog check\n\n");
    if statuses.is_empty() {
        out.push_str("No releasable packages changed.\n");
    } else {
        out.push_str("| package | fragments |\n| --- | --- |\n");
        for status in statuses {
            if status.fragments > 0 {
                out.push_str(&format!("| {} | ✅ {} |\n", status.name, status.fragments));
            } else {
                out.push_str(&format!("| {} | ❌ needs an entry |\n", status.name));
            }
        }
        if statuses.iter().any(|status| status.fragments == 0) {
            out.push_str(
                "\nAdd one with `trellis changelog new --package <name> --kind <kind> --body <text>`.\n",
            );
        }
    }
    for problem in invalid {
        out.push_str(&format!("\n⚠️ {problem}\n"));
    }
    out
}
