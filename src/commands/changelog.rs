//! `trellis changelog` — fragment management on the native engine. `new`
//! writes a fragment; `check` decides which changed packages still need one.

use crate::changelog;
use crate::config::Strictness;
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
    category: Option<&str>,
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
                bail!(
                    "package `{name}` has release lifecycle `{}`, so it never gets a changelog \
                     entry",
                    workspace.members[idx].lifecycle.key()
                );
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
    let categories = &workspace.config.changelog.categories;
    if let Some(category) = category
        && !categories.iter().any(|c| c == category)
    {
        // Mirrors `load_fragments`: with none configured, name the key that
        // turns the axis on rather than matching against an empty list.
        if categories.is_empty() {
            bail!(
                "no `categories` are configured; add them under \
                 [tools.trellis.changelog] to group entries by category"
            );
        }
        bail!(
            "unknown category `{category}`; configured categories: {}",
            changelog::category_labels(categories)
        );
    }
    if body.trim().is_empty() {
        bail!("--body must not be empty");
    }

    let path = changelog::write_fragment(workspace, project, kind, category, body.trim())?;
    crate::status!(
        "created {}",
        path.strip_prefix(&workspace.root)
            .unwrap_or(&path)
            .display()
    );
    Ok(())
}

// ---- check -------------------------------------------------------------

/// How `check` reports. `text` is for a person; the other two are the machine
/// surfaces CI consumes. Mirrors `doctor`'s `--format`, which is the shape a
/// second structured output always wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum CheckFormat {
    #[default]
    Text,
    /// The `trellis.changelog_check/1` payload.
    Json,
    /// `key=value` lines for `$GITHUB_OUTPUT`, so a workflow can post, update,
    /// or delete a PR comment without a `jq` pipeline.
    Github,
}

pub struct CheckOptions {
    pub base: String,
    pub head: String,
    pub format: CheckFormat,
    /// Overrides the workspace's `changelog.strictness` for this run.
    pub strictness: Option<Strictness>,
}

struct PackageStatus {
    name: String,
    fragments: usize,
}

/// Map the base...head diff to releasable packages and decide which still
/// need a changelog fragment. Returns false (non-zero exit) when one does and
/// strictness is `error`, or when any fragment is invalid.
pub fn check(workspace: &Workspace, options: &CheckOptions) -> Result<bool> {
    let strictness = options
        .strictness
        .unwrap_or(workspace.config.changelog.strictness);
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

    // Under `off` the diff is still mapped and reported — the rows are useful
    // on their own — but nothing is *asked* of the contributor, so there is no
    // verdict to carry into the payload, the preview, or the exit code.
    let needs_entry: Vec<&str> = match strictness {
        Strictness::Off => Vec::new(),
        Strictness::Warn | Strictness::Error => statuses
            .iter()
            .filter(|status| status.fragments == 0)
            .map(|status| status.name.as_str())
            .collect(),
    };
    // A fragment that does not parse is malformed input, not a judgment call,
    // so it fails at every strictness.
    let ok = (strictness != Strictness::Error || needs_entry.is_empty())
        && fragments.problems.is_empty();
    // `invalid_fragments` is a contract field of `trellis.changelog_check/1`
    // and stays an array of strings; the structured form exists for `doctor`.
    let invalid = fragments.problem_messages();
    let has_entries = !fragments.fragments.is_empty();
    let preview = preview(&statuses, &needs_entry, &invalid, strictness);

    match options.format {
        CheckFormat::Json => {
            let document = ChangelogCheckDocument {
                schema: ChangelogCheckDocument::SCHEMA,
                ok,
                strictness,
                has_entries,
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
                preview,
            };
            println!("{}", serde_json::to_string_pretty(&document)?);
        }
        CheckFormat::Github => print_github_outputs(
            ok,
            strictness,
            has_entries,
            &needs_entry,
            &invalid,
            &preview,
        )?,
        CheckFormat::Text => {
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
                } else if !needs_entry.contains(&status.name.as_str()) {
                    // `off`: report the fact, ask for nothing.
                    "no entries".to_string()
                } else if strictness == Strictness::Warn {
                    // Named as advisory, so a green exit code doesn't read as
                    // the line having been ignored.
                    "needs a changelog entry (warning)".to_string()
                } else {
                    "needs a changelog entry".to_string()
                };
                crate::status!("{}: {state}", status.name);
            }
            for problem in &invalid {
                crate::status!("invalid: {problem}");
            }
        }
    }
    Ok(ok)
}

