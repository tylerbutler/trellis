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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePr {
    pub number: u64,
    pub url: String,
    pub head: String,
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

    /// Only same-repository PRs with the requested head and base can own a release.
    pub fn find_open_pr(&self, branch: &str, base: &str) -> Result<Option<ReleasePr>> {
        let mut found = None;
        for page in 1.. {
            let url = self.url(&format!(
                "/pulls?head={}:{}&state=open&per_page=100&page={page}",
                self.owner,
                Self::query_value(branch),
            ));
            let body = expect_status("GET", &url, 200, self.get(&url)?)?;
            let prs = body
                .as_array()
                .context("GitHub PR listing is not an array")?;
            for pr in prs {
                if let Some(pr) = self.match_release_pr(pr, branch, base)?
                    && found.replace(pr).is_some()
                {
                    bail!(
                        "multiple same-repository release PRs match `{branch}` -> `{base}`; close the conflicting PR"
                    );
                }
            }
            if prs.len() < 100 {
                break;
            }
        }
        Ok(found)
    }

    fn match_release_pr(
        &self,
        pr: &serde_json::Value,
        branch: &str,
        base: &str,
    ) -> Result<Option<ReleasePr>> {
        let repo = format!("{}/{}", self.owner, self.repo);
        let text = |pointer| {
            pr.pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .with_context(|| format!("GitHub PR response is missing `{pointer}`"))
        };
        if pr
            .pointer("/head/repo")
            .is_some_and(serde_json::Value::is_null)
        {
            return Ok(None);
        }
        if !text("/head/repo/full_name")?.eq_ignore_ascii_case(&repo)
            || !text("/base/repo/full_name")?.eq_ignore_ascii_case(&repo)
        {
            return Ok(None);
        }
        if text("/head/ref")? != branch || text("/state")? != "open" {
            return Ok(None);
        }
        if text("/base/ref")? != base {
            bail!(
                "release branch `{branch}` already has a PR targeting `{}`; choose a different --branch or correct its base",
                text("/base/ref")?
            );
        }
        let head = text("/head/sha")?;
        if !matches!(head.len(), 40 | 64) || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("GitHub release PR has an invalid head SHA");
        }
        let url = text("/html_url")?;
        if !url.starts_with("https://") || url.chars().any(char::is_control) {
            bail!("GitHub release PR has an invalid html_url");
        }
        Ok(Some(ReleasePr {
            number: pr["number"]
                .as_u64()
                .filter(|n| *n > 0)
                .context("GitHub release PR has no number")?,
            url: url.to_string(),
            head: head.to_string(),
        }))
    }

    pub fn check_open_pr(&self, expected: &ReleasePr, branch: &str, base: &str) -> Result<()> {
        let live = self.read_open_pr(expected.number, branch, base)?;
        if live != *expected {
            bail!("release PR changed during preparation; retry against its current head");
        }
        Ok(())
    }

    /// Only the previously checked head can be stale after a successful leased push.
    pub fn wait_for_pushed_pr(
        &self,
        previous: &ReleasePr,
        pushed_head: &str,
        branch: &str,
        base: &str,
    ) -> Result<()> {
        let mut pushed = previous.clone();
        pushed.head = pushed_head.to_string();
        for attempt in 0..6 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            let live = self.read_open_pr(previous.number, branch, base)?;
            if live == pushed {
                return Ok(());
            }
            if live != *previous {
                bail!("release PR changed during preparation; retry against its current head");
            }
        }
        bail!(
            "GitHub release PR still reports the previous head `{}` after waiting for pushed head `{pushed_head}`",
            previous.head
        )
    }

    fn read_open_pr(&self, number: u64, branch: &str, base: &str) -> Result<ReleasePr> {
        let url = self.url(&format!("/pulls/{number}"));
        let body = expect_status("GET", &url, 200, self.get(&url)?)?;
        self.match_release_pr(&body, branch, base)?
            .context("release PR was closed, merged, or retargeted; retry after checking its state")
    }

    /// Open a PR and return its URL.
    pub fn create_pr(
        &self,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<(u64, String)> {
        let url = self.url("/pulls");
        let payload = serde_json::json!({
            "base": base,
            "head": head,
            "title": title,
            "body": body,
        });
        let response = expect_status("POST", &url, 201, self.send(Write::Post, &url, &payload)?)?;
        let url = response["html_url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("GitHub API POST {url}: response has no html_url"))?;
        if !url.starts_with("https://") || url.chars().any(char::is_control) {
            bail!("GitHub PR creation returned an invalid html_url");
        }
        let number = response["number"]
            .as_u64()
            .filter(|n| *n > 0)
            .context("GitHub PR creation returned no number")?;
        Ok((number, url))
    }

    fn query_value(text: &str) -> String {
        text.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_./".contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }

    /// Retitle and re-body an existing PR.
    pub fn update_pr(&self, number: u64, title: &str, body: &str) -> Result<()> {
        let url = self.url(&format!("/pulls/{number}"));
        let payload = serde_json::json!({ "title": title, "body": body });
        expect_status("PATCH", &url, 200, self.send(Write::Patch, &url, &payload)?)?;
        Ok(())
    }

    /// Whether a GitHub Release exists for `tag`.
    pub fn release_exists(&self, tag: &str) -> Result<bool> {
        let url = self.url(&format!("/releases/tags/{tag}"));
        let (status, body) = self.get(&url)?;
        match status {
            200 => Ok(true),
            404 => Ok(false),
            _ => bail!("GitHub API GET {url} failed: {}", api_error(status, &body)),
        }
    }

    /// Create a GitHub Release on `tag`.
    pub fn create_release(
        &self,
        tag: &str,
        title: &str,
        notes: &str,
        prerelease: bool,
    ) -> Result<()> {
        let url = self.url("/releases");
        let payload = serde_json::json!({
            "tag_name": tag,
            "name": title,
            "body": notes,
            "prerelease": prerelease,
        });
        expect_status("POST", &url, 201, self.send(Write::Post, &url, &payload)?)?;
        Ok(())
    }

    /// `tail` appended to this repository's API root.
    fn url(&self, tail: &str) -> String {
        format!("{}/repos/{}/{}{tail}", self.base, self.owner, self.repo)
    }

    fn get(&self, url: &str) -> Result<(u16, serde_json::Value)> {
        crate::term::trace_http("GET", url);
        let response = self
            .headers(self.agent.get(url))
            .call()
            .with_context(|| format!("GitHub API request failed: GET {url}"))?;
        Ok(read_response(response))
    }

    fn send(
        &self,
        method: Write,
        url: &str,
        payload: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value)> {
        let name = method.name();
        crate::term::trace_http(name, url);
        let request = match method {
            Write::Post => self.agent.post(url),
            Write::Patch => self.agent.patch(url),
        };
        let response = self
            .headers(request)
            .send_json(payload)
            .with_context(|| format!("GitHub API request failed: {name} {url}"))?;
        Ok(read_response(response))
    }

    fn headers<B>(&self, request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        request
            .header("authorization", format!("Bearer {}", self.token))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", concat!("trellis/", env!("CARGO_PKG_VERSION")))
    }
}

/// The two methods trellis writes with.
#[derive(Clone, Copy)]
enum Write {
    Post,
    Patch,
}

impl Write {
    fn name(self) -> &'static str {
        match self {
            Write::Post => "POST",
            Write::Patch => "PATCH",
        }
    }
}

fn read_response(mut response: ureq::http::Response<ureq::Body>) -> (u16, serde_json::Value) {
    let status = response.status().as_u16();
    // Error bodies matter as much as success bodies (they carry the API's
    // "message"), but not every response is JSON — a 502 from a proxy, say.
    let body = response
        .body_mut()
        .read_json()
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// The body of a response with the `expected` status; anything else is an
/// error naming the call.
fn expect_status(
    method: &str,
    url: &str,
    expected: u16,
    (status, body): (u16, serde_json::Value),
) -> Result<serde_json::Value> {
    if status != expected {
        bail!(
            "GitHub API {method} {url} failed: {}",
            api_error(status, &body)
        );
    }
    Ok(body)
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
    if let Some(token) = ["GITHUB_TOKEN", "GH_TOKEN"]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .map(|token| token.trim().to_string())
        .find(|token| !token.is_empty())
    {
        return Ok(token);
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
    let url = crate::git::git_stdout(root, &["remote", "get-url", "origin"])
        .context("GitHub operations need an `origin` remote")?;
    let url = url.trim();
    parse_remote_url(url).ok_or_else(|| {
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

    fn owner_repo(url: &str) -> (String, String) {
        parse_remote_url(url).unwrap()
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
        assert_eq!(parse_remote_url("git@gitlab.com:owner/repo.git"), None);
        assert_eq!(parse_remote_url("https://gitlab.com/owner/repo"), None);
        assert_eq!(parse_remote_url("/local/bare/repo.git"), None);
    }

    #[test]
    fn rejects_missing_owner_or_repo() {
        assert_eq!(parse_remote_url("git@github.com:/repo"), None);
        assert_eq!(parse_remote_url("git@github.com:owner/"), None);
        assert_eq!(parse_remote_url("git@github.com:owner"), None);
        assert_eq!(parse_remote_url("https://github.com//repo"), None);
        assert_eq!(parse_remote_url("https://github.com/owner/"), None);
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
