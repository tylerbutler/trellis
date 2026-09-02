//! Surgical `manifest.toml` patching: after a version bump, the locked
//! versions of workspace-internal deps must be updated *without* running
//! `gleam update` (which would hit Hex and trip rate limits on shared
//! runners). toml_edit keeps the rest of the file byte-identical.

use crate::workspace::Member;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use toml_edit::{DocumentMut, Value};

#[derive(Debug, PartialEq, Eq)]
pub struct PatchedEntry {
    pub name: String,
    pub old: String,
    pub new: String,
}

/// Replace a string value in place, keeping the whitespace and comments
/// around it so the rest of the line is byte-identical.
pub(crate) fn set_str_keep_decor(value: &mut Value, new: &str) {
    let mut replacement = Value::from(new);
    *replacement.decor_mut() = value.decor().clone();
    *value = replacement;
}

/// Update `packages[].version` for every entry whose name appears in
/// `versions` and whose locked version differs. Returns the new text and what
/// changed; the text is unchanged when nothing needed patching.
pub fn patch_locked_versions(
    text: &str,
    versions: &BTreeMap<String, String>,
) -> Result<(String, Vec<PatchedEntry>)> {
    patch_packages(text, "version", false, versions)
}

/// Update `packages[].commit` for every git-sourced entry whose name appears
/// in `commits` and whose locked commit differs — the lockfile half of
/// `trellis pin`, with the same surgical contract as
/// [`patch_locked_versions`].
pub fn patch_locked_commits(
    text: &str,
    commits: &BTreeMap<String, String>,
) -> Result<(String, Vec<PatchedEntry>)> {
    patch_packages(text, "commit", true, commits)
}

/// Walk `packages` in either shape gleam writes — the inline-array form
/// `packages = [ { name = ..., version = ... }, … ]` or `[[packages]]`
/// tables — and set `key` from `wanted` on every named entry that differs.
fn patch_packages(
    text: &str,
    key: &str,
    git_only: bool,
    wanted: &BTreeMap<String, String>,
) -> Result<(String, Vec<PatchedEntry>)> {
    let mut doc: DocumentMut = text.parse().context("failed to parse manifest.toml")?;
    let mut patched = Vec::new();
    let mut visit = |table: &mut dyn toml_edit::TableLike| {
        let Some(name) = table.get("name").and_then(|item| item.as_str()) else {
            return;
        };
        let Some(new) = wanted.get(name) else {
            return;
        };
        if git_only && table.get("source").and_then(|item| item.as_str()) != Some("git") {
            return;
        }
        let name = name.to_string();
        let Some(value) = table.get_mut(key).and_then(|item| item.as_value_mut()) else {
            return;
        };
        let old = value.as_str().unwrap_or_default().to_string();
        if old != *new {
            set_str_keep_decor(value, new);
            patched.push(PatchedEntry {
                name,
                old,
                new: new.clone(),
            });
        }
    };

    if let Some(array) = doc.get_mut("packages").and_then(|item| item.as_array_mut()) {
        for item in array.iter_mut() {
            if let Some(table) = item.as_inline_table_mut() {
                visit(table);
            }
        }
    } else if let Some(tables) = doc
        .get_mut("packages")
        .and_then(|item| item.as_array_of_tables_mut())
    {
        for table in tables.iter_mut() {
            visit(table);
        }
    }

    Ok((doc.to_string(), patched))
}

/// A member's manifest.toml with its workspace-internal locked versions
/// brought in line with `versions`.
pub struct PatchedLockfile {
    pub path: PathBuf,
    /// Workspace-relative `<member>/manifest.toml`, for messages.
    pub rel_path: String,
    pub text: String,
    pub patched: Vec<PatchedEntry>,
}

