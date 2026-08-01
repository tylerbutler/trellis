//! Shared e2e helpers: a mock GitHub API served from a background thread.
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
