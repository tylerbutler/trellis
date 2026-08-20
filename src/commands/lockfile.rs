//! `trellis lockfile refresh` — runs `gleam deps download` scoped to one
//! package at a time, to avoid rate limits from refreshing the whole
//! workspace at once. Each package's refresh is wrapped in the configured
//! retry policy.

use crate::tools;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::process::Command;

pub fn refresh(workspace: &Workspace, package: Option<&str>) -> Result<bool> {
    workspace.refuse_under_adapter("lockfile refresh")?;
    let targets: Vec<usize> = match package {
        Some(name) => {
            let idx = workspace
                .member_index(name)
                .with_context(|| format!("unknown package `{name}`"))?;
            vec![idx]
        }
        None => (0..workspace.members.len()).collect(),
    };

    let retry = &workspace.config.publish.retry;
    for idx in targets {
        let member = &workspace.members[idx];
        tools::with_retry(retry, &format!("deps download for {}", member.name), || {
            let gleam = tools::gleam_bin();
            crate::status!(
                "[{}] {}",
                crate::term::package(&member.name),
                crate::term::dim("$ gleam deps download")
            );
            let status = Command::new(&gleam)
                .args(["deps", "download"])
                .current_dir(&member.path)
                .status()
                .with_context(|| format!("failed to run `{gleam}` — is gleam installed?"))?;
            if !status.success() {
                bail!("`gleam deps download` failed for `{}`", member.name);
            }
            Ok(())
        })?;
    }
    Ok(true)
}
