//! `trellis release pr` — create or update the release pull request (design
//! §11 question 2, resolved toward "absorb"): compute the version plan, run
//! `version apply` on a release branch, push it, and drive the GitHub API to
//! open or refresh the PR. The tool already knows exactly what changed; the
//! API does the PR mechanics.
//!
//! `trellis release bootstrap` — `tag create` for adopting trellis on a
//! repository that already has the versions and changelogs it wants; see
//! [`crate::commands::tag::create`].

use super::release_state::{self, State};
use crate::commands::version_override::Overrides;
use crate::commands::{tag, version};
use crate::git::{git_output, git_stdout, git_with_identity, ls_remote_commit};
use crate::github::{GitHubClient, ReleasePr};
use crate::json::{ReleaseOutcome, ReleasePrDocument};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};

pub struct PrOptions {
    /// Base branch the PR targets.
    pub base: String,
    /// Branch the release commit is (force-)pushed to.
    pub branch: String,
    pub overrides: Overrides,
    pub continuation: ContinuationOptions,
    pub json: bool,
    pub github_output: bool,
}

#[derive(Default)]
pub struct ContinuationOptions {
    pub resume: Option<bool>,
    pub auto_pre: Option<bool>,
    pub skip_unchanged: Option<bool>,
    pub force: bool,
}

struct PrResult {
    outcome: ReleaseOutcome,
    plan: Vec<version::PlanEntry>,
    identity: Option<(u64, String)>,
    action: Option<&'static str>,
}

