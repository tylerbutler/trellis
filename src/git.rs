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

/// Which of `candidates` (absolute paths under `dir`) the branch left exactly
/// as the merge base of `base...head` had them — same path, byte-identical
/// content. Everything else is the branch's own work: added, or edited.
///
/// Comparing content against the merge base rather than reading the
/// `base...head` diff is what makes the answer hold in every mode a fragment
/// can reach the check in — committed on the branch, staged, merely written to
/// the working tree, or an edit to a file the base branch already had. A
/// contributor running the check locally before committing gets the same
/// answer CI will give afterwards.
pub fn unchanged_since_merge_base(
    workspace: &Workspace,
    base: &str,
    head: &str,
    dir: &Path,
    candidates: &[PathBuf],
) -> Result<HashSet<PathBuf>> {
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }
    let repo_root = git_stdout(&workspace.root, &["rev-parse", "--show-toplevel"])
        .context("changelog check requires the workspace to be inside a git repository")?;
    let repo_root = PathBuf::from(repo_root.trim())
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(repo_root.trim()));
    // A `dir` outside the repository has no tracked history to compare
    // against, so every file in it reads as the branch's own.
    let Ok(relative) = dir.strip_prefix(&repo_root) else {
        return Ok(HashSet::new());
    };
    // git pathspecs are `/`-separated on every platform.
    let pathspec = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let merge_base = git_stdout(&workspace.root, &["merge-base", base, head])
        .with_context(|| format!("no merge base between {base} and {head}"))?;

    // `--full-tree` makes both the pathspec and the output relative to the
    // repository root rather than to the directory git was invoked in; `-z`
    // turns off the path quoting that would mangle a non-ASCII filename.
    let listed = git_stdout(
        &workspace.root,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            merge_base.trim(),
            "--",
            &pathspec,
        ],
    )?;
    let mut at_base: std::collections::HashMap<PathBuf, String> = std::collections::HashMap::new();
    for record in listed.split('\0').filter(|record| !record.is_empty()) {
        // "<mode> SP <type> SP <object> TAB <path>"
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        let Some(object) = meta.split_whitespace().nth(2) else {
            continue;
        };
        at_base.insert(repo_root.join(path), object.to_string());
    }
    if at_base.is_empty() {
        return Ok(HashSet::new());
    }

    // Hash the working-tree files the same way git would, so the comparison
    // sees content rather than mtimes or index state.
    let stdin = candidates
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let hashed = git_stdout_with_stdin(&workspace.root, &["hash-object", "--stdin-paths"], &stdin)?;
    let hashes: Vec<String> = lines(&hashed).collect();
    if hashes.len() != candidates.len() {
        bail!(
            "git hash-object returned {} hash(es) for {} file(s)",
            hashes.len(),
            candidates.len()
        );
    }
    Ok(candidates
        .iter()
        .zip(hashes)
        .filter(|(path, hash)| at_base.get(*path).is_some_and(|object| object == hash))
        .map(|(path, _)| path.clone())
        .collect())
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

/// Every non-gitignored member manifest under `cwd` — tracked and untracked
/// alike, so freshly created packages are discovered before their first
/// commit. Paths are relative to `cwd`.
///
/// `rel_path` is the manifest's path within a member directory: `gleam.toml`
/// normally, or whatever `[tools.trellis.adapter].manifest` names, which may
/// carry directories of its own (`.claude-plugin/plugin.json`).
pub fn ls_manifests(cwd: &Path, rel_path: &str) -> Result<Vec<String>> {
    // A plain pathspec wildcard matches across `/`, so `*gleam.toml` finds
    // manifests at any depth; the suffix filter drops accidental matches
    // like `mygleam.toml`.
    let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let text = git_stdout(
        cwd,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            &format!("*{file_name}"),
        ],
    )?;
    Ok(lines(&text)
        .filter(|path| manifest_dir(path, rel_path).is_some())
        .collect())
}

/// The member directory owning `path`, when `path` is exactly `<dir>/<rel_path>`.
/// `""` means the manifest sits at the search root. `None` means `path` merely
/// ends in a similar name (`mygleam.toml`, `other/plugin.json` under a
/// different layout).
pub fn manifest_dir<'a>(path: &'a str, rel_path: &str) -> Option<&'a str> {
    let dir = path.strip_suffix(rel_path)?;
    match dir.strip_suffix('/') {
        Some(dir) => Some(dir),
        None if dir.is_empty() => Some(""),
        None => None,
    }
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
    crate::term::trace_command("git", args, cwd);
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

/// [`git_stdout`] with `stdin` piped in, for the plumbing that reads its input
/// that way rather than from argv.
fn git_stdout_with_stdin(cwd: &Path, args: &[&str], stdin: &str) -> Result<String> {
    use std::io::Write;
    use std::process::Stdio;

    crate::term::trace_command("git", args, cwd);
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run git")?;
    child
        .stdin
        .take()
        .context("git stdin was not piped")?
        .write_all(stdin.as_bytes())
        .context("failed to write to git")?;
    let output = child.wait_with_output().context("failed to run git")?;
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
