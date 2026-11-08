//! Git integration: map changed files to the workspace members that own them
//! (`--since <ref>`), and enumerate manifests for member auto-discovery.

use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Indices of members owning any file changed since `since`. Includes
/// committed changes (`since...HEAD`), uncommitted changes, and untracked
/// files, so the answer is the same locally and in CI.
pub fn changed_members(workspace: &Workspace, since: &str) -> Result<HashSet<usize>> {
    let repo_root = repo_root(&workspace.root)
        .context("--since requires the workspace to be inside a git repository")?;

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
    let repo_root = repo_root(&workspace.root)
        .context("changelog check requires the workspace to be inside a git repository")?;
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
    let repo_root = repo_root(&workspace.root)
        .context("changelog check requires the workspace to be inside a git repository")?;
    let repo_root = repo_root.canonicalize().unwrap_or(repo_root);
    // A `dir` outside the repository has no tracked history to compare
    // against, so every file in it reads as the branch's own.
    let Ok(relative) = dir.strip_prefix(&repo_root) else {
        return Ok(HashSet::new());
    };
    // git pathspecs are `/`-separated on every platform.
    let pathspec = slash_path(relative);
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

/// `path`'s components joined by `/` regardless of platform — the form git
/// pathspecs and trellis's own `rel_path` displays use.
pub fn slash_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
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
fn identity_fallback_args(cwd: &Path) -> Vec<String> {
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

/// Run a git command that writes a commit or annotated tag, with the identity
/// fallback prepended when the user has none configured.
pub(crate) fn git_with_identity(cwd: &Path, args: &[&str]) -> Result<String> {
    let identity = identity_fallback_args(cwd);
    let full: Vec<&str> = identity
        .iter()
        .map(String::as_str)
        .chain(args.iter().copied())
        .collect();
    git_stdout(cwd, &full)
}

/// The commit a ref names on `url`, from one `ls-remote` — `None` when the
/// remote has no such ref. Annotated tags resolve to the peeled commit (the
/// object gleam locks), not the tag object. A name matching several refs
/// with different commits (say a branch and a tag) is an error.
pub fn ls_remote_commit(cwd: &Path, url: &str, refname: &str) -> Result<Option<String>> {
    // The peeled pattern is queried explicitly: ls-remote only prints the
    // `^{}` line for an annotated tag when a pattern matches it.
    let peeled = format!("{refname}^{{}}");
    let output = git_output(cwd, &["ls-remote", "--exit-code", url, refname, &peeled])?;
    match output.status.code() {
        Some(0) => {}
        Some(2) => return Ok(None),
        _ => bail!(
            "git ls-remote failed for `{refname}` on {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
    // "<oid>\t<ref>" lines; an annotated tag contributes a second
    // "<oid>\t<ref>^{}" line carrying the peeled commit, which wins.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits: std::collections::BTreeMap<String, String> = Default::default();
    for line in stdout.lines() {
        let Some((oid, reference)) = line.split_once('\t') else {
            continue;
        };
        match reference.strip_suffix("^{}") {
            Some(base) => {
                commits.insert(base.to_string(), oid.to_string());
            }
            None => {
                commits
                    .entry(reference.to_string())
                    .or_insert_with(|| oid.to_string());
            }
        }
    }
    let distinct: std::collections::BTreeSet<&String> = commits.values().collect();
    match distinct.len() {
        0 => Ok(None),
        1 => Ok(distinct.into_iter().next().cloned()),
        _ => bail!(
            "`{refname}` is ambiguous on {url}: it matches {} — use a full refname",
            commits.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Fetch a ref's history (commits only) from `url` into the local repository
/// so ancestry can be tested. `--filter=tree:0` keeps it cheap; a server that
/// rejects filters gets one plain retry.
pub fn fetch_ref(cwd: &Path, url: &str, refname: &str) -> Result<()> {
    let filtered = [
        "fetch",
        "--quiet",
        "--no-tags",
        "--filter=tree:0",
        url,
        refname,
    ];
    if git_stdout(cwd, &filtered).is_ok() {
        return Ok(());
    }
    git_stdout(cwd, &["fetch", "--quiet", "--no-tags", url, refname]).map(|_| ())
}

/// Whether `ancestor` is an ancestor of (or equal to) `descendant` among
/// locally-known objects. A commit git doesn't have counts as "no": after
/// fetching a tracked ref, a pin absent from its history is exactly the
/// drift being tested for.
pub fn is_ancestor(cwd: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = git_output(cwd, &["merge-base", "--is-ancestor", ancestor, descendant])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let lower = stderr.to_lowercase();
            if lower.contains("not a valid")
                || lower.contains("bad object")
                || lower.contains("bad revision")
            {
                Ok(false)
            } else {
                bail!("git merge-base --is-ancestor failed: {}", stderr.trim())
            }
        }
    }
}

/// Run git in `cwd` and return the raw output, whatever the exit status.
/// Shared command construction keeps tracing and missing-Git errors uniform.
pub(crate) fn git_output(cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
    git_command(cwd, args).output().context("failed to run git")
}

fn git_command(cwd: &Path, args: &[&str]) -> Command {
    crate::term::trace_command("git", args, cwd);
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    command
}

/// [`git_output`] that fails on a non-zero exit, returning stdout.
pub(crate) fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(cwd, args)?;
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

    let mut child = git_command(cwd, args)
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

#[derive(Debug, Clone)]
pub struct FragmentCommit {
    pub commit: String,
    pub subject: String,
}

/// Walk all parents until the current file lifetime's addition. Renames use
/// Git's 50% similarity threshold; divergent origins and shallow boundaries
/// are unresolved. Cached commit records and parent diffs are shared by files.
pub struct FragmentHistory {
    root: PathBuf,
    head: String,
    shallow: HashSet<String>,
    commits: HashMap<String, (Vec<String>, String)>,
    changes: HashMap<(String, String), HashMap<String, Option<String>>>,
}

impl FragmentHistory {
    pub fn new(cwd: &Path, reference: &str) -> Result<Option<Self>> {
        let output = local_git_output(cwd, &["rev-parse", "--show-toplevel"])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not a git repository") {
                return Ok(None);
            }
            bail!("cannot find fragment repository: {}", stderr.trim());
        }
        let root = PathBuf::from(String::from_utf8(output.stdout)?.trim()).canonicalize()?;
        let output = local_git_output(
            &root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{reference}^{{commit}}"),
            ],
        )?;
        if !output.status.success() {
            if reference == "HEAD" && output.status.code() == Some(1) {
                return Ok(None);
            }
            bail!(
                "cannot resolve fragment source ref `{reference}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let head = String::from_utf8(output.stdout)?.trim().to_string();
        let shallow_path = local_git_stdout(&root, &["rev-parse", "--git-path", "shallow"])?;
        let shallow = match std::fs::read_to_string(root.join(shallow_path.trim())) {
            Ok(text) => lines(&text).collect(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(err) => return Err(err).context("cannot read shallow history boundary"),
        };
        Ok(Some(Self {
            root,
            head,
            shallow,
            commits: HashMap::new(),
            changes: HashMap::new(),
        }))
    }

    pub fn source(&mut self, path: &Path) -> Result<Option<FragmentCommit>> {
        let path = path
            .parent()
            .context("fragment has no parent directory")?
            .canonicalize()?
            .join(path.file_name().context("fragment has no filename")?);
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Ok(None);
        };
        let path = slash_path(relative);
        let tree = local_git_stdout(
            &self.root,
            &[
                "ls-tree",
                "-r",
                "-z",
                "--full-tree",
                &self.head,
                "--",
                &path,
            ],
        )?;
        if !tree.split('\0').any(|record| {
            record
                .split_once('\t')
                .is_some_and(|(_, name)| name == path)
        }) {
            return Ok(None);
        }
        let mut pending = vec![(self.head.clone(), path)];
        let mut visited = HashSet::new();
        let mut additions = HashSet::new();
        while let Some((commit, path)) = pending.pop() {
            if !visited.insert((commit.clone(), path.clone())) {
                continue;
            }
            if self.shallow.contains(&commit) {
                return Ok(None);
            }
            if !self.commits.contains_key(&commit) {
                let text =
                    local_git_stdout(&self.root, &["show", "-s", "--format=%P%x00%s", &commit])?;
                let (parents, subject) = text
                    .trim_end_matches('\n')
                    .split_once('\0')
                    .context("invalid Git fragment commit record")?;
                self.commits.insert(
                    commit.clone(),
                    (
                        parents.split_whitespace().map(str::to_string).collect(),
                        subject.to_string(),
                    ),
                );
            }
            let parents = self.commits[&commit].0.clone();
            let mut inherited = false;
            for parent in parents {
                let key = (parent.clone(), commit.clone());
                if !self.changes.contains_key(&key) {
                    let text = local_git_stdout(
                        &self.root,
                        &[
                            "diff-tree",
                            "-r",
                            "--no-ext-diff",
                            "--no-textconv",
                            "-M50%",
                            "--name-status",
                            "-z",
                            &parent,
                            &commit,
                            "--",
                        ],
                    )?;
                    let mut fields = text.split('\0').filter(|field| !field.is_empty());
                    let mut changes = HashMap::new();
                    while let Some(status) = fields.next() {
                        let old = fields.next().context("missing Git change path")?;
                        let (name, previous) = if status.starts_with('R') {
                            (
                                fields.next().context("missing Git rename destination")?,
                                Some(old.to_string()),
                            )
                        } else {
                            (
                                old,
                                if status == "A" {
                                    None
                                } else {
                                    Some(old.to_string())
                                },
                            )
                        };
                        changes.insert(name.to_string(), previous);
                    }
                    self.changes.insert(key.clone(), changes);
                }
                match self.changes[&key].get(&path) {
                    Some(None) => {}
                    previous => {
                        let previous = previous
                            .and_then(|p| p.clone())
                            .unwrap_or_else(|| path.clone());
                        pending.push((parent, previous));
                        inherited = true;
                    }
                }
            }
            if !inherited {
                additions.insert(commit);
            }
            if additions.len() > 1 {
                return Ok(None);
            }
        }
        Ok(additions.into_iter().next().map(|commit| FragmentCommit {
            subject: self.commits[&commit].1.clone(),
            commit,
        }))
    }
}

fn local_git_output(cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
    git_command(cwd, args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("LC_ALL", "C")
        .output()
        .context("failed to run local git")
}

fn local_git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = local_git_output(cwd, args)?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod source_tests {
    use super::*;

    fn run(root: &Path, args: &[&str]) -> String {
        git_with_identity(root, args).unwrap()
    }

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q", "-b", "main"]);
        dir
    }

    fn commit(root: &Path, subject: &str) -> String {
        run(root, &["add", "."]);
        run(
            root,
            &["commit", "-q", "-m", subject, "-m", "Body only #999"],
        );
        run(root, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn source(root: &Path, path: &str) -> Option<FragmentCommit> {
        FragmentHistory::new(root, "HEAD")
            .unwrap()
            .unwrap()
            .source(&root.join(path))
            .unwrap()
    }

    #[test]
    fn fragment_source_follows_edits_renames_and_current_lifetime() {
        let dir = repository();
        let root = dir.path();
        std::fs::write(root.join("old.toml"), "first fragment\n").unwrap();
        let first = commit(root, "Add #42");
        assert_eq!(source(root, "old.toml").unwrap().commit, first);
        std::fs::write(root.join("old.toml"), "edited fragment\n").unwrap();
        commit(root, "Edit #43");
        std::fs::write(root.join("old.toml"), "local edit\n").unwrap();
        assert_eq!(source(root, "old.toml").unwrap().commit, first);
        run(root, &["checkout", "--", "old.toml"]);
        run(root, &["mv", "old.toml", "renamed.toml"]);
        commit(root, "Rename #44");
        let found = source(root, "renamed.toml").unwrap();
        assert_eq!(found.commit, first);
        assert_eq!(found.subject, "Add #42");
        run(root, &["rm", "renamed.toml"]);
        commit(root, "Delete");
        std::fs::write(root.join("renamed.toml"), "second lifetime\n").unwrap();
        let second = commit(root, "Recreate #45");
        assert_eq!(source(root, "renamed.toml").unwrap().commit, second);
        std::fs::write(root.join("new.toml"), "untracked\n").unwrap();
        assert!(source(root, "new.toml").is_none());
    }

    #[test]
    fn fragment_source_handles_merges_squashes_and_ambiguous_additions() {
        for squash in [false, true] {
            let dir = repository();
            let root = dir.path();
            std::fs::write(root.join("base"), "base\n").unwrap();
            commit(root, "Base");
            run(root, &["checkout", "-q", "-b", "feature"]);
            std::fs::write(root.join("fragment.toml"), "fragment\n").unwrap();
            let added = commit(root, "Feature #42");
            run(root, &["checkout", "-q", "main"]);
            let expected = if squash {
                run(root, &["merge", "--squash", "feature"]);
                commit(root, "Squash #43")
            } else {
                run(root, &["merge", "--no-ff", "-m", "Merge #43", "feature"]);
                added
            };
            assert_eq!(source(root, "fragment.toml").unwrap().commit, expected);
        }
        let dir = repository();
        let root = dir.path();
        std::fs::write(root.join("base"), "base\n").unwrap();
        commit(root, "Base");
        run(root, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(root.join("fragment.toml"), "same\n").unwrap();
        commit(root, "One source");
        run(root, &["checkout", "-q", "main"]);
        std::fs::write(root.join("fragment.toml"), "same\n").unwrap();
        commit(root, "Another source");
        run(root, &["merge", "--no-ff", "-m", "Merge", "feature"]);
        assert!(source(root, "fragment.toml").is_none());
    }

    #[test]
    fn fragment_source_does_not_attribute_a_shallow_boundary() {
        let origin = repository();
        std::fs::write(origin.path().join("fragment.toml"), "original\n").unwrap();
        let original = commit(origin.path(), "Original");
        std::fs::write(origin.path().join("fragment.toml"), "edited\n").unwrap();
        commit(origin.path(), "Later edit");
        let clone = tempfile::tempdir().unwrap();
        let url = format!("file://{}", origin.path().display());
        run(clone.path(), &["clone", "-q", "--depth=1", &url, "."]);
        assert!(source(clone.path(), "fragment.toml").is_none());
        run(clone.path(), &["fetch", "-q", "--unshallow"]);
        assert_eq!(
            source(clone.path(), "fragment.toml").unwrap().commit,
            original
        );
    }

    #[test]
    fn fragment_source_is_optional_but_invalid_refs_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(FragmentHistory::new(dir.path(), "HEAD").unwrap().is_none());
        run(dir.path(), &["init", "-q"]);
        assert!(FragmentHistory::new(dir.path(), "HEAD").unwrap().is_none());
        std::fs::write(dir.path().join("base"), "base").unwrap();
        commit(dir.path(), "Base");
        assert!(FragmentHistory::new(dir.path(), "missing-ref").is_err());
    }

    #[test]
    fn fragment_source_uses_the_selected_ref_and_caches_shared_commits() {
        let dir = repository();
        let root = dir.path();
        std::fs::write(root.join("base"), "base").unwrap();
        let base = commit(root, "Base");
        for path in ["one.toml", "two spaces.toml"] {
            std::fs::write(root.join(path), "fragment").unwrap();
        }
        let added = commit(root, "Add #42");
        let mut history = FragmentHistory::new(root, &added).unwrap().unwrap();
        assert_eq!(
            history
                .source(&root.join("one.toml"))
                .unwrap()
                .unwrap()
                .commit,
            added
        );
        let cached = (history.commits.len(), history.changes.len());
        assert_eq!(
            history
                .source(&root.join("two spaces.toml"))
                .unwrap()
                .unwrap()
                .commit,
            added
        );
        assert_eq!((history.commits.len(), history.changes.len()), cached);
        assert!(
            FragmentHistory::new(root, &base)
                .unwrap()
                .unwrap()
                .source(&root.join("one.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn fragment_source_tracks_a_symlink_not_its_target() {
        let dir = repository();
        let root = dir.path();
        std::fs::write(root.join("target.toml"), "fragment").unwrap();
        commit(root, "Target");
        std::os::unix::fs::symlink("target.toml", root.join("fragment.toml")).unwrap();
        let added = commit(root, "Add fragment link #42");
        assert_eq!(source(root, "fragment.toml").unwrap().commit, added);
    }
}
