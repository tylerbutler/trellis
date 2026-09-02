//! `trellis pin` — the consumer half of series tags: rewrite symbolic git
//! dependency refs to commit SHAs, recording the tracked ref in a
//! `# trellis:pin <ref>` comment on the dependency's own line
//! (ratchet-style). The comment is the record of intent: `--update`
//! re-resolves it, `--unpin` restores it, `--check` verifies the pinned SHA
//! is still reachable from it, and a dependency whose comment is deleted
//! simply stops updating — safe by default.

use crate::workspace::{SelectionFilter, Workspace};
use anyhow::{Context, Result};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use toml_edit::DocumentMut;

/// The comment marker recording a pinned dependency's tracked ref. The
/// trailing space is part of the marker: `# trellis:pin` with no ref after it
/// is not a pin.
const MARKER: &str = "# trellis:pin ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Resolve symbolic refs to SHAs and record the intent.
    Pin,
    /// Re-resolve each recorded intent to its latest SHA.
    Update,
    /// Fail if a pinned SHA is not reachable from its tracked ref.
    Check,
    /// Restore the symbolic refs.
    Unpin,
}

pub struct PinOptions {
    pub mode: Mode,
    /// Explicit package names; empty means all members.
    pub packages: Vec<String>,
}

/// One pinned dependency whose tracked ref no longer contains its SHA (or
/// whose pin comment disagrees with its ref). Shared with `doctor`, which
/// reports these as warnings where `pin --check` exits non-zero.
pub struct Drift {
    pub package: String,
    /// Workspace-relative path of the gleam.toml, forward slashes.
    pub rel_path: String,
    pub message: String,
}

/// A git requirement found in a member's gleam.toml.
#[derive(Debug, PartialEq, Eq)]
struct GitDep {
    section: &'static str,
    name: String,
    url: String,
    git_ref: String,
    /// The tracked ref from a `# trellis:pin` comment, when present.
    pinned: Option<String>,
}

/// What to do to one dependency: set `ref` to `new_ref`, and set the pin
/// comment to track `comment` (`None` strips the marker).
struct RefChange {
    new_ref: String,
    comment: Option<String>,
}

