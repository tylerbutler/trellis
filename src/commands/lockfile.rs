//! `trellis lockfile refresh` — scoped `gleam deps download`, encoding the
//! "don't refresh the whole workspace at once or you'll get rate-limited"
//! rule as behavior instead of a workflow comment. Each package's refresh is
//! wrapped in the configured retry policy.

use crate::tools;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::process::Command;

pub fn refresh(workspace: &Workspace, package: Option<&str>) -> Result<bool> {
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
            println!("[{}] $ gleam deps download", member.name);
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
