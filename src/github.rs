//! Minimal GitHub REST API client — the five calls trellis needs for release
//! PR management and GitHub Releases, previously delegated to the gh CLI.
//! Speaking to the API directly removes the runtime gh dependency; the CLI
//! remains only as a last-resort token source (`gh auth token`).

use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;
use std::process::Command;

pub struct GitHubClient {
    agent: ureq::Agent,
    base: String,
    token: String,
    owner: String,
    repo: String,
}

impl GitHubClient {
    /// A client for the repository `root` is a checkout of.
    ///
    /// The owner/repo pair comes from TRELLIS_GITHUB_REPO (`owner/repo`, tests
    /// and unusual remotes) or the `origin` remote URL. The token comes from
    /// GITHUB_TOKEN, then GH_TOKEN — GITHUB_TOKEN is ambient in GitHub Actions,
    /// so CI needs no setup — then a logged-in gh CLI. The base URL comes from
    /// TRELLIS_GITHUB_API_URL (tests point this at a local mock), defaulting
    /// to the public API.
    pub fn for_repo(root: &Path) -> Result<Self> {
        let (owner, repo) = resolve_repo(root)?;
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build(),
        );
        Ok(Self {
            agent,
            base: std::env::var("TRELLIS_GITHUB_API_URL")
                .unwrap_or_else(|_| "https://api.github.com".to_string()),
            token: resolve_token()?,
            owner,
            repo,
        })
    }

    /// The number of the open PR whose head is `branch`, if one exists.
    pub fn find_open_pr(&self, branch: &str) -> Result<Option<u64>> {
        let url = format!(
            "{}/repos/{}/{}/pulls?head={}:{branch}&state=open",
            self.base, self.owner, self.repo, self.owner
        );
        let (status, body) = self.get(&url)?;
        if status != 200 {
            bail!("GitHub API GET {url} failed: {}", api_error(status, &body));
        }
        Ok(body
            .as_array()
            .and_then(|prs| prs.first())
            .and_then(|pr| pr["number"].as_u64()))
    }

    /// Open a PR and return its URL.
    pub fn create_pr(&self, base: &str, head: &str, title: &str, body: &str) -> Result<String> {
        let url = format!("{}/repos/{}/{}/pulls", self.base, self.owner, self.repo);
        let payload = serde_json::json!({
            "base": base,
            "head": head,
            "title": title,
            "body": body,
        });
        let (status, response) = self.send("POST", &url, &payload)?;
        if status != 201 {
            bail!(
                "GitHub API POST {url} failed: {}",
                api_error(status, &response)
            );
        }
        response["html_url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("GitHub API POST {url}: response has no html_url"))
    }

    /// Retitle and re-body an existing PR.
    pub fn update_pr(&self, number: u64, title: &str, body: &str) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{number}",
            self.base, self.owner, self.repo
        );
        let payload = serde_json::json!({ "title": title, "body": body });
        let (status, response) = self.send("PATCH", &url, &payload)?;
        if status != 200 {
            bail!(
                "GitHub API PATCH {url} failed: {}",
                api_error(status, &response)
            );
        }
        Ok(())
    }

    /// Whether a GitHub Release exists for `tag`.
    pub fn release_exists(&self, tag: &str) -> Result<bool> {
        let url = format!(
            "{}/repos/{}/{}/releases/tags/{tag}",
            self.base, self.owner, self.repo
        );
        let (status, body) = self.get(&url)?;
        match status {
            200 => Ok(true),
            404 => Ok(false),
            _ => bail!("GitHub API GET {url} failed: {}", api_error(status, &body)),
        }
    }

    /// Create a GitHub Release on `tag`.
    pub fn create_release(&self, tag: &str, title: &str, notes: &str) -> Result<()> {
        let url = format!("{}/repos/{}/{}/releases", self.base, self.owner, self.repo);
        let payload = serde_json::json!({
            "tag_name": tag,
            "name": title,
            "body": notes,
        });
        let (status, response) = self.send("POST", &url, &payload)?;
        if status != 201 {
            bail!(
                "GitHub API POST {url} failed: {}",
                api_error(status, &response)
            );
        }
        Ok(())
    }

    fn get(&self, url: &str) -> Result<(u16, serde_json::Value)> {
        crate::term::trace_http("GET", url);
        let response = self
            .headers(self.agent.get(url))
            .call()
            .with_context(|| format!("GitHub API request failed: GET {url}"))?;
        read_response(response)
    }

    fn send(
        &self,
        method: &str,
        url: &str,
        payload: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value)> {
        crate::term::trace_http(method, url);
        let request = match method {
            "POST" => self.agent.post(url),
            "PATCH" => self.agent.patch(url),
            other => bail!("unsupported HTTP method {other}"),
        };
        let response = self
            .headers(request)
            .send_json(payload)
            .with_context(|| format!("GitHub API request failed: {method} {url}"))?;
        read_response(response)
    }

    fn headers<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        request
            .header("authorization", format!("Bearer {}", self.token))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", concat!("trellis/", env!("CARGO_PKG_VERSION")))
    }
}