fn is_full_sha(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The tracked ref recorded in a decor suffix, if any. Everything from the
/// marker to the next whitespace; git refnames cannot contain spaces.
fn tracked_ref(suffix: &str) -> Option<&str> {
    let rest = &suffix[suffix.find(MARKER)? + MARKER.len()..];
    rest.split_whitespace().next()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Every git requirement in `[dependencies]` and `[dev-dependencies]`.
/// A `path` key alongside `git` selects a subdirectory of the remote repo
/// (Gleam 1.18+) and changes nothing here; a git requirement without a `ref`
/// is skipped (gleam itself rejects it).
fn scan_git_deps(text: &str) -> Result<Vec<GitDep>> {
    let doc: DocumentMut = text.parse().context("failed to parse gleam.toml")?;
    let mut deps = Vec::new();
    for section in ["dependencies", "dev-dependencies"] {
        let Some(table) = doc.get(section).and_then(|item| item.as_table_like()) else {
            continue;
        };
        for (name, item) in table.iter() {
            let Some(value) = item.as_value() else {
                continue;
            };
            let Some(dep) = value.as_inline_table() else {
                continue;
            };
            let Some(url) = dep.get("git").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(git_ref) = dep.get("ref").and_then(|v| v.as_str()) else {
                continue;
            };
            let pinned = value
                .decor()
                .suffix()
                .and_then(|raw| raw.as_str())
                .and_then(tracked_ref)
                .map(str::to_string);
            deps.push(GitDep {
                section,
                name: name.to_string(),
                url: url.to_string(),
                git_ref: git_ref.to_string(),
                pinned,
            });
        }
    }
    Ok(deps)
}

/// Apply `changes` (keyed by section and dependency name), touching nothing
/// else: the `ref` value keeps its decor, and the pin comment is edited
/// within the line's existing suffix, preserving any unrelated trailing
/// comment in front of it. Returns the new text and whether anything changed.
fn apply_changes(
    text: &str,
    changes: &BTreeMap<(String, String), RefChange>,
) -> Result<(String, bool)> {
    let mut doc: DocumentMut = text.parse().context("failed to parse gleam.toml")?;
    let mut changed = false;
    for ((section, name), change) in changes {
        let Some(value) = doc
            .get_mut(section)
            .and_then(|item| item.as_table_like_mut())
            .and_then(|table| table.get_mut(name))
            .and_then(|item| item.as_value_mut())
        else {
            continue;
        };
        if let Some(reference) = value
            .as_inline_table_mut()
            .and_then(|dep| dep.get_mut("ref"))
            && reference.as_str() != Some(change.new_ref.as_str())
        {
            crate::lockfile::set_str_keep_decor(reference, &change.new_ref);
            changed = true;
        }
        let decor = value.decor_mut();
        let old_suffix = decor
            .suffix()
            .and_then(|raw| raw.as_str())
            .unwrap_or_default()
            .to_string();
        let base = old_suffix
            .find(MARKER)
            .map_or(old_suffix.as_str(), |idx| &old_suffix[..idx])
            .trim_end();
        let new_suffix = match &change.comment {
            Some(tracked) => format!("{base} {MARKER}{tracked}"),
            None => base.to_string(),
        };
        if new_suffix != old_suffix {
            decor.set_suffix(new_suffix);
            changed = true;
        }
    }
    Ok((doc.to_string(), changed))
}

/// One `ls-remote` per unique (url, ref) across the whole run.
fn resolve(
    cache: &mut HashMap<(String, String), Option<String>>,
    root: &Path,
    url: &str,
    refname: &str,
) -> Result<Option<String>> {
    match cache.entry((url.to_string(), refname.to_string())) {
        Entry::Occupied(hit) => Ok(hit.get().clone()),
        Entry::Vacant(miss) => Ok(miss
            .insert(crate::git::ls_remote_commit(root, url, refname)?)
            .clone()),
    }
}

pub fn run(workspace: &Workspace, options: &PinOptions) -> Result<bool> {
    let indices = workspace.select(&SelectionFilter {
        names: options.packages.clone(),
        ..SelectionFilter::default()
    })?;
    if options.mode == Mode::Check {
        let drifts = check_pins(workspace, &indices)?;
        for drift in &drifts {
            crate::status!(
                "{} [{}] {}",
                crate::term::err("drift:"),
                crate::term::package(&drift.package),
                drift.message
            );
        }
        return Ok(drifts.is_empty());
    }

    let mut cache = HashMap::new();
    for &idx in &indices {
        let member = &workspace.members[idx];
        let path = member.path.join("gleam.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut changes = BTreeMap::new();
        let mut commits = BTreeMap::new();
        for dep in
            scan_git_deps(&text).with_context(|| format!("in {}/gleam.toml", member.rel_path))?
        {
            // Each mode decides the verb, the new `ref`, the tracked ref to
            // record (`None` strips the pin comment), and the dimmed detail.
            let (verb, new_ref, comment, detail) = match options.mode {
                Mode::Pin => {
                    if is_full_sha(&dep.git_ref) {
                        continue; // already pinned, or a hand-written SHA
                    }
                    let sha = resolve(&mut cache, &workspace.root, &dep.url, &dep.git_ref)?
                        .with_context(|| {
                            format!("ref `{}` not found on {}", dep.git_ref, dep.url)
                        })?;
                    let detail = format!("tracking {}", dep.git_ref);
                    ("pinned", sha, Some(dep.git_ref), detail)
                }
                Mode::Update => {
                    let Some(tracked) = dep.pinned else {
                        continue; // no recorded intent; nothing to follow
                    };
                    let sha = resolve(&mut cache, &workspace.root, &dep.url, &tracked)?
                        .with_context(|| {
                            format!("tracked ref `{tracked}` not found on {}", dep.url)
                        })?;
                    if sha == dep.git_ref {
                        continue;
                    }
                    let detail = format!("was {}, tracking {tracked}", short(&dep.git_ref));
                    ("updated", sha, Some(tracked), detail)
                }
                Mode::Unpin => {
                    let Some(tracked) = dep.pinned else {
                        continue;
                    };
                    let detail = format!("restored {tracked}");
                    ("unpinned", tracked, None, detail)
                }
                Mode::Check => unreachable!("handled above"),
            };
            // Pinning and updating name the SHA they landed on; unpinning
            // has none to show.
            let sha = if comment.is_some() {
                format!("{} ", short(&new_ref))
            } else {
                String::new()
            };
            crate::status!(
                "[{}] {} {} {sha}{}",
                crate::term::package(&member.name),
                crate::term::ok(verb),
                dep.name,
                crate::term::dim(&detail)
            );
            if comment.is_some() {
                commits.insert(dep.name.clone(), new_ref.clone());
            }
            changes.insert(
                (dep.section.to_string(), dep.name),
                RefChange { new_ref, comment },
            );
        }
        if changes.is_empty() {
            continue;
        }
        let (new_text, changed) = apply_changes(&text, &changes)?;
        if changed {
            std::fs::write(&path, new_text)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
        patch_manifest(member, &commits)?;
    }
    Ok(true)
}

/// After a rewrite the locked commit in manifest.toml is stale; patch it
/// surgically rather than running `gleam update` (same rate-limit posture as
/// `lockfile refresh`). A missing manifest or entry is fine — gleam will lock
/// the pinned commit on the next download.
fn patch_manifest(
    member: &crate::workspace::Member,
    commits: &BTreeMap<String, String>,
) -> Result<()> {
    let path = member.path.join("manifest.toml");
    if commits.is_empty() || !path.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let (new_text, patched) = crate::lockfile::patch_locked_commits(&text, commits)
        .with_context(|| format!("in {}/manifest.toml", member.rel_path))?;
    if !patched.is_empty() {
        std::fs::write(&path, new_text)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(())
}

/// Verify every pinned dependency of the selected members: its tracked ref
/// must still exist, and its SHA must be an ancestor of (or equal to) the
/// commit the ref points at now. A SHA no longer reachable means the ref was
/// force-moved past it — the supply-chain signal this exists to catch.
/// Ancestry needs the commits locally, so refs whose tip moved are fetched
/// (commits only) into the workspace's repository.
pub fn check_pins(workspace: &Workspace, indices: &[usize]) -> Result<Vec<Drift>> {
    let mut cache = HashMap::new();
    let mut fetched: HashSet<(String, String)> = HashSet::new();
    let mut drifts = Vec::new();
    for &idx in indices {
        let member = &workspace.members[idx];
        let path = member.path.join("gleam.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let rel_path = format!("{}/gleam.toml", member.rel_path);
        let mut drift = |message: String| {
            drifts.push(Drift {
                package: member.name.clone(),
                rel_path: rel_path.clone(),
                message,
            });
        };
        for dep in
            scan_git_deps(&text).with_context(|| format!("in {}/gleam.toml", member.rel_path))?
        {
            let Some(tracked) = dep.pinned else {
                continue; // unpinned deps are not tracked
            };
            if !is_full_sha(&dep.git_ref) {
                drift(format!(
                    "`{}` has a `# trellis:pin {tracked}` comment but its ref `{}` is not a \
                     pinned commit SHA (run `trellis pin`)",
                    dep.name, dep.git_ref
                ));
                continue;
            }
            let Some(tip) = resolve(&mut cache, &workspace.root, &dep.url, &tracked)? else {
                drift(format!(
                    "`{}` tracks `{tracked}`, which no longer exists on {}",
                    dep.name, dep.url
                ));
                continue;
            };
            if tip == dep.git_ref {
                continue; // pinned at the tip itself; no fetch needed
            }
            if fetched.insert((dep.url.clone(), tracked.clone())) {
                crate::git::fetch_ref(&workspace.root, &dep.url, &tracked)?;
            }
            if !crate::git::is_ancestor(&workspace.root, &dep.git_ref, &tip)? {
                drift(format!(
                    "`{}` is pinned at {} which is not reachable from its tracked ref \
                     `{tracked}` (now {}) — the ref may have been force-moved",
                    dep.name,
                    short(&dep.git_ref),
                    short(&tip)
                ));
            }
        }
    }
    Ok(drifts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "4f2a9c8144f5c1622fe1e33b21ceca82a67df2ba";

    fn text() -> String {
        concat!(
            "name = \"gp_app\"\n",
            "version = \"0.2.0\"\n",
            "\n",
            "[dependencies]\n",
            "gleam_stdlib = \">= 0.34.0 and < 2.0.0\"\n",
            "gp_core = { path = \"../gp_core\" }\n",
            "remote_lib = { git = \"https://example.com/mono.git\", ref = \"main\", path = \"packages/remote_lib\" }\n",
            "\n",
            "[dev-dependencies]\n",
            "fork = { git = \"https://example.com/fork.git\", ref = \"v2\" } # keep\n",
        )
        .to_string()
    }

    fn change(
        section: &str,
        name: &str,
        new_ref: &str,
        comment: Option<&str>,
    ) -> BTreeMap<(String, String), RefChange> {
        BTreeMap::from([(
            (section.to_string(), name.to_string()),
            RefChange {
                new_ref: new_ref.to_string(),
                comment: comment.map(str::to_string),
            },
        )])
    }

    #[test]
    fn full_sha_is_forty_hex_chars() {
        assert!(is_full_sha(SHA));
        assert!(!is_full_sha("main"));
        assert!(!is_full_sha("4f2a9c8")); // abbreviated
        assert!(!is_full_sha(&format!("{}g", &SHA[..39]))); // non-hex
    }

    #[test]
    fn tracked_ref_grammar() {
        assert_eq!(tracked_ref(" # trellis:pin main"), Some("main"));
        assert_eq!(tracked_ref(" # note # trellis:pin v1 "), Some("v1"));
        assert_eq!(
            tracked_ref(" # trellis:pin lattice_core-v1"),
            Some("lattice_core-v1")
        );
        assert_eq!(tracked_ref(""), None);
        assert_eq!(tracked_ref(" # note"), None);
        assert_eq!(tracked_ref(" # trellis:pin"), None); // marker needs a ref
        assert_eq!(tracked_ref(" # trellis:pin "), None);
    }

    #[test]
    fn scan_finds_git_deps_in_both_sections() {
        let deps = scan_git_deps(&text()).unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "remote_lib");
        assert_eq!(deps[0].section, "dependencies");
        assert_eq!(deps[0].url, "https://example.com/mono.git");
        assert_eq!(deps[0].git_ref, "main");
        assert_eq!(deps[0].pinned, None);
        assert_eq!(deps[1].name, "fork");
        assert_eq!(deps[1].section, "dev-dependencies");
        assert_eq!(deps[1].pinned, None); // "# keep" is not a pin
    }

    #[test]
    fn scan_reads_the_pin_comment() {
        let pinned = text().replace(
            "ref = \"main\", path = \"packages/remote_lib\" }",
            &format!("ref = \"{SHA}\", path = \"packages/remote_lib\" }} # trellis:pin main"),
        );
        let deps = scan_git_deps(&pinned).unwrap();
        assert_eq!(deps[0].git_ref, SHA);
        assert_eq!(deps[0].pinned.as_deref(), Some("main"));
    }

    #[test]
    fn pin_rewrites_ref_appends_comment_and_preserves_everything_else() {
        let (pinned, changed) = apply_changes(
            &text(),
            &change("dependencies", "remote_lib", SHA, Some("main")),
        )
        .unwrap();
        assert!(changed);
        assert_eq!(
            pinned,
            text().replace(
                "remote_lib = { git = \"https://example.com/mono.git\", ref = \"main\", path = \"packages/remote_lib\" }",
                &format!(
                    "remote_lib = {{ git = \"https://example.com/mono.git\", ref = \"{SHA}\", path = \"packages/remote_lib\" }} # trellis:pin main"
                ),
            )
        );
        // Idempotent: applying the same change again touches nothing.
        let (again, changed) = apply_changes(
            &pinned,
            &change("dependencies", "remote_lib", SHA, Some("main")),
        )
        .unwrap();
        assert!(!changed);
        assert_eq!(again, pinned);
    }

    #[test]
    fn pin_preserves_an_existing_trailing_comment() {
        let (pinned, _) = apply_changes(
            &text(),
            &change("dev-dependencies", "fork", SHA, Some("v2")),
        )
        .unwrap();
        assert!(pinned.contains(&format!(
            "fork = {{ git = \"https://example.com/fork.git\", ref = \"{SHA}\" }} # keep # trellis:pin v2"
        )));
        let (unpinned, changed) =
            apply_changes(&pinned, &change("dev-dependencies", "fork", "v2", None)).unwrap();
        assert!(changed);
        assert_eq!(unpinned, text());
    }

    #[test]
    fn unpin_restores_the_original_text() {
        let (pinned, _) = apply_changes(
            &text(),
            &change("dependencies", "remote_lib", SHA, Some("main")),
        )
        .unwrap();
        let (unpinned, changed) =
            apply_changes(&pinned, &change("dependencies", "remote_lib", "main", None)).unwrap();
        assert!(changed);
        assert_eq!(unpinned, text());
    }
}