/// Emit `key=value` lines for `$GITHUB_OUTPUT`, in the shape `ci outputs`
/// established: arrays are JSON, so a workflow reads them with `fromJSON()`.
fn print_github_outputs(
    ok: bool,
    strictness: Strictness,
    has_entries: bool,
    needs_entry: &[&str],
    invalid: &[String],
    preview: &str,
) -> Result<()> {
    println!("ok={ok}");
    println!(
        "strictness={}",
        match strictness {
            Strictness::Warn => "warn",
            Strictness::Error => "error",
            Strictness::Off => "off",
        }
    );
    println!("has_entries={has_entries}");
    println!("needs_entry={}", !needs_entry.is_empty());
    println!(
        "needs_entry_packages={}",
        serde_json::to_string(needs_entry)?
    );
    println!("invalid_fragments={}", serde_json::to_string(invalid)?);
    // The comment body is markdown, so it needs GitHub's heredoc form.
    let delimiter = heredoc_delimiter(preview);
    println!("preview<<{delimiter}");
    print!("{preview}");
    if !preview.ends_with('\n') {
        println!();
    }
    println!("{delimiter}");
    Ok(())
}

/// A delimiter the value cannot contain. GitHub ends the block at the first
/// line equal to it, so a value carrying the delimiter would truncate the
/// output — and let whatever follows be read as further `key=value` lines.
fn heredoc_delimiter(value: &str) -> String {
    let mut delimiter = String::from("TRELLIS_PREVIEW");
    while value.lines().any(|line| line == delimiter) {
        delimiter.push('_');
    }
    delimiter
}

/// Markdown summary for the PR sticky comment. `needs_entry` is passed rather
/// than re-derived so the comment says exactly what the exit code did — under
/// `off` there is no ❌ and no call to action.
fn preview(
    statuses: &[PackageStatus],
    needs_entry: &[&str],
    invalid: &[String],
    strictness: Strictness,
) -> String {
    let mut out = String::from("### Changelog check\n\n");
    if statuses.is_empty() {
        out.push_str("No releasable packages changed.\n");
    } else {
        out.push_str("| package | fragments |\n| --- | --- |\n");
        for status in statuses {
            let cell = if status.fragments > 0 {
                format!("✅ {}", status.fragments)
            } else if !needs_entry.contains(&status.name.as_str()) {
                "— none".to_string()
            } else if strictness == Strictness::Warn {
                "⚠️ no entry".to_string()
            } else {
                "❌ needs an entry".to_string()
            };
            out.push_str(&format!("| {} | {cell} |\n", status.name));
        }
        if !needs_entry.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_heredoc_delimiter_avoids_the_value_it_wraps() {
        assert_eq!(
            heredoc_delimiter("### Changelog check\n"),
            "TRELLIS_PREVIEW"
        );
        // A fragment body quoting the delimiter would otherwise end the block
        // early and let the rest be read as further `key=value` lines.
        assert_eq!(
            heredoc_delimiter("a\nTRELLIS_PREVIEW\nb\n"),
            "TRELLIS_PREVIEW_"
        );
        assert_eq!(
            heredoc_delimiter("TRELLIS_PREVIEW\nTRELLIS_PREVIEW_\n"),
            "TRELLIS_PREVIEW__"
        );
        // Only a whole line ends a block, so a mention mid-line is harmless.
        assert_eq!(
            heredoc_delimiter("see TRELLIS_PREVIEW here\n"),
            "TRELLIS_PREVIEW"
        );
    }
}
