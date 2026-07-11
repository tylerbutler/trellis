//! `trellis publish` — per package, in topological order:
//!   1. idempotency check against the Hex API (safe re-runs of a partially
//!      failed release),
//!   2. validation (`gleam format --check`, `build --warnings-as-errors`,
//!      `test`), Hex-touching steps wrapped in the retry policy,
//!   3. path-dep rewrite computed from the graph,
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
    let targets: Vec<usize> = match &options.selector {
        Selector::Package(name) => {
            let idx = workspace
                .member_index(name)
                .with_context(|| format!("unknown package `{name}`"))?;
            if !workspace.members[idx].releasable {
                bail!("package `{name}` is excluded from release by ignore-release");
            }
            vec![idx]
        }
        Selector::Tag(tag) => {
            let (idx, tag_version) = super::tag::resolve_tag(workspace, tag)?;
            let member = &workspace.members[idx];
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
        // Member indices are already topological; publish order follows.
        Selector::AllUntagged => (0..workspace.members.len())
            .filter(|&idx| workspace.members[idx].releasable)
            .collect(),
    };

    let hex = HexClient::from_env();
    let retry = &workspace.config.publish.retry;
    let releasable_versions: BTreeMap<String, String> = workspace
        .members
        .iter()
        .filter(|member| member.releasable)
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
            println!("[{name}] {version} is already on Hex; skipping");
            continue;
        }

        // Compute the rewrite up front — for --dry-run reporting, and so a
        // package that could never publish (path dep on an unreleasable
        // member) fails before validation wastes time.
        let manifest_path = member.path.join("gleam.toml");
        let original = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let (rewritten, rewrites) = rewrite::rewrite_path_deps(
            &original,
            &releasable_versions,
            workspace.config.publish.path_dep_requirement,
        )
        .with_context(|| format!("cannot prepare `{name}` for publishing"))?;

        if options.dry_run {
            println!("[{name}] would publish {version}");
            for rewrite in &rewrites {
                println!("[{name}]   {} -> \"{}\"", rewrite.name, rewrite.requirement);
            }
            continue;
        }

        // 2. Validate against the *original* manifest (path deps intact).
        println!("[{name}] validating {version}");
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
        for rewrite in &rewrites {
            println!(
                "[{name}] rewrote {} -> \"{}\"",
                rewrite.name, rewrite.requirement
            );
        }
        let result = tools::with_retry(retry, &format!("gleam publish for {name}"), || {
            gleam_step(workspace, idx, &["publish", "--yes"])
        });
        drop(guard); // restore gleam.toml before deciding success
        result?;
        println!("[{name}] published {version}");
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
    println!("[{}] $ gleam {}", member.name, args.join(" "));
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
