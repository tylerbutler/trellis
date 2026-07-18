//! Git integration: map changed files to the workspace members that own them
//! (`--since <ref>`), and enumerate manifests for member auto-discovery.

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

    Ok(members_owning(workspace, &repo_root, files))
}

/// Indices of members owning any file changed between two committed refs
/// (`base...head`). Unlike [`changed_members`], the working tree is not
/// consulted — this is the primitive for PR checks against explicit SHAs.
pub fn changed_members_between(
    workspace: &Workspace,
    base: &str,
    head: &str,
) -> Result<HashSet<usize>> {
    let repo_root = git_stdout(&workspace.root, &["rev-parse", "--show-toplevel"])
        .context("changelog check requires the workspace to be inside a git repository")?;
    let repo_root = PathBuf::from(repo_root.trim());
    let range = format!("{base}...{head}");
    let files: Vec<String> = lines(&git_stdout(
        &workspace.root,
        &["diff", "--name-only", &range],
    )?)
    .collect();
    Ok(members_owning(workspace, &repo_root, files))
}

fn members_owning(
    workspace: &Workspace,
    repo_root: &Path,
    files: impl IntoIterator<Item = String>,
) -> HashSet<usize> {
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
    changed
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

/// The git repository root containing `dir`, if any. `None` means `dir` is
/// not inside a work tree (or git itself is unavailable).
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    git_stdout(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|out| PathBuf::from(out.trim()))
}

/// Every non-gitignored `gleam.toml` under `cwd` — tracked and untracked
/// alike, so freshly created packages are discovered before their first
/// commit. Paths are relative to `cwd`.
pub fn ls_gleam_manifests(cwd: &Path) -> Result<Vec<String>> {
    // A plain pathspec wildcard matches across `/`, so `*gleam.toml` finds
    // manifests at any depth; the basename filter drops accidental matches
    // like `mygleam.toml`.
    let text = git_stdout(
        cwd,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*gleam.toml",
        ],
    )?;
    Ok(lines(&text)
        .filter(|path| {
            Path::new(path)
                .file_name()
                .is_some_and(|name| name == "gleam.toml")
        })
        .collect())
}

/// `-c user.name=... -c user.email=...` args to prepend to a git command that
/// creates a commit or annotated tag, but only when no identity is
/// configured (CI runners) — never overriding the user's own config.
pub fn identity_fallback_args(cwd: &Path) -> Vec<String> {
    let has_identity = git_stdout(cwd, &["config", "user.email"])
        .map(|email| !email.trim().is_empty())
        .unwrap_or(false);
    if has_identity {
        Vec::new()
    } else {
        vec![
            "-c".into(),
            "user.name=trellis".into(),
            "-c".into(),
            "user.email=trellis@localhost".into(),
        ]
    }
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
