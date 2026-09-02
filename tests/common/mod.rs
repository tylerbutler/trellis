#![allow(dead_code)]
//! Shared e2e helpers: fixture/process/git utilities and a mock GitHub API
//! served from a background thread.
//!
//! Point TRELLIS_GITHUB_API_URL at the returned base URL and set
//! TRELLIS_GITHUB_REPO plus GITHUB_TOKEN; every request the binary makes is
//! appended to `.fake/github-log` as `METHOD path?query`, then the JSON body,
//! then `---`, so tests assert on the log the way they asserted on the old
//! fake-gh log. State lives in `.fake/`: a created release becomes a
//! `release-<tag>` marker file (so existence checks respond 200 afterwards),
//! and the PR listing replies with `.fake/pr-list` or `[]`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use std::fs;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A trellis invocation in `dir` with deterministic changelog dates
/// (SOURCE_DATE_EPOCH = 2026-07-11) and proxy variables stripped, so requests
/// reach the localhost mocks instead of an agent proxy.
pub fn trellis(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trellis").unwrap();
    cmd.current_dir(dir);
    cmd.env("SOURCE_DATE_EPOCH", "1783728000");
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

/// A trellis invocation aimed at the mock GitHub API: base URL and repo
/// overridden, and a token in the environment.
pub fn trellis_github(dir: &Path, api: &str) -> Command {
    let mut cmd = trellis(dir);
    cmd.env("TRELLIS_GITHUB_API_URL", api)
        .env("TRELLIS_GITHUB_REPO", "example/repo")
        .env("GITHUB_TOKEN", "test-token");
    cmd
}

/// Run a command and parse its stdout as JSON. Takes the expected exit status
/// because `changelog check` reports failure through it while still emitting a
/// well-formed payload.
pub fn json_output(dir: &Path, args: &[&str], expect_success: bool) -> serde_json::Value {
    let output = trellis(dir).args(args).output().unwrap();
    assert_eq!(
        output.status.success(),
        expect_success,
        "unexpected exit for {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("{args:?} did not emit JSON: {err}"))
}

pub fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Copy the `basic` fixture into `root`.
pub fn copy_fixture_to(root: &Path) {
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let from = fixture("basic");
    let mut files = Vec::new();
    walk(&from, &mut files);
    for file in files {
        let dest = root.join(file.strip_prefix(&from).unwrap());
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&file, &dest).unwrap();
    }
}

pub fn git(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

pub fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A committed repository on `main`, for the commands that read git state.
pub fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
}

/// Write the next unreleased fragment for `project`. Uses the pre-1.0
/// `project` key on purpose, so the alias stays exercised across the suite.
pub fn add_fragment(root: &Path, project: &str, kind: &str, body: &str) {
    let dir = root.join(".changes/unreleased");
    fs::create_dir_all(&dir).unwrap();
    for n in 1u32.. {
        let path = dir.join(format!("{project}-{n}.toml"));
        if !path.exists() {
            write(
                &path,
                &format!("project = \"{project}\"\nkind = \"{kind}\"\nbody = \"{body}\"\n"),
            );
            return;
        }
    }
}

pub fn version_of(root: &Path, package: &str) -> String {
    let manifest = fs::read_to_string(root.join("packages").join(package).join("gleam.toml"))
        .unwrap_or_else(|err| panic!("no gleam.toml for {package}: {err}"));
    manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = "))
        .unwrap_or_else(|| panic!("no version in {package}'s gleam.toml"))
        .trim_matches('"')
        .to_string()
}

pub fn set_version(root: &Path, package: &str, version: &str) {
    let path = root.join("packages").join(package).join("gleam.toml");
    let text = fs::read_to_string(&path).unwrap();
    let text: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with("version = ") {
                format!("version = \"{version}\"")
            } else {
                line.to_string()
            }
        })
        .collect();
    fs::write(&path, text.join("\n") + "\n").unwrap();
}

pub fn mock_github(root: &Path) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::fs::create_dir_all(root.join(".fake")).unwrap();
    let root: PathBuf = root.to_path_buf();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            handle(&mut stream, &root);
        }
    });
    base
}

fn handle(stream: &mut TcpStream, root: &Path) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            }
            Err(_) => return,
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();

    let mut request_line = headers.lines().next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("").to_string();
    let target = request_line.next().unwrap_or("").to_string();
    let path = target.split('?').next().unwrap_or("").to_string();

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(".fake/github-log"))
        .unwrap();
    writeln!(log, "{method} {target}\n{body}\n---").unwrap();

    let (status, response) = route(&method, &path, &body, root);
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        )
        .as_bytes(),
    );
}

fn route(method: &str, path: &str, body: &str, root: &Path) -> (&'static str, String) {
    // Paths are /repos/{owner}/{repo}/<resource...>.
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let resource = (parts.first().copied(), parts.get(3).copied());
    match (method, resource, parts.get(4).copied()) {
        ("GET", (Some("repos"), Some("pulls")), None) => {
            let list = std::fs::read_to_string(root.join(".fake/pr-list"))
                .unwrap_or_else(|_| "[]".to_string());
            ("200 OK", list)
        }
        ("POST", (Some("repos"), Some("pulls")), None) => (
            "201 Created",
            r#"{"number":7,"html_url":"https://github.com/example/repo/pull/7"}"#.to_string(),
        ),
        ("PATCH", (Some("repos"), Some("pulls")), Some(_)) => ("200 OK", "{}".to_string()),
        ("GET", (Some("repos"), Some("releases")), Some("tags")) => {
            let tag = parts.get(5).copied().unwrap_or("");
            if root.join(format!(".fake/release-{tag}")).exists() {
                ("200 OK", format!(r#"{{"tag_name":"{tag}"}}"#))
            } else {
                ("404 Not Found", r#"{"message":"Not Found"}"#.to_string())
            }
        }
        ("POST", (Some("repos"), Some("releases")), None) => {
            let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let tag = parsed["tag_name"].as_str().unwrap_or("unknown");
            std::fs::write(root.join(format!(".fake/release-{tag}")), "").unwrap();
            ("201 Created", "{}".to_string())
        }
        _ => ("404 Not Found", r#"{"message":"no route"}"#.to_string()),
    }
}