fn read_response(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<(u16, serde_json::Value)> {
    let status = response.status().as_u16();
    // Error bodies matter as much as success bodies (they carry the API's
    // "message"), but not every response is JSON — a 502 from a proxy, say.
    let body = response
        .body_mut()
        .read_json()
        .unwrap_or(serde_json::Value::Null);
    Ok((status, body))
}

/// A one-line description of a failed API call: the status plus whatever
/// `message` GitHub attached.
fn api_error(status: u16, body: &serde_json::Value) -> String {
    match body["message"].as_str() {
        Some(message) => format!("HTTP {status}: {message}"),
        None => format!("HTTP {status}"),
    }
}

/// The token, from the environment or a logged-in gh CLI.
fn resolve_token() -> Result<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(token) = std::env::var(var) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                return Ok(token);
            }
        }
    }
    let gh = crate::tools::gh_bin();
    if let Ok(output) = Command::new(&gh).args(["auth", "token"]).output()
        && output.status.success()
    {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    bail!("no GitHub token found: set GITHUB_TOKEN (or GH_TOKEN), or log in with `gh auth login`")
}

/// Owner and repository, from TRELLIS_GITHUB_REPO or the `origin` remote.
fn resolve_repo(root: &Path) -> Result<(String, String)> {
    if let Ok(spec) = std::env::var("TRELLIS_GITHUB_REPO") {
        return match spec.split_once('/') {
            Some((owner, repo)) if !owner.is_empty() && !repo.is_empty() => {
                Ok((owner.to_string(), repo.to_string()))
            }
            _ => bail!("TRELLIS_GITHUB_REPO must be `owner/repo`, got `{spec}`"),
        };
    }
    let args = ["remote", "get-url", "origin"];
    crate::term::trace_command("git", &args, root);
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!(
            "GitHub operations need an `origin` remote: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_remote_url(&url).ok_or_else(|| {
        anyhow!(
            "origin remote `{url}` is not a GitHub repository (set TRELLIS_GITHUB_REPO to override)"
        )
    })
}

/// Parse owner/repo from a git remote URL (HTTPS, scp-like SSH, or ssh://).
///
/// Returns `None` if the URL is not a GitHub URL or cannot be parsed. Vendored
/// from tylerbutler/repoverlay (src/github.rs), with input trimming and
/// `ssh://` handling added.
pub fn parse_remote_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut segments = path.splitn(3, '/');
    let owner = segments.next()?;
    let repo = segments.next()?.trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::parse_remote_url;

    fn parsed(url: &str) -> Option<(String, String)> {
        parse_remote_url(url)
    }

    fn owner_repo(url: &str) -> (String, String) {
        parsed(url).unwrap()
    }

    #[test]
    fn parses_scp_like_ssh_urls() {
        assert_eq!(
            owner_repo("git@github.com:owner/repo.git"),
            ("owner".to_string(), "repo".to_string())
        );
        assert_eq!(
            owner_repo("git@github.com:owner/repo"),
            ("owner".to_string(), "repo".to_string())
        );
    }

    #[test]
    fn parses_ssh_scheme_urls() {
        assert_eq!(
            owner_repo("ssh://git@github.com/owner/repo.git"),
            ("owner".to_string(), "repo".to_string())
        );
    }

    #[test]
    fn parses_https_urls() {
        assert_eq!(
            owner_repo("https://github.com/owner/repo.git"),
            ("owner".to_string(), "repo".to_string())
        );
        assert_eq!(
            owner_repo("https://github.com/owner/repo"),
            ("owner".to_string(), "repo".to_string())
        );
    }

    #[test]
    fn rejects_non_github_urls() {
        assert_eq!(parsed("git@gitlab.com:owner/repo.git"), None);
        assert_eq!(parsed("https://gitlab.com/owner/repo"), None);
        assert_eq!(parsed("/local/bare/repo.git"), None);
    }

    #[test]
    fn rejects_missing_owner_or_repo() {
        assert_eq!(parsed("git@github.com:/repo"), None);
        assert_eq!(parsed("git@github.com:owner/"), None);
        assert_eq!(parsed("git@github.com:owner"), None);
        assert_eq!(parsed("https://github.com//repo"), None);
        assert_eq!(parsed("https://github.com/owner/"), None);
    }

    #[test]
    fn trims_the_trailing_newline_git_emits() {
        assert_eq!(
            owner_repo("https://github.com/owner/repo.git\n"),
            ("owner".to_string(), "repo".to_string())
        );
    }

    #[test]
    fn tolerates_extra_path_segments_and_trailing_slash() {
        assert_eq!(
            owner_repo("https://github.com/owner/repo/tree/main/subdir"),
            ("owner".to_string(), "repo".to_string())
        );
        assert_eq!(
            owner_repo("https://github.com/owner/repo/"),
            ("owner".to_string(), "repo".to_string())
        );
    }

    #[test]
    fn keeps_dots_in_repo_names() {
        assert_eq!(
            owner_repo("https://github.com/owner/repo.js.git"),
            ("owner".to_string(), "repo.js".to_string())
        );
        assert_eq!(
            owner_repo("https://github.com/owner/repo.js"),
            ("owner".to_string(), "repo.js".to_string())
        );
    }
}
