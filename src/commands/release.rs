//! `trellis release pr` — create or update the release pull request,
//! absorbing the PR management half of the changie-release action (design
//! §11 question 2, resolved toward "absorb"): compute the version plan, run
//! `version apply` on a release branch, push it, and drive `gh` to open or
//! refresh the PR. The tool already knows exactly what changed; gh does the
//! PR mechanics.

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

    // The plan must be computed before apply consumes the fragments.
    let plan = version::compute_plan(workspace)?;
    if plan.is_empty() {
        println!("no unreleased changes; nothing to release");
        return Ok(true);
    }

    let original_branch = git_stdout(root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    git_stdout(root, &["checkout", "-B", &options.branch])?;

    // Restore the starting branch however we leave this function.
    let result = build_release_commit_and_pr(workspace, options, &plan);
    let _ = Command::new("git")
        .args(["checkout", &original_branch])
        .current_dir(root)
        .output();
    result
}

fn build_release_commit_and_pr(
    workspace: &Workspace,
    options: &PrOptions,
    plan: &[version::PlanEntry],
) -> Result<bool> {
    let root = &workspace.root;
    if !version::apply(workspace, false)? {
        bail!("version apply failed");
    }

    let summary = plan
        .iter()
        .map(|entry| format!("{} v{}", entry.name, entry.next))
        .collect::<Vec<_>>()
        .join(", ");
    let title = format!("Release: {summary}");

    git_stdout(root, &["add", "-A"])?;
    let mut commit_args: Vec<String> = Vec::new();
    // A commit needs an identity; supply a fallback only when none is set
    // (CI runners), never overriding the user's own config.
    if git_stdout(root, &["config", "user.email"])
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        commit_args.extend([
            "-c".into(),
            "user.name=trellis".into(),
            "-c".into(),
            "user.email=trellis@localhost".into(),
        ]);
    }
    commit_args.extend(["commit".into(), "-m".into(), format!("release: {summary}")]);
    let commit_args: Vec<&str> = commit_args.iter().map(String::as_str).collect();
    git_stdout(root, &commit_args)?;

    // The release branch is regenerated from scratch each run, so a plain
    // force push implements create-or-update.
    git_stdout(root, &["push", "-f", "-u", "origin", &options.branch])
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
            println!("updated release PR #{number}: {title}");
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
            println!("created release PR: {}", url.trim());
        }
    }
    Ok(true)
}

/// Markdown body: the bump table, plus each package's new CHANGELOG section
/// when one exists (it will after `changie merge`).
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