pub fn pr(workspace: &Workspace, options: &PrOptions) -> Result<bool> {
    let root = &workspace.root;
    let dirty = git_stdout(root, &["status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        bail!("working tree is not clean; commit or stash before `trellis release pr`");
    }

    git_stdout(root, &["check-ref-format", "--branch", &options.branch])?;
    if options.base.chars().any(char::is_control) || options.base.starts_with('-') {
        bail!("invalid release base");
    }
    let original_head = git_stdout(root, &["rev-parse", "HEAD"])?;
    let symbolic = git_output(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let original_branch = match symbolic.status.code() {
        Some(0) => String::from_utf8(symbolic.stdout)?.trim().to_string(),
        Some(1) => original_head.trim().to_string(),
        _ => bail!("cannot determine the caller's original branch"),
    };
    let base_ref = format!("{}^{{commit}}", options.base);
    let base_commit = git_stdout(root, &["rev-parse", "--verify", &base_ref])
        .with_context(|| format!("cannot resolve base branch `{}`", options.base))?;
    let base_commit = base_commit.trim();
    git_stdout(root, &["checkout", "--detach", base_commit])?;

    // Restore the starting branch however we leave this function.
    let result = (|| {
        let workspace = Workspace::load(root)
            .with_context(|| format!("failed to load base branch `{}`", options.base))?;
        prepare(&workspace, options, base_commit)
    })();
    let restored = git_stdout(root, &["checkout", &original_branch]);
    let result = match (result, restored) {
        (Ok(result), Ok(_)) => result,
        (Err(error), Ok(_)) => return Err(error),
        (Ok(_), Err(error)) => return Err(error.context("failed to restore the caller's branch")),
        (Err(error), Err(restoration)) => {
            bail!("{error:#}; failed to restore the caller's branch: {restoration:#}")
        }
    };
    let document = ReleasePrDocument {
        schema: ReleasePrDocument::SCHEMA,
        outcome: result.outcome,
        base: &options.base,
        branch: &options.branch,
        pr_number: result.identity.as_ref().map(|(n, _)| *n),
        pr_url: result.identity.map(|(_, url)| url),
        pr_action: result.action,
        bumped: version::bumps(&result.plan),
    };
    if options.json {
        println!("{}", serde_json::to_string_pretty(&document)?);
    } else if options.github_output {
        print!("{}", document.github_output()?);
    } else {
        match result.outcome {
            ReleaseOutcome::Unchanged => {
                crate::status!("release content is unchanged; nothing to prepare")
            }
            ReleaseOutcome::NoChanges => {
                crate::status!("no unreleased changes; nothing to release")
            }
            ReleaseOutcome::Prepared => {}
        }
    }
    Ok(true)
}

fn prepare(workspace: &Workspace, options: &PrOptions, base_commit: &str) -> Result<PrResult> {
    let config = &workspace.config.release;
    let resume = options.continuation.resume.unwrap_or(config.resume);
    let auto_pre = options.continuation.auto_pre.unwrap_or(config.auto_pre);
    let skip = options
        .continuation
        .skip_unchanged
        .unwrap_or(config.skip_unchanged);
    let mut overrides = options.overrides.clone();
    // Preserve no-token no-op and preflight behavior for existing explicit callers.
    let preflight = if !resume && !auto_pre {
        Some(version::compute_plan(workspace, &overrides)?)
    } else {
        None
    };
    if preflight.as_ref().is_some_and(Vec::is_empty) {
        return Ok(PrResult {
            outcome: ReleaseOutcome::NoChanges,
            plan: Vec::new(),
            identity: None,
            action: None,
        });
    }
    let github = GitHubClient::for_repo(&workspace.root)?;
    let pending = github.find_open_pr(&options.branch, &options.base)?;
    let mut pending_state = None;
    if let Some(pr) = &pending {
        git_stdout(&workspace.root, &["fetch", "--no-tags", "origin", &pr.head])
            .context("failed to fetch the selected release PR head")?;
        if resume || skip {
            pending_state = Some(State::from_pr(workspace, base_commit, &pr.head)?);
        }
        if resume {
            let state = pending_state.as_ref().context(
                "pending PR has no proposed release versions; use --no-resume with explicit release options to replace it")?;
            overrides = state.resume(workspace, &options.overrides)?;
        } else if auto_pre && !overrides.has_mode() {
            bail!(
                "a pending release PR owns preparation; pass --resume to preserve its proposed versions or --pre explicitly"
            );
        }
    }
    let mut automatic = false;
    if auto_pre
        && !overrides.has_mode()
        && let Some(label) = release_state::candidate_label(workspace)?
    {
        overrides = overrides.with_label(&label)?;
        automatic = true;
    }
    let plan = version::compute_plan(workspace, &overrides)?;
    if let Some(pr) = &pending {
        State::validate_pending_series(workspace, &pr.head, &plan)?;
    }
    if plan.is_empty() {
        if let Some(state) = &pending_state
            && state.inputs != release_state::inputs(workspace, base_commit)?
        {
            bail!(
                "release content was removed and the pending PR has no valid replacement plan; close the PR or add release fragments"
            );
        }
        return Ok(PrResult {
            outcome: ReleaseOutcome::NoChanges,
            plan,
            identity: pending.map(|p| (p.number, p.url)),
            action: None,
        });
    }
    let inputs = release_state::inputs(workspace, base_commit)?;
    let retargeted = plan.iter().any(|entry| {
        !entry.current.pre.is_empty()
            && ((
                entry.current.major,
                entry.current.minor,
                entry.current.patch,
            ) != (entry.next.major, entry.next.minor, entry.next.patch)
                || entry
                    .current
                    .pre
                    .as_str()
                    .rsplit_once('.')
                    .map(|(label, _)| label)
                    != entry
                        .next
                        .pre
                        .as_str()
                        .rsplit_once('.')
                        .map(|(label, _)| label))
    });
    let guard = (skip || automatic) && !options.continuation.force && !overrides.promoting();
    if guard {
        let unchanged = if let Some(previous) = &pending_state {
            previous.inputs == inputs && previous.matches_plan(&plan)
        } else if pending.is_none() && plan.iter().any(|p| !p.current.pre.is_empty()) {
            !retargeted && !release_state::candidate_changed(workspace, base_commit, &plan)?
        } else {
            false
        };
        if unchanged {
            return Ok(PrResult {
                outcome: ReleaseOutcome::Unchanged,
                plan,
                identity: pending.map(|p| (p.number, p.url)),
                action: None,
            });
        }
    }
    release_state::validate_location(workspace)?;
    let identity = build_release_commit_and_pr(
        workspace,
        options,
        &overrides,
        &plan,
        &github,
        pending.as_ref(),
    )?;
    Ok(PrResult {
        outcome: ReleaseOutcome::Prepared,
        plan,
        identity: Some(identity),
        action: Some(if pending.is_some() {
            "updated"
        } else {
            "created"
        }),
    })
}

fn build_release_commit_and_pr(
    workspace: &Workspace,
    options: &PrOptions,
    overrides: &Overrides,
    plan: &[version::PlanEntry],
    github: &GitHubClient,
    pending: Option<&ReleasePr>,
) -> Result<(u64, String)> {
    let root = &workspace.root;
    let reference = format!("refs/heads/{}", options.branch);
    let expected = ls_remote_commit(root, "origin", &reference).with_context(|| {
        format!(
            "failed to push branch {}: cannot inspect destination",
            options.branch
        )
    })?;
    if let Some(pr) = pending {
        anyhow::ensure!(
            expected.as_deref() == Some(pr.head.as_str()),
            "release branch differs from the selected PR head; retry"
        );
    }
    if !version::apply(workspace, overrides, false)? {
        bail!("version apply failed");
    }
    release_state::remove(workspace)?;

    let summary = plan
        .iter()
        .map(|entry| format!("{} v{}", entry.name, entry.next))
        .collect::<Vec<_>>()
        .join(", ");
    let title = format!("release: {summary}");

    git_stdout(root, &["add", "-A"])?;
    git_with_identity(root, &["commit", "-m", &title])?;

    // Prepare on detached HEAD so failures never move an existing local
    // release branch; only the remote branch is replaced after the commit is
    // complete.
    if let Some(pr) = pending {
        github.check_open_pr(pr, &options.branch, &options.base)?;
    }
    let destination = format!("HEAD:{reference}");
    let lease = format!(
        "--force-with-lease={reference}:{}",
        expected.as_deref().unwrap_or("")
    );
    let pushed_head = git_stdout(root, &["rev-parse", "HEAD"])?;
    git_stdout(root, &["push", &lease, "origin", &destination])
        .with_context(|| format!("failed to push branch {}", options.branch))?;

    let body = pr_body(workspace, plan);
    let finalized: Result<_> = (|| match pending {
        Some(pr) => {
            github.wait_for_pushed_pr(pr, pushed_head.trim(), &options.branch, &options.base)?;
            github.update_pr(pr.number, &title, &body)?;
            crate::status!(
                "{} release PR #{}: {title}",
                crate::term::ok("updated"),
                pr.number
            );
            Ok((pr.number, pr.url.clone()))
        }
        None => {
            let (number, url) = github.create_pr(&options.base, &options.branch, &title, &body)?;
            crate::status!("{} release PR: {url}", crate::term::ok("created"));
            Ok((number, url))
        }
    })();
    finalized.with_context(|| {
        format!(
            "branch `{}` was pushed successfully, but PR finalization remains incomplete; check the remote branch and PR before retrying",
            options.branch
        )
    })
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
        let member = &workspace.members[entry.member];
        if let Some(section) = tag::release_notes(member, &entry.next.to_string()) {
            body.push_str(&format!(
                "\n## {} v{}\n\n{section}\n",
                entry.name, entry.next
            ));
        }
    }
    body
}
