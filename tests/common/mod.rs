//! Shared support for the independently compiled integration-test targets.

#![allow(dead_code)]

use assert_cmd::Command;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn trellis(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("trellis").unwrap();
    cmd.current_dir(dir);
    cmd
}

pub fn trellis_with_stable_date(dir: &Path) -> Command {
    let mut cmd = trellis(dir);
    cmd.env("SOURCE_DATE_EPOCH", "1783728000");
    cmd
}

pub fn trellis_with_local_http(dir: &Path) -> Command {
    let mut cmd = trellis(dir);
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

pub fn trellis_github(dir: &Path, api: &str) -> Command {
    let mut cmd = trellis_with_local_http(dir);
    cmd.env("TRELLIS_GITHUB_API_URL", api)
        .env("TRELLIS_GITHUB_REPO", "example/repo")
        .env("GITHUB_TOKEN", "test-token");
    cmd
}

pub fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

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

pub fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "init"]);
}

pub fn make_executable(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn add_fragment(root: &Path, package: &str, kind: &str, body: &str) {
    let dir = root.join(".changes/unreleased");
    fs::create_dir_all(&dir).unwrap();
    for n in 1u32.. {
        let path = dir.join(format!("{package}-{n}.toml"));
        if !path.exists() {
            write(
                &path,
                &format!("project = \"{package}\"\nkind = \"{kind}\"\nbody = \"{body}\"\n"),
            );
            return;
        }
    }
}

pub fn version_of(root: &Path, package: &str) -> String {
    let manifest =
        fs::read_to_string(root.join("packages").join(package).join("gleam.toml")).unwrap();
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|line| line.strip_suffix('"'))
        .unwrap()
        .to_string()
}

pub fn install_fake_gleam(root: &Path) -> PathBuf {
    let script = root.join("fake-gleam.sh");
    write(
        &script,
        &format!(
            concat!(
                "#!/bin/sh\n",
                "set -eu\n",
                "root=\"{root}\"\n",
                "echo \"$(basename \"$PWD\") gleam $*\" >> \"$root/.fake/gleam-log\"\n",
                "if [ -d build/packages ]; then state=present; else state=absent; fi\n",
                "echo \"$(basename \"$PWD\") $1 $state\" >> \"$root/.fake/build-state\"\n",
                "if [ \"$1\" = publish ]; then\n",
                "  cp gleam.toml \"$root/.fake/published-$(basename \"$PWD\").toml\"\n",
                "fi\n",
            ),
            root = root.display()
        ),
    );
    make_executable(&script);
    fs::create_dir_all(root.join(".fake")).unwrap();
    script
}

pub fn bare_origin(root: &Path) -> tempfile::TempDir {
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare"]);
    git(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    remote
}

pub fn series_repo(root: &Path, publish: &str) -> tempfile::TempDir {
    copy_fixture_to(root);
    write(
        &root.join("gleam.toml"),
        &format!(
            "[tools.trellis]\nmembers = [\"packages/*\", \"examples/*\"]\n\
             exclude = {{ \"@release\" = [\"examples/*\"] }}\n\n\
             [tools.trellis.publish]\n{publish}\n"
        ),
    );
    init_repo(root);
    bare_origin(root)
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

pub fn git_stdout(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "safe.bareRepository=all"])
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn commit_of(dir: &Path, revision: &str) -> String {
    git_stdout(dir, &["rev-parse", &format!("{revision}^{{commit}}")])
}

/// Start a mock GitHub API and return its base URL.
///
/// Requests are appended to `.fake/github-log`. Created releases become
/// `.fake/release-<tag>` marker files, and pull-request listings read
/// `.fake/pr-list` or default to an empty list.
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
