//! `trellis lockfile refresh` — runs `gleam deps download` scoped to one
//! package at a time, to avoid rate limits from refreshing the whole
//! workspace at once. Each package's refresh is wrapped in the configured
//! retry policy.

use crate::tools;
use crate::workspace::{SelectionFilter, Workspace};
use anyhow::{Context, Result, bail};
use std::process::Command;

pub fn refresh(workspace: &Workspace, package: Option<&str>) -> Result<bool> {
    let targets = workspace.select(&SelectionFilter {
        names: package.map(str::to_string).into_iter().collect(),
        ..SelectionFilter::default()
    })?;

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
