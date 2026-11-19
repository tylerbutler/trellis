//! Required release assets, draft staging, and retryable finalization.

use super::{GitHubClient, Write, expect_status, read_response};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, Write as _};
use std::path::Path;

const MARKER: &str = "<!-- trellis.release_assets/1\n";

pub struct LocalAsset {
    pub name: String,
    digest: String,
    file: tempfile::NamedTempFile,
}

impl LocalAsset {
    pub fn read(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("release asset must have a UTF-8 file name")?
            .to_string();
        if name.starts_with("trellis-upload-")
            || name.len() > 170
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        {
            bail!(
                "asset `{name}` must use at most 170 ASCII letters, digits, dots, underscores or hyphens, and must not start with `trellis-upload-`"
            );
        }
        if !std::fs::metadata(path)
            .with_context(|| format!("cannot read required asset `{}`", path.display()))?
            .is_file()
        {
            bail!("required asset `{}` is not a regular file", path.display());
        }
        let mut source = std::fs::File::open(path)
            .with_context(|| format!("cannot read required asset `{}`", path.display()))?;
        if !source.metadata()?.is_file() {
            bail!("required asset `{}` is not a regular file", path.display());
        }
        let mut file = tempfile::NamedTempFile::new()?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
        }
        Ok(Self {
            name,
            digest: format!("sha256:{:x}", hash.finalize()),
            file,
        })
    }
}

pub struct ReleaseRequest<'a> {
    pub tag: &'a str,
    pub revision: &'a str,
    pub notes: &'a str,
    pub prerelease: bool,
    pub assets: &'a [LocalAsset],
    pub draft: bool,
    pub replace_assets: bool,
    pub dry_run: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetIntent {
    revision: String,
    assets: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Release {
    id: u64,
    tag_name: String,
    draft: bool,
    body: Option<String>,
    target_commitish: String,
}

#[derive(Deserialize)]
struct Asset {
    id: u64,
    name: String,
    state: String,
    digest: Option<String>,
}

fn recorded_intent(body: &str) -> Result<Option<AssetIntent>> {
    let Some((_, rest)) = body.split_once(MARKER) else {
        if body.contains("<!-- trellis.release_assets/") {
            bail!("unsupported required asset record");
        }
        return Ok(None);
    };
    if rest.contains("<!-- trellis.release_assets/") {
        bail!("release has duplicate required asset records");
    }
    let (json, _) = rest
        .split_once("\n-->")
        .context("invalid required asset record")?;
    let intent: AssetIntent =
        serde_json::from_str(json).context("invalid required asset record")?;
    for digest in intent.assets.values() {
        let hash = digest
            .strip_prefix("sha256:")
            .context("invalid required asset digest")?;
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            bail!("invalid required asset digest");
        }
    }
    Ok(Some(intent))
}

