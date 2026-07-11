//! `trellis tag` — compare each releasable member's gleam.toml version
//! against existing `{name}-v{version}` tags; create the missing ones in
//! topological order, optionally with GitHub Releases carrying the matching
//! CHANGELOG section.

use crate::tools;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::Path;
use std::process::Command;

/// Releasable members (topo order) whose current version has no tag yet.
fn missing_tags(workspace: &Workspace) -> Result<Vec<(usize, String)>> {
    let existing = git_stdout(&workspace.root, &["tag", "--list"])?;
    let existing: std::collections::HashSet<&str> = existing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Ok(workspace
        .members
        .iter()
        .enumerate()
        .filter(|(_, member)| member.releasable)
        .filter_map(|(idx, member)| {
            let tag = workspace.config.format_tag(&member.name, member.version());
            (!existing.contains(tag.as_str())).then_some((idx, tag))
        })
        .collect())
}

pub fn plan(workspace: &Workspace, json: bool) -> Result<()> {
    let missing = missing_tags(workspace)?;
    if json {
        let items: Vec<_> = missing
            .iter()
            .map(|(idx, tag)| {
                let member = &workspace.members[*idx];
                json!({
                    "name": member.name,
                    "version": member.version(),
                    "tag": tag,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else if missing.is_empty() {
        println!("every releasable package version is already tagged");
    } else {
        for (idx, tag) in &missing {
            let member = &workspace.members[*idx];
            println!("{}: {} needs tag {tag}", member.name, member.version());
        }
    }
    Ok(())
}

pub struct CreateOptions {
    pub push: bool,
    pub github_release: bool,
}

pub fn create(workspace: &Workspace, options: &CreateOptions) -> Result<()> {
    let push = options.push || options.github_release;
    let targets = if push {
        workspace
            .members
            .iter()
            .enumerate()
            .filter(|(_, member)| member.releasable)
            .map(|(idx, member)| {
                (
                    idx,
                    workspace.config.format_tag(&member.name, member.version()),
                )
            })
            .collect()
    } else {
        missing_tags(workspace)?
    };
    if targets.is_empty() {
        println!("every releasable package version is already tagged");
        return Ok(());
    }

    for (idx, tag) in &targets {
        let member = &workspace.members[*idx];
        let local_oid = local_tag_oid(&workspace.root, tag)?;
        let remote_oid = if push {
            remote_tag_oid(&workspace.root, tag)?
        } else {
            None
        };
        if let (Some(local), Some(remote)) = (&local_oid, &remote_oid)
            && local != remote
        {
            bail!(
                "tag `{tag}` points to different objects locally ({local}) and on origin ({remote})"
            );
        }
        if local_oid.is_none() {
            if remote_oid.is_some() {
                git_stdout(&workspace.root, &["fetch", "origin", "tag", tag])?;
                println!("fetched {tag}");
            } else {
                let mut args = crate::git::identity_fallback_args(&workspace.root);
                args.extend([
                    "tag".into(),
                    "-a".into(),
                    tag.clone(),
                    "-m".into(),
                    format!("{} {}", member.name, member.version()),
                ]);
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                git_stdout(&workspace.root, &args)?;
                println!("tagged {tag}");
            }
        }
        if push && remote_oid.is_none() {
            git_stdout(&workspace.root, &["push", "origin", tag])
                .with_context(|| format!("failed to push tag {tag}"))?;
            println!("pushed {tag}");
        }
        if options.github_release {
            if github_release_exists(&workspace.root, tag)? {
                println!("GitHub release {tag} already exists; skipping");
            } else {
                let notes = release_notes(workspace, *idx);
                let gh = tools::gh_bin();
                let output = Command::new(&gh)
                    .args(["release", "create", tag, "--title", tag, "--notes", &notes])
                    .current_dir(&workspace.root)
                    .output()
                    .with_context(|| {
                        format!("failed to run `{gh}` — is the GitHub CLI installed?")
                    })?;
                if !output.status.success() {
                    bail!(
                        "`{gh} release create {tag}` failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                println!("created GitHub release {tag}");
            }
        }
    }
    Ok(())
}

fn local_tag_oid(root: &Path, tag: &str) -> Result<Option<String>> {
    let reference = format!("refs/tags/{tag}");
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .current_dir(root)
        .output()
        .context("failed to run git")?;
    match output.status.code() {
        Some(0) => output
            .stdout
            .split(|byte| byte.is_ascii_whitespace())
            .find(|part| !part.is_empty())
            .map(|oid| String::from_utf8_lossy(oid).into_owned())
            .map(Some)
            .context("git rev-parse returned no object ID"),
        Some(1) => Ok(None),
        _ => bail!("git rev-parse failed while checking tag `{tag}`"),
    }
}

fn remote_tag_oid(root: &Path, tag: &str) -> Result<Option<String>> {
    let reference = format!("refs/tags/{tag}");
    let output = Command::new("git")
        .args(["ls-remote", "--exit-code", "--tags", "origin", &reference])
        .current_dir(root)
        .output()
        .context("failed to run git")?;
    match output.status.code() {
        Some(0) => output
            .stdout
            .split(|byte| byte.is_ascii_whitespace())
            .find(|part| !part.is_empty())
            .map(|oid| String::from_utf8_lossy(oid).into_owned())
            .map(Some)
            .context("git ls-remote returned no object ID"),
        Some(2) => Ok(None),
        _ => bail!(
            "git ls-remote failed while checking tag `{tag}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn github_release_exists(root: &Path, tag: &str) -> Result<bool> {
    let gh = tools::gh_bin();
    let output = Command::new(&gh)
        .args(["release", "view", tag, "--json", "tagName"])
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to run `{gh}` — is the GitHub CLI installed?"))?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("release not found") {
        return Ok(false);
    }
    bail!("`{gh} release view {tag}` failed: {}", stderr.trim())
}

/// The member's CHANGELOG section for its current version, or a minimal
/// fallback body.
fn release_notes(workspace: &Workspace, idx: usize) -> String {
    let member = &workspace.members[idx];
    std::fs::read_to_string(member.path.join("CHANGELOG.md"))
        .ok()
        .and_then(|text| changelog_section(&text, member.version()))
        .unwrap_or_else(|| format!("{} {}", member.name, member.version()))
}

/// Extract the `## …` section whose heading names `version`, using the same
/// tolerant heading forms as the doctor check (`## 1.2.3`, `## [1.2.3]`,
/// `## name-v1.2.3 - date`).
pub fn changelog_section(text: &str, version: &str) -> Option<String> {
    let Ok(wanted) = semver::Version::parse(version) else {
        return None;
    };
    let mut section: Option<String> = None;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if section.is_some() {
                break; // next section starts; we're done
            }
            if heading_version(heading) == Some(wanted.clone()) {
                section = Some(String::new());
            }
        } else if let Some(section) = section.as_mut() {
            section.push_str(line);
            section.push('\n');
        }
    }
    section
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn heading_version(heading: &str) -> Option<semver::Version> {
    let token = heading.split_whitespace().next()?;
    let token = token.trim_matches(['[', ']']);
    let token = token.rsplit_once("-v").map(|(_, v)| v).unwrap_or(token);
    let token = token.strip_prefix('v').unwrap_or(token);
    semver::Version::parse(token).ok()
}

/// Resolve a tag like `lat_core-v1.2.0` to a releasable member and the
/// version the tag claims (which may differ from gleam.toml — the caller
/// decides whether that's fatal). Prefers the longest package-name match so
/// `lat_core_extra-v1.0.0` never resolves to `lat_core`.
pub fn resolve_tag(workspace: &Workspace, tag: &str) -> Result<(usize, String)> {
    let Some((prefix_tpl, suffix_tpl)) =
        workspace.config.publish.tag_format.split_once("{version}")
    else {
        bail!(
            "tag-format `{}` has no {{version}} placeholder",
            workspace.config.publish.tag_format
        );
    };

    let mut best: Option<(usize, String)> = None;
    for (idx, member) in workspace.members.iter().enumerate() {
        if !member.releasable {
            continue;
        }
        let prefix = prefix_tpl.replace("{name}", &member.name);
        let suffix = suffix_tpl.replace("{name}", &member.name);
        if tag.len() > prefix.len() + suffix.len()
            && tag.starts_with(&prefix)
            && tag.ends_with(&suffix)
        {
            let version = tag[prefix.len()..tag.len() - suffix.len()].to_string();
            let longer = best
                .as_ref()
                .is_none_or(|(other, _)| member.name.len() > workspace.members[*other].name.len());
            if longer {
                best = Some((idx, version));
            }
        }
    }
    best.with_context(|| {
        format!(
            "tag `{tag}` does not match any releasable package (tag format: {})",
            workspace.config.publish.tag_format
        )
    })
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

#[cfg(test)]
mod tests {
    use super::changelog_section;

    #[test]
    fn extracts_the_matching_section_only() {
        let text = concat!(
            "# Changelog\n\n",
            "## lat_core-v1.3.0 - 2026-07-01\n\n",
            "### Added\n* new thing\n\n",
            "## [1.2.0]\n\n* older\n",
        );
        assert_eq!(
            changelog_section(text, "1.3.0").unwrap(),
            "### Added\n* new thing"
        );
        assert_eq!(changelog_section(text, "1.2.0").unwrap(), "* older");
        assert_eq!(changelog_section(text, "9.9.9"), None);
    }
}
