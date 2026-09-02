//! `trellis release pr` — create or update the release pull request (design
//! §11 question 2, resolved toward "absorb"): compute the version plan, run
//! `version apply` on a release branch, push it, and drive the GitHub API to
//! open or refresh the PR. The tool already knows exactly what changed; the
//! API does the PR mechanics.
//!
//! `trellis release bootstrap` — `tag create` for adopting trellis on a
//! repository that already has the versions and changelogs it wants; see
//! [`bootstrap`].

use crate::commands::version_override::Overrides;
use crate::commands::{tag, version};
use crate::git::{git_output, git_stdout};
use crate::github::GitHubClient;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};

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
    let _ = git_output(root, &["checkout", &original_branch]);
    result
}

/// `trellis release bootstrap` — an alias for `tag create` under the release
/// umbrella, for the repository *adopting* trellis: versions and changelogs
/// are already right, only the tags (and GitHub Releases) are missing.
/// Unlike `release pr`, it never runs `version apply` and requires no
/// unreleased changelog fragments — `tag::plan_tags` reads versions straight
/// off `gleam.toml`.
pub fn bootstrap(workspace: &Workspace, options: &tag::CreateOptions) -> Result<bool> {
    tag::create(workspace, options)?;
    Ok(true)
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
    let github = GitHubClient::for_repo(root)?;
    match github.find_open_pr(&options.branch)? {
        Some(number) => {
            github.update_pr(number, &title, &body)?;
            crate::status!(
                "{} release PR #{number}: {title}",
                crate::term::ok("updated")
            );
        }
        None => {
            let url = github.create_pr(&options.base, &options.branch, &title, &body)?;
            crate::status!("{} release PR: {url}", crate::term::ok("created"));
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