/// [`patch_locked_versions`] for one member's manifest.toml. `None` when the
/// member has no lockfile yet or nothing in it needs patching.
pub fn patch_member(
    member: &Member,
    versions: &BTreeMap<String, String>,
) -> Result<Option<PatchedLockfile>> {
    let path = member.path.join("manifest.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let (text, patched) = patch_locked_versions(&text, versions)
        .with_context(|| format!("failed to patch {}", path.display()))?;
    if patched.is_empty() {
        return Ok(None);
    }
    Ok(Some(PatchedLockfile {
        rel_path: format!("{}/manifest.toml", member.rel_path),
        path,
        text,
        patched,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn patches_only_named_packages_and_preserves_layout() {
        let text = concat!(
            "# This file was generated by Gleam\n",
            "# You typically do not need to edit this file\n",
            "packages = [\n",
            "  { name = \"gleam_stdlib\", version = \"0.40.0\", source = \"hex\" },\n",
            "  { name = \"lat_core\", version = \"1.2.0\", source = \"local\", path = \"../lat_core\" },\n",
            "]\n",
            "\n",
            "[requirements]\n",
            "lat_core = { path = \"../lat_core\" }\n",
        );
        let (patched_text, patched) =
            patch_locked_versions(text, &versions(&[("lat_core", "1.3.0")])).unwrap();
        assert_eq!(
            patched,
            vec![PatchedEntry {
                name: "lat_core".to_string(),
                old: "1.2.0".to_string(),
                new: "1.3.0".to_string(),
            }]
        );
        // Only the one version string changed; comments, layout, and the Hex
        // package are untouched.
        assert_eq!(patched_text, text.replace("\"1.2.0\"", "\"1.3.0\""));
    }

    #[test]
    fn no_change_when_versions_already_match() {
        let text = "packages = [ { name = \"a\", version = \"1.0.0\" } ]\n";
        let (patched_text, patched) =
            patch_locked_versions(text, &versions(&[("a", "1.0.0")])).unwrap();
        assert!(patched.is_empty());
        assert_eq!(patched_text, text);
    }

    #[test]
    fn patches_array_of_tables_form() {
        let text = "[[packages]]\nname = \"a\"\nversion = \"1.0.0\"\n";
        let (patched_text, patched) =
            patch_locked_versions(text, &versions(&[("a", "2.0.0")])).unwrap();
        assert_eq!(patched.len(), 1);
        assert!(patched_text.contains("version = \"2.0.0\""));
    }

    #[test]
    fn patches_only_git_sourced_commits() {
        let text = concat!(
            "packages = [\n",
            "  { name = \"dep_a\", version = \"1.0.0\", source = \"git\", repo = \"https://example.com/a\", commit = \"aaaa\" },\n",
            "  { name = \"dep_b\", version = \"1.0.0\", source = \"hex\", outer_checksum = \"aaaa\" },\n",
            "]\n",
        );
        let (patched_text, patched) =
            patch_locked_commits(text, &versions(&[("dep_a", "bbbb"), ("dep_b", "bbbb")])).unwrap();
        assert_eq!(
            patched,
            vec![PatchedEntry {
                name: "dep_a".to_string(),
                old: "aaaa".to_string(),
                new: "bbbb".to_string(),
            }]
        );
        // Only dep_a's commit changed; the hex entry is untouched even though
        // its name was requested.
        assert_eq!(
            patched_text,
            text.replace(
                "repo = \"https://example.com/a\", commit = \"aaaa\"",
                "repo = \"https://example.com/a\", commit = \"bbbb\""
            )
        );
    }

    #[test]
    fn commit_patch_is_idempotent_and_handles_array_of_tables() {
        let inline = "packages = [ { name = \"a\", source = \"git\", commit = \"aaaa\" } ]\n";
        let (text, patched) = patch_locked_commits(inline, &versions(&[("a", "aaaa")])).unwrap();
        assert!(patched.is_empty());
        assert_eq!(text, inline);

        let tables = "[[packages]]\nname = \"a\"\nsource = \"git\"\ncommit = \"aaaa\"\n";
        let (text, patched) = patch_locked_commits(tables, &versions(&[("a", "bbbb")])).unwrap();
        assert_eq!(patched.len(), 1);
        assert!(text.contains("commit = \"bbbb\""));
    }
}
