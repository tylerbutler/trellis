//! Git integration for `--since <ref>`: map changed files to the workspace
//! members that own them.

use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Indices of members owning any file changed since `since`. Includes
/// committed changes (`since...HEAD`), uncommitted changes, and untracked
/// files, so the answer is the same locally and in CI.
pub fn changed_members(workspace: &Workspace, since: &str) -> Result<HashSet<usize>> {
    let repo_root = git_stdout(&workspace.root, &["rev-parse", "--show-toplevel"])
        .context("--since requires the workspace to be inside a git repository")?;
    let repo_root = PathBuf::from(repo_root.trim());

    let mut files: Vec<String> = Vec::new();
    let range = format!("{since}...HEAD");
    files.extend(lines(&git_stdout(
        &workspace.root,
        &["diff", "--name-only", &range],
    )?));
    files.extend(lines(&git_stdout(
        &workspace.root,
        &["diff", "--name-only", "HEAD"],
    )?));
    files.extend(lines(&git_stdout(
        &workspace.root,
        &["ls-files", "--others", "--exclude-standard"],
    )?));

    let mut changed = HashSet::new();
    for file in files {
        // `diff` paths are relative to the repo root; `ls-files` paths are
        // relative to the working directory we invoked git in (the workspace
        // root). Absolute paths make both comparable to member paths.
        let absolute = if Path::new(&file).is_absolute() {
            PathBuf::from(&file)
        } else if repo_root.join(&file).exists() || !workspace.root.join(&file).exists() {
            repo_root.join(&file)
        } else {
            workspace.root.join(&file)
        };
        if let Some(idx) = owning_member(workspace, &absolute) {
            changed.insert(idx);
        }
    }
    Ok(changed)
}

/// The member owning a file: the one whose directory is the longest prefix of
/// the file's path (longest wins, in case members nest).
fn owning_member(workspace: &Workspace, file: &Path) -> Option<usize> {
    workspace
        .members
        .iter()
        .enumerate()
        .filter(|(_, member)| file.starts_with(&member.path))
        .max_by_key(|(_, member)| member.path.components().count())
        .map(|(idx, _)| idx)
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn lines(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}
