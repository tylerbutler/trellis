//! `trellis init` — bootstrap a workspace.
//!
//! `trellis new` scaffolds a *member*; nothing scaffolded the workspace, so
//! adopting trellis in an existing repo meant hand-writing `[tools.trellis]`
//! from the reference page. Configless mode softens that — a repo with no
//! config works — but the moment one option is needed the user is back at the
//! docs.
//!
//! The output is deliberately almost empty. Everything trellis can derive, it
//! derives, so the only thing `init` must write is the table itself: its
//! presence is what marks the workspace root. The comments it leaves behind
//! point at what *could* be configured, which is the part a reference page is
//! bad at answering.

use crate::config::has_trellis_table;
use crate::git;
use crate::workspace::{self, GLEAM_TOML};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The comment block written above the table. It names the two things a reader
/// most needs: that membership is derived, and where the rest is documented.
const PREAMBLE: &str = "\
# Workspace configuration for trellis. This table's presence is what marks the
# workspace root; every key in it is optional.
#
# Members are auto-discovered — every gleam.toml git knows about, outside
# build/. Add `members = [\"packages/*\"]` only to pin them to something
# narrower than that.
#
# Task exclusions, custom tasks, tag formats, and changelog kinds are all
# configured here too: https://trellis.tylerbutler.com/docs/configuration/
";

pub fn run(start: &Path) -> Result<bool> {
    let root = git::repo_root(start).with_context(|| {
        format!(
            "`trellis init` needs a git repository: {} is not inside one, and member \
             discovery reads from git",
            start.display()
        )
    })?;

    refuse_if_already_a_workspace(&root)?;

    let manifest_path = root.join(GLEAM_TOML);
    let existing = std::fs::read_to_string(&manifest_path).ok();
    let document = render_config(existing.as_deref())?;
    std::fs::write(&manifest_path, document)
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    crate::status!(
        "{} {}",
        if existing.is_some() {
            "added [tools.trellis] to"
        } else {
            // A root that is not itself a package still needs a manifest to
            // carry the table; a config-only gleam.toml is a supported shape.
            "created"
        },
        manifest_path.display()
    );

    // Reported rather than written: seeing the derived list is what tells the
    // user whether it needs narrowing, and printing it costs nothing.
    let members = workspace::discovered_member_paths(&root);
    if members.is_empty() {
        crate::status!("no packages discovered yet — doctor will say so below");
    } else {
        crate::status!("members are auto-discovered; found {}:", members.len());
        for member in &members {
            crate::status!("  {member}");
        }
    }
    crate::status!();

    // The issue asks for this, and it is the honest close: init's job is done
    // when doctor agrees the workspace is coherent.
    crate::commands::doctor::run(&root, &crate::commands::doctor::DoctorOptions::default())
}

/// Refuse rather than merge into an existing setup. Two `[tools.trellis]`
/// tables is not a configuration trellis has semantics for — root discovery
/// stops at the first one it walks up to — so the useful move is to say where
/// the existing one is.
fn refuse_if_already_a_workspace(root: &Path) -> Result<()> {
    if let Some(found) = trellis_table_at_or_above(root) {
        bail!(
            "{} already has a [tools.trellis] table; this repository is already a \
             trellis workspace",
            found.display()
        );
    }
    for member in workspace::discovered_member_paths(root) {
        let path = root.join(&member).join(GLEAM_TOML);
        if reads_trellis_table(&path) {
            bail!(
                "{} has a [tools.trellis] table; a member manifest cannot carry one \
                 (it would hijack workspace-root discovery). Remove it, then rerun \
                 `trellis init`",
                path.display()
            );
        }
    }
    Ok(())
}

/// The nearest `gleam.toml` at or above `root` carrying the table. Ancestors
/// count: initializing inside an existing workspace would nest one workspace in
/// another, which trellis has no notion of.
fn trellis_table_at_or_above(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .map(|dir| dir.join(GLEAM_TOML))
        .find(|manifest| reads_trellis_table(manifest))
}

fn reads_trellis_table(manifest: &Path) -> bool {
    std::fs::read_to_string(manifest)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .is_some_and(|document| has_trellis_table(&document))
}

/// Add `[tools.trellis]` to an existing manifest, or compose a config-only one.
///
/// `toml_edit` rather than a format string so an existing manifest keeps its
/// formatting, comments, and key order — the same surgical-edit rule
/// `version apply` follows for the `version` key.
fn render_config(existing: Option<&str>) -> Result<String> {
    let mut document: toml_edit::DocumentMut = match existing {
        Some(text) => text
            .parse()
            .context("failed to parse the existing gleam.toml")?,
        None => toml_edit::DocumentMut::new(),
    };

    let mut trellis = toml_edit::Table::new();
    trellis.decor_mut().set_prefix(format!(
        "{}{PREAMBLE}",
        if existing.is_some() { "\n" } else { "" }
    ));

    // Built explicitly rather than through `document["tools"]["trellis"]`,
    // which auto-vivifies `tools` as an *inline* table and renders `tools = {}`
    // — valid TOML that `has_trellis_table` correctly refuses to recognize.
    let tools = document.entry("tools").or_insert_with(|| {
        let mut table = toml_edit::Table::new();
        // Otherwise `tools` gets a `[tools]` header of its own above the one
        // that matters.
        table.set_implicit(true);
        toml_edit::Item::Table(table)
    });
    let tools = tools.as_table_mut().context(
        "`tools` in gleam.toml is an inline table; add `[tools.trellis]` by hand \
         to keep its formatting",
    )?;
    tools.insert("trellis", toml_edit::Item::Table(trellis));
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_only_manifest_is_just_the_table() {
        let rendered = render_config(None).unwrap();
        assert!(rendered.contains("[tools.trellis]"), "{rendered}");
        // No `[tools]` header of its own, which would be noise.
        assert!(!rendered.contains("\n[tools]"), "{rendered}");
        // Nothing derivable is declared. The comment block mentions `members`,
        // so this looks for an actual key rather than the word.
        let declared: Vec<&str> = rendered
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#') && !line.is_empty() && !line.starts_with('['))
            .collect();
        assert!(declared.is_empty(), "expected no keys, got {declared:?}");
        assert!(rendered.starts_with('#'), "{rendered}");
    }

    #[test]
    fn an_existing_manifest_keeps_its_contents() {
        let existing = "name = \"app\"\nversion = \"1.0.0\"\n\n\
                        [dependencies]\ngleam_stdlib = \">= 0.44.0\"\n";
        let rendered = render_config(Some(existing)).unwrap();
        assert!(rendered.starts_with(existing), "{rendered}");
        assert!(rendered.contains("[tools.trellis]"), "{rendered}");
    }

    #[test]
    fn the_rendered_table_is_what_root_discovery_looks_for() {
        // The whole point of the file: `has_trellis_table` must agree that this
        // marks a workspace root.
        for existing in [None, Some("name = \"app\"\nversion = \"1.0.0\"\n")] {
            let rendered = render_config(existing).unwrap();
            let parsed: toml::Value = toml::from_str(&rendered).unwrap();
            assert!(has_trellis_table(&parsed), "{rendered}");
        }
    }
}
