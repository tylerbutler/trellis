use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) fn route(
    method: &str,
    path: &str,
    target: &str,
    body: &[u8],
    root: &Path,
) -> Option<(&'static str, String)> {
    let prefix = "/repos/example/repo/releases";
    let resource = path.strip_prefix(prefix)?;
    let data: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if method == "POST" && resource.is_empty() && data["draft"].is_boolean() {
        let mut release = data;
        release["id"] = json!(100);
        release["assets"] = json!([]);
        let tag = release["tag_name"].as_str().unwrap();
        std::fs::write(
            root.join(format!(".fake/release-{tag}")),
            release.to_string(),
        )
        .unwrap();
        return Some(("201 Created", release.to_string()));
    }
    let (file, mut release) = std::fs::read_dir(root.join(".fake"))
        .ok()?
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().starts_with("release-"))
        .find_map(|f| {
            let value: Value =
                serde_json::from_str(&std::fs::read_to_string(f.path()).ok()?).ok()?;
            if value["id"] == 100 {
                Some((f.path(), value))
            } else {
                None
            }
        })?;
    let mut status = "200 OK";
    let response = if method == "GET" && resource.is_empty() {
        json!([release])
    } else if method == "GET" && resource.starts_with("/tags/") {
        if resource.strip_prefix("/tags/")? != release["tag_name"].as_str()? {
            return None;
        }
        if release["draft"] == true {
            return Some((
                "404 Not Found",
                json!({"message":"drafts are not published"}).to_string(),
            ));
        }
        release.clone()
    } else if method == "GET" && resource == "/100/assets" {
        let page = target.split("page=").last().unwrap_or("1");
        if page == "1" {
            release["assets"].clone()
        } else {
            json!([])
        }
    } else if method == "POST" && resource == "/100/assets" {
        let name = target.split("?name=").nth(1).unwrap();
        if let Ok(fail) = std::fs::read_to_string(root.join(".fake/fail-asset"))
            && name.ends_with(fail.trim())
        {
            return Some((
                "500 Internal Server Error",
                json!({"message":"upload failed"}).to_string(),
            ));
        }
        let id = release["next_asset_id"].as_u64().unwrap_or(1);
        release["next_asset_id"] = json!(id + 1);
        let asset = json!({
            "id": id, "name": name, "state": "uploaded",
            "digest": format!("sha256:{:x}", Sha256::digest(body)),
        });
        release["assets"]
            .as_array_mut()
            .unwrap()
            .push(asset.clone());
        std::fs::write(root.join(format!(".fake/asset-{id}")), body).unwrap();
        status = if root.join(".fake/fail-upload-response").exists() {
            "500 Internal Server Error"
        } else {
            "201 Created"
        };
        asset
    } else if method == "PATCH" && resource == "/100" {
        if data["draft"] == false && root.join(".fake/fail-finalize").exists() {
            return Some((
                "500 Internal Server Error",
                json!({"message":"finalization failed"}).to_string(),
            ));
        }
        for (key, value) in data.as_object().unwrap() {
            release[key] = value.clone();
        }
        release.clone()
    } else {
        let id = resource.strip_prefix("/assets/")?;
        let id: u64 = id.parse().unwrap();
        let assets = release["assets"].as_array_mut().unwrap();
        let index = assets.iter().position(|a| a["id"] == id)?;
        match method {
            "DELETE" => {
                assets.remove(index);
                status = "204 No Content";
                Value::Null
            }
            "PATCH" => {
                if root.join(".fake/fail-rename").exists() {
                    return Some((
                        "500 Internal Server Error",
                        json!({"message":"rename failed"}).to_string(),
                    ));
                }
                assets[index]["name"] = data["name"].clone();
                assets[index].clone()
            }
            "GET" => {
                return Some((
                    "200 OK",
                    std::fs::read_to_string(root.join(format!(".fake/asset-{id}"))).unwrap(),
                ));
            }
            _ => return None,
        }
    };
    std::fs::write(file, release.to_string()).unwrap();
    Some((
        status,
        if status.starts_with("204") {
            String::new()
        } else {
            response.to_string()
        },
    ))
}
