//! `trellis release pr` — create or update the release pull request (design
//! §11 question 2, resolved toward "absorb"): compute the version plan, run
//! `version apply` on a release branch, push it, and drive `gh` to open or
//! refresh the PR. The tool already knows exactly what changed; gh does the
//! PR mechanics.
//!
//! `trellis release bootstrap` — reconcile tags against current manifest
//! versions with no version bump involved, for adopting trellis on a
//! repository that already has the versions and changelogs it wants; see
//! [`bootstrap`].

use crate::commands::version_override::Overrides;
use crate::commands::{tag, version};
use crate::tools;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

pub struct PrOptions {
    /// Base branch the PR targets.
    pub base: String,
    /// Branch the release commit is (force-)pushed to.
    pub branch: String,
}

pub fn pr(workspace: &Workspace, options: &PrOptions) -> Result<bool> {
    let root = &workspace.root;
    let dirty = git_stdout(root, &["status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        bail!("working tree is not clean; commit or stash before `trellis release pr`");
    }

    let original_branch = git_stdout(root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let base_ref = format!("{}^{{commit}}", options.base);
    let base_commit = git_stdout(root, &["rev-parse", "--verify", &base_ref])
        .with_context(|| format!("cannot resolve base branch `{}`", options.base))?;
    let base_commit = base_commit.trim();
    git_stdout(root, &["checkout", "--detach", base_commit])?;

    // Restore the starting branch however we leave this function.
    let result = (|| {
        let workspace = Workspace::load(root)
            .with_context(|| format!("failed to load base branch `{}`", options.base))?;
        // `release pr` cuts an ordinary release; an override belongs on the
        // `version` commands, where it can be previewed by `plan` first.
        let plan = version::compute_plan(&workspace, &Overrides::default())?;
        if plan.is_empty() {
            crate::status!("no unreleased changes; nothing to release");
            return Ok(true);
        }
        build_release_commit_and_pr(&workspace, options, &plan)
    })();
    crate::term::trace_command("git", &["checkout", &original_branch], root);
    let _ = Command::new("git")
        .args(["checkout", &original_branch])
        .current_dir(root)
        .output();
    result
}

pub struct BootstrapOptions {
    /// Report every planned action without doing anything.
    pub dry_run: bool,
    /// Push tags to origin.
    pub push: bool,
    /// Create a GitHub Release per exact tag (implies `push`).
    pub github_release: bool,
}

/// `trellis release bootstrap` — reconcile tags (and, with `--github-release`,
/// GitHub Releases) against the workspace's *current* manifest versions. A
/// repository adopting trellis with correct package versions and changelogs
/// already in place, but no tags yet, needs exactly this: unlike `release
/// pr`, bootstrap never runs `version apply` and requires no unreleased
/// changelog fragments — `tag::plan_tags` reads versions straight off
/// `gleam.toml`.
///
/// Every planned tag is checked for a local/remote conflict before any of
/// them are mutated (`tag::tag_conflicts`, itself computed from the same
/// read-only `tag::tag_status` a `--dry-run` preview reports from) — one
/// package's immutable tag disagreeing with origin fails the whole run
/// rather than leaving an earlier package half-tagged. `--dry-run` runs this
/// same preflight, so a conflict is reported (and the command still exits
/// non-zero) without needing `--push` for real.
pub fn bootstrap(workspace: &Workspace, options: &BootstrapOptions) -> Result<bool> {
    let push = options.push || options.github_release;
    let planned = tag::plan_tags(workspace)?;
    if planned.is_empty() {
        crate::status!("no releasable packages to bootstrap");
        return Ok(true);
    }

    let statuses = tag::tag_status(workspace, &planned, push, options.github_release)?;
    let conflicts = tag::tag_conflicts(&statuses);
    if !conflicts.is_empty() {
        let details = conflicts
            .iter()
            .map(|conflict| {
                format!(
                    "`{}` points to different objects locally ({}) and on origin ({})",
                    conflict.tag, conflict.local_oid, conflict.remote_oid
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        bail!("bootstrap refused: conflicting tags found: {details}");
    }

    if options.dry_run {
        report_bootstrap_plan(workspace, &statuses, push, options.github_release);
        return Ok(true);
    }

    tag::create(
        workspace,
        &tag::CreateOptions {
            push: options.push,
            github_release: options.github_release,
        },
    )?;
    Ok(true)
}

/// One status line per planned tag: its local action, plus (when queried)
/// what pushing it would do and whether a GitHub Release already exists.
/// Deterministic and mutation-free — every value comes straight out of
/// `statuses`, itself gathered read-only.
fn report_bootstrap_plan(
    workspace: &Workspace,
    statuses: &[tag::TagStatus],
    push: bool,
    github_release: bool,
) {
    for status in statuses {
        let member = &workspace.members[status.member];
        let kind = match status.kind {
            tag::TagKind::Exact => "tag",
            tag::TagKind::Series => "series tag",
        };
        let local = match status.action {
            tag::TagAction::Create => "create",
            tag::TagAction::Move => "move",
            tag::TagAction::UpToDate => "up to date",
        };
        let mut line = format!(
            "{}: {} {kind} {} — {local}",
            member.name,
            member.version(),
            status.tag
        );
        if push {
            let remote = match status.remote_action {
                Some(tag::RemoteAction::Fetch) => "fetch from origin",
                Some(tag::RemoteAction::Push) => "push",
                Some(tag::RemoteAction::ForcePush) => "force-push",
                Some(tag::RemoteAction::UpToDate) => "already on origin",
                None => "not checked",
            };
            line.push_str(&format!("; {remote}"));
        }
        if github_release && status.kind == tag::TagKind::Exact {
            let release = match status.release_exists {
                Some(true) => "GitHub release already exists",
                Some(false) => "create GitHub release",
                None => "GitHub release not checked",
            };
            line.push_str(&format!("; {release}"));
        }
        crate::status!("{line}");
    }
}

fn build_release_commit_and_pr(
    workspace: &Workspace,
    options: &PrOptions,
    plan: &[version::PlanEntry],
) -> Result<bool> {
    let root = &workspace.root;
    if !version::apply(workspace, &Overrides::default(), false)? {
        bail!("version apply failed");
    }

    let summary = plan
        .iter()
        .map(|entry| format!("{} v{}", entry.name, entry.next))
        .collect::<Vec<_>>()
        .join(", ");
    let title = format!("release: {summary}");

    git_stdout(root, &["add", "-A"])?;
    let mut commit_args = crate::git::identity_fallback_args(root);
    commit_args.extend(["commit".into(), "-m".into(), format!("release: {summary}")]);
    let commit_args: Vec<&str> = commit_args.iter().map(String::as_str).collect();
    git_stdout(root, &commit_args)?;

    // Prepare on detached HEAD so failures never move an existing local
    // release branch; only the remote branch is replaced after the commit is
    // complete.
    let destination = format!("HEAD:refs/heads/{}", options.branch);
    git_stdout(root, &["push", "-f", "origin", &destination])
        .with_context(|| format!("failed to push branch {}", options.branch))?;

    let body = pr_body(workspace, plan);
    let existing = gh_stdout(
        root,
        &[
            "pr",
            "list",
            "--head",
            &options.branch,
            "--state",
            "open",
            "--json",
            "number",
        ],
    )?;
    let existing: serde_json::Value =
        serde_json::from_str(existing.trim()).unwrap_or(serde_json::Value::Null);
    match existing
        .as_array()
        .and_then(|prs| prs.first())
        .and_then(|pr| pr["number"].as_u64())
    {
        Some(number) => {
            let number = number.to_string();
            gh_stdout(
                root,
                &["pr", "edit", &number, "--title", &title, "--body", &body],
            )?;
            crate::status!("updated release PR #{number}: {title}");
        }
        None => {
            let url = gh_stdout(
                root,
                &[
                    "pr",
                    "create",
                    "--base",
                    &options.base,
                    "--head",
                    &options.branch,
                    "--title",
                    &title,
                    "--body",
                    &body,
                ],
            )?;
            crate::status!("created release PR: {}", url.trim());
        }
    }
    Ok(true)
}

/// Markdown body: the bump table, plus each package's new CHANGELOG section
/// (present after `version apply` reassembled the changelogs).
fn pr_body(workspace: &Workspace, plan: &[version::PlanEntry]) -> String {
    let mut body = String::from(
        "Releases prepared by `trellis release pr`.\n\n| package | from | to | fragments |\n| --- | --- | --- | --- |\n",
    );
    for entry in plan {
        body.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            entry.name, entry.current, entry.next, entry.fragments
        ));
    }
    for entry in plan {
        let Some(idx) = workspace.member_index(&entry.name) else {
            continue;
        };
        let changelog = workspace.members[idx].path.join("CHANGELOG.md");
        if let Some(section) = std::fs::read_to_string(changelog)
            .ok()
            .and_then(|text| tag::changelog_section(&text, &entry.next))
        {
            body.push_str(&format!(
                "\n## {} v{}\n\n{section}\n",
                entry.name, entry.next
            ));
        }
    }
    body
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    run_tool(cwd, "git", args)
}

fn gh_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let gh = tools::gh_bin();
    run_tool(cwd, &gh, args)
}

fn run_tool(cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
    crate::term::trace_command(program, args, cwd);
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run `{program}`"))?;
    if !output.status.success() {
        bail!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
