//! `trellis publish` — per package, in topological order:
//!   1. idempotency check against the Hex API (safe re-runs of a partially
//!      failed release),
//!   2. validation (`gleam format --check`, `build --warnings-as-errors`,
//!      `test`), Hex-touching steps wrapped in the retry policy,
//!   3. path-dep rewrite computed from the graph, clearing the resolved
//!      dependency tree the rewrite invalidates,
//!   4. `gleam publish --yes` with retry,
//!   5. restore the original gleam.toml (the repo never shows rewritten
//!      files, even on failure).

use crate::hex::HexClient;
use crate::rewrite;
use crate::tools;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

pub enum Selector {
    /// A single package by name.
    Package(String),
    /// A pushed tag, e.g. `lat_core-v1.2.0`; refuses to publish when the tag
    /// version differs from gleam.toml.
    Tag(String),
    /// Every releasable package whose current version isn't on Hex yet.
    AllUntagged,
}

pub struct PublishOptions {
    pub selector: Selector,
    pub dry_run: bool,
}

pub fn run(workspace: &Workspace, options: &PublishOptions) -> Result<bool> {
    workspace.refuse_under_adapter("publish")?;
    let targets: Vec<usize> = match &options.selector {
        Selector::Package(name) => {
            let idx = workspace
                .member_index(name)
                .with_context(|| format!("unknown package `{name}`"))?;
            let member = &workspace.members[idx];
            if !member.publishes_to_hex() {
                bail!(
                    "package `{name}` has release lifecycle `{}`, not `hex`, so it is never \
                     published",
                    member.lifecycle.key()
                );
            }
            vec![idx]
        }
        Selector::Tag(tag) => match super::tag::resolve_tag(workspace, tag)? {
            super::tag::ResolvedTag::Exact {
                member: idx,
                version: tag_version,
            } => {
                let member = &workspace.members[idx];
                if !member.publishes_to_hex() {
                    bail!(
                        "tag `{tag}` names `{}`, which has release lifecycle `{}`, not `hex`, \
                         so it is never published",
                        member.name,
                        member.lifecycle.key()
                    );
                }
                if tag_version != member.version() {
                    bail!(
                        "tag `{tag}` says version {tag_version} but {}/gleam.toml says {} — \
                         refusing to publish a version that doesn't match its tag",
                        member.rel_path,
                        member.version()
                    );
                }
                vec![idx]
            }
            // A moving tag names a series, not a release; publishing off one
            // would ship whatever the working tree happens to say today.
            super::tag::ResolvedTag::Series {
                member: idx,
                series,
            } => {
                let member = &workspace.members[idx];
                bail!(
                    "tag `{tag}` is the moving v{series} series tag for `{}` and names no \
                     specific version — publish with `--package {}` or `--all-untagged`",
                    member.name,
                    member.name
                );
            }
        },
        // Member indices are already topological; publish order follows.
        Selector::AllUntagged => (0..workspace.members.len())
            .filter(|&idx| workspace.members[idx].publishes_to_hex())
            .collect(),
    };

    let hex = HexClient::from_env();
    let retry = &workspace.config.publish.retry;
    // Path deps are rewritten to Hex requirements derived from the
    // dependency's *current* version — only meaningful for a dependency that
    // will actually exist on Hex, so this map is `hex`-lifecycle members only.
    // A `git_only` or `workspace` runtime path dep has no entry here, and
    // `rewrite_path_deps` refuses to publish a package that needs one.
    let hex_versions: BTreeMap<String, String> = workspace
        .members
        .iter()
        .filter(|member| member.publishes_to_hex())
        .map(|member| (member.name.clone(), member.version().to_string()))
        .collect();

    for idx in targets {
        let member = &workspace.members[idx];
        let name = member.name.clone();
        let version = member.version().to_string();

        // 1. Idempotency: skip what's already on Hex.
        let published = tools::with_retry(retry, &format!("Hex API check for {name}"), || {
            hex.published_versions(&name)
        })?;
        if published.iter().any(|v| v == &version) {
            crate::status!(
                "[{}] {}",
                crate::term::package(&name),
                crate::term::dim(&format!("{version} is already on Hex; skipping"))
            );
            continue;
        }

        // Compute the rewrite up front — for --dry-run reporting, and so a
        // package that could never publish (path dep on a non-hex member)
        // fails before validation wastes time.
        let manifest_path = member.path.join("gleam.toml");
        let original = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let (rewritten, rewrites) = rewrite::rewrite_path_deps(
            &original,
            &hex_versions,
            workspace.config.publish.path_dep_requirement,
        )
        .with_context(|| format!("cannot prepare `{name}` for publishing"))?;

        if options.dry_run {
            crate::status!(
                "[{}] {}",
                crate::term::package(&name),
                crate::term::dim(&format!("would publish {version}"))
            );
            for rewrite in &rewrites {
                crate::status!(
                    "[{}] {}",
                    crate::term::package(&name),
                    crate::term::dim(&format!(
                        "  {} -> \"{}\"",
                        rewrite.name, rewrite.requirement
                    ))
                );
            }
            continue;
        }

        // 2. Validate against the *original* manifest (path deps intact).
        crate::status!("[{}] validating {version}", crate::term::package(&name));
        gleam_step(workspace, idx, &["format", "--check"])?;
        // Build and test resolve deps, which touches Hex — retry those.
        tools::with_retry(retry, &format!("gleam build for {name}"), || {
            gleam_step(workspace, idx, &["build", "--warnings-as-errors"])
        })?;
        tools::with_retry(retry, &format!("gleam test for {name}"), || {
            gleam_step(workspace, idx, &["test"])
        })?;

        // 3–5. Rewrite, publish with retry, restore. The guard restores the
        // original even when publishing fails or panics.
        let guard = RestoreGuard {
            path: manifest_path.clone(),
            original,
        };
        std::fs::write(&manifest_path, &rewritten)
            .with_context(|| format!("failed to write {}", manifest_path.display()))?;
        // Validation resolved `build/packages` while the rewritten deps were
        // still paths, and gleam cannot swap a local dependency for the Hex
        // release of the same name in place: it drops the local entry, then
        // reads the gleam.toml it just removed and fails with a file-IO error.
        // Clearing the resolved tree makes the next resolve start from the
        // rewritten manifest; `build/dev` — the compiled half, and the
        // expensive one — is untouched.
        if !rewrites.is_empty() {
            let resolved = member.path.join("build").join("packages");
            if resolved.exists() {
                std::fs::remove_dir_all(&resolved).with_context(|| {
                    format!("failed to clear resolved deps at {}", resolved.display())
                })?;
            }
        }
        for rewrite in &rewrites {
            crate::status!(
                "[{}] rewrote {} -> \"{}\"",
                crate::term::package(&name),
                rewrite.name,
                rewrite.requirement
            );
        }
        let result = tools::with_retry(retry, &format!("gleam publish for {name}"), || {
            gleam_step(workspace, idx, &["publish", "--yes"])
        });
        drop(guard); // restore gleam.toml before deciding success
        result?;
        crate::status!(
            "[{}] {} {version}",
            crate::term::package(&name),
            crate::term::ok("published")
        );
    }
    Ok(true)
}

struct RestoreGuard {
    path: PathBuf,
    original: String,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if let Err(err) = std::fs::write(&self.path, &self.original) {
            // Failing to restore must be loud: the repo now shows a
            // rewritten gleam.toml.
            eprintln!(
                "error: failed to restore {}: {err} — restore it from git before committing",
                self.path.display()
            );
        }
    }
}

/// Run one gleam step in the member's directory, streaming output to the
/// terminal. Non-zero exit is an error (which the retry wrapper may absorb).
fn gleam_step(workspace: &Workspace, idx: usize, args: &[&str]) -> Result<()> {
    let member = &workspace.members[idx];
    let gleam = tools::gleam_bin();
    crate::status!(
        "[{}] {}",
        crate::term::package(&member.name),
        crate::term::dim(&format!("$ gleam {}", args.join(" ")))
    );
    let status = Command::new(&gleam)
        .args(args)
        .current_dir(&member.path)
        .status()
        .with_context(|| format!("failed to run `{gleam}` — is gleam installed?"))?;
    if !status.success() {
        bail!("`gleam {}` failed for `{}`", args.join(" "), member.name);
    }
    Ok(())
}