impl GitHubClient {
    pub(super) fn check_recorded_assets(&self, release: &serde_json::Value) -> Result<()> {
        let body = release["body"].as_str().unwrap_or("");
        if let Some(intent) = recorded_intent(body)? {
            let id = release["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .context("GitHub release has no id")?;
            self.verify_required_assets(id, &intent)?;
        }
        Ok(())
    }

    fn read_release(&self, tag: &str) -> Result<Option<Release>> {
        match self.find_release(tag)? {
            Some(body) => {
                let release: Release =
                    serde_json::from_value(body).context("invalid GitHub release response")?;
                if release.id == 0 || release.tag_name != tag {
                    bail!("GitHub release does not identify expected tag `{tag}`");
                }
                Ok(Some(release))
            }
            None => Ok(None),
        }
    }

    fn release_intent(
        &self,
        release: Option<&Release>,
        request: &ReleaseRequest<'_>,
    ) -> Result<AssetIntent> {
        if release.is_some_and(|r| {
            matches!(r.target_commitish.len(), 40 | 64)
                && r.target_commitish.bytes().all(|c| c.is_ascii_hexdigit())
                && r.target_commitish != request.revision
        }) {
            bail!("GitHub release target does not identify the qualified revision at HEAD");
        }
        let body = release.and_then(|r| r.body.as_deref()).unwrap_or("");
        let mut intent = if let Some(intent) = recorded_intent(body)? {
            if intent.revision != request.revision {
                bail!("release does not identify the qualified revision at HEAD");
            }
            intent
        } else {
            AssetIntent {
                revision: request.revision.to_string(),
                assets: BTreeMap::new(),
            }
        };
        for asset in request.assets {
            if intent
                .assets
                .get(&asset.name)
                .is_some_and(|digest| digest != &asset.digest)
                && !request.replace_assets
            {
                bail!(
                    "required asset `{}` conflicts with the recorded digest; use --replace-assets to replace it",
                    asset.name
                );
            }
            intent
                .assets
                .insert(asset.name.clone(), asset.digest.clone());
        }
        if request.draft && release.is_some_and(|r| !r.draft) {
            bail!("release is already published; it cannot be staged as a draft");
        }
        Ok(intent)
    }

    pub fn check_release(&self, request: &ReleaseRequest<'_>) -> Result<()> {
        let release = self.read_release(request.tag)?;
        self.release_intent(release.as_ref(), request)?;
        if let Some(release) = release {
            let assets = self.release_assets(release.id)?;
            for local in request.assets {
                if let Some(existing) = assets.iter().find(|a| a.name == local.name)
                    && !self.asset_matches(existing, &local.digest)?
                    && !request.replace_assets
                    && existing.state != "starter"
                {
                    bail!(
                        "asset `{}` conflicts with existing contents; use --replace-assets to replace it",
                        local.name
                    );
                }
            }
        }
        Ok(())
    }

    pub fn reconcile_release(&self, request: &ReleaseRequest<'_>) -> Result<()> {
        let existing = self.read_release(request.tag)?;
        let intent = self.release_intent(existing.as_ref(), request)?;
        if request.dry_run {
            crate::status!(
                "would stage required assets and {} GitHub release {}",
                if request.draft {
                    "retain draft"
                } else {
                    "finalize"
                },
                request.tag
            );
            return Ok(());
        }
        let notes = existing
            .as_ref()
            .and_then(|r| r.body.as_deref())
            .unwrap_or(request.notes);
        let notes = notes
            .split_once(MARKER)
            .map_or(notes, |(notes, _)| notes)
            .trim_end();
        let body = format!(
            "{notes}\n\n{MARKER}{}\n-->",
            serde_json::to_string(&intent)?
        );
        let release = if let Some(release) = existing {
            if release.body.as_deref() != Some(&body) {
                let url = self.url(&format!("/releases/{}", release.id));
                expect_status(
                    "PATCH",
                    &url,
                    200,
                    self.send(Write::Patch, &url, &serde_json::json!({ "body": body }))?,
                )?;
            }
            release
        } else {
            let url = self.url("/releases");
            let payload = serde_json::json!({
                "tag_name": request.tag, "target_commitish": request.revision,
                "name": request.tag, "body": body, "prerelease": request.prerelease, "draft": true,
            });
            let response =
                expect_status("POST", &url, 201, self.send(Write::Post, &url, &payload)?)?;
            let release: Release = serde_json::from_value(response)
                .context("invalid GitHub release creation response")?;
            if release.id == 0 || release.tag_name != request.tag || !release.draft {
                bail!("GitHub did not create the expected draft release");
            }
            release
        };
        for asset in request.assets {
            self.reconcile_asset(release.id, asset, request.replace_assets)?;
        }
        if !request.draft {
            self.verify_required_assets(release.id, &intent)?;
            if release.draft {
                let current = self
                    .read_release(request.tag)?
                    .context("draft release disappeared before finalization")?;
                if current.id != release.id || current.body.as_deref() != Some(&body) {
                    bail!(
                        "release changed during asset verification; retry against its current state"
                    );
                }
                let url = self.url(&format!("/releases/{}", release.id));
                let response = expect_status(
                    "PATCH",
                    &url,
                    200,
                    self.send(
                        Write::Patch,
                        &url,
                        &serde_json::json!({
                            "draft": false, "prerelease": request.prerelease,
                        }),
                    )?,
                )?;
                if response["draft"].as_bool() != Some(false) {
                    bail!("GitHub did not finalize the draft release");
                }
            }
        }
        Ok(())
    }

    fn verify_required_assets(&self, release: u64, intent: &AssetIntent) -> Result<()> {
        let assets = self.release_assets(release)?;
        for (name, digest) in &intent.assets {
            let asset = assets.iter().find(|a| &a.name == name).with_context(|| {
                format!("required asset `{name}` is missing; retry with --asset")
            })?;
            if !self.asset_matches(asset, digest)? {
                bail!(
                    "required asset `{name}` is incomplete or has different contents; retry with --asset"
                );
            }
        }
        Ok(())
    }

    fn release_assets(&self, release: u64) -> Result<Vec<Asset>> {
        let mut assets = Vec::new();
        let mut names = HashSet::new();
        for page in 1.. {
            let url = self.url(&format!(
                "/releases/{release}/assets?per_page=100&page={page}"
            ));
            let body = expect_status("GET", &url, 200, self.get(&url)?)?;
            let batch: Vec<Asset> =
                serde_json::from_value(body).context("invalid GitHub asset listing")?;
            let complete = batch.len() < 100;
            for asset in batch {
                if asset.id == 0 || !names.insert(asset.name.clone()) {
                    bail!("GitHub returned invalid or duplicate release assets");
                }
                assets.push(asset);
            }
            if complete {
                break;
            }
        }
        Ok(assets)
    }

    fn asset_matches(&self, asset: &Asset, digest: &str) -> Result<bool> {
        if asset.state != "uploaded" {
            return Ok(false);
        }
        if let Some(actual) = &asset.digest {
            return Ok(actual == digest);
        }
        let url = self.url(&format!("/releases/assets/{}", asset.id));
        let mut response = self
            .headers(self.agent.get(&url))
            .header("accept", "application/octet-stream")
            .call()
            .with_context(|| {
                format!(
                    "cannot download asset `{}` to verify its contents",
                    asset.name
                )
            })?;
        if response.status().as_u16() != 200 {
            bail!(
                "GitHub asset verification failed: HTTP {}",
                response.status()
            );
        }
        let mut hash = Sha256::new();
        let mut reader = response.body_mut().as_reader();
        let mut buffer = [0; 65536];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        Ok(format!("sha256:{:x}", hash.finalize()) == digest)
    }

    fn reconcile_asset(&self, release: u64, local: &LocalAsset, replace: bool) -> Result<()> {
        let assets = self.release_assets(release)?;
        let existing = assets.iter().find(|a| a.name == local.name);
        if let Some(existing) = existing {
            if self.asset_matches(existing, &local.digest)? {
                return Ok(());
            }
            if !replace && existing.state != "starter" {
                bail!(
                    "asset `{}` conflicts with existing contents; use --replace-assets to replace it",
                    local.name
                );
            }
        }
        // Upload and verify under a recoverable name before removing the old asset.
        let staged_name = format!("trellis-upload-{}-{}", &local.digest[7..], local.name);
        let upload_name = if existing.is_some() || assets.iter().any(|a| a.name == staged_name) {
            &staged_name
        } else {
            &local.name
        };
        let staged = assets.iter().find(|a| a.name == *upload_name);
        let uploaded = if let Some(staged) = staged {
            if !self.asset_matches(staged, &local.digest)? {
                if staged.state == "uploaded" {
                    bail!("staged asset `{upload_name}` has conflicting contents");
                }
                self.delete_asset(staged.id)?;
                self.upload_asset(release, upload_name, local)?
            } else {
                staged.id
            }
        } else {
            self.upload_asset(release, upload_name, local)?
        };
        if upload_name != &local.name {
            if let Some(existing) = existing {
                self.delete_asset(existing.id)?;
            }
            let url = self.url(&format!("/releases/assets/{uploaded}"));
            expect_status(
                "PATCH",
                &url,
                200,
                self.send(
                    Write::Patch,
                    &url,
                    &serde_json::json!({ "name": local.name }),
                )?,
            )?;
        }
        Ok(())
    }

    fn upload_asset(&self, release: u64, name: &str, local: &LocalAsset) -> Result<u64> {
        // Never send the token to an upload URL supplied by an API response.
        let base = if self.base == "https://api.github.com" {
            "https://uploads.github.com"
        } else {
            &self.base
        };
        let url = format!(
            "{base}/repos/{}/{}/releases/{release}/assets?name={}",
            self.owner,
            self.repo,
            Self::query_value(name)
        );
        let mut file = local.file.reopen()?;
        file.rewind()?;
        let response = self
            .headers(self.agent.post(&url))
            .header("content-type", "application/octet-stream")
            .header("content-length", file.metadata()?.len().to_string())
            .send(&file)
            .with_context(|| {
                format!(
                    "upload of required asset `{}` failed; retry the same command",
                    local.name
                )
            })?;
        let body = expect_status("POST", &url, 201, read_response(response))?;
        let asset: Asset =
            serde_json::from_value(body).context("invalid uploaded asset response")?;
        if asset.id == 0 || asset.name != name || !self.asset_matches(&asset, &local.digest)? {
            bail!("uploaded asset `{name}` did not pass content verification");
        }
        Ok(asset.id)
    }

    fn delete_asset(&self, asset: u64) -> Result<()> {
        let url = self.url(&format!("/releases/assets/{asset}"));
        let response = self.headers(self.agent.delete(&url)).call()?;
        expect_status("DELETE", &url, 204, read_response(response))?;
        Ok(())
    }
}
