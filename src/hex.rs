//! Minimal Hex API client, used only for the publish idempotency check:
//! "is this exact version already on Hex?" One GET per package keeps the
//! Hex interaction budget small; everything else is local TOML work.

use anyhow::{Context, Result};

pub struct HexClient {
    base: String,
}

impl HexClient {
    /// Base URL from TRELLIS_HEX_API_URL (tests point this at a local mock),
    /// defaulting to the real Hex API.
    pub fn from_env() -> Self {
        Self {
            base: std::env::var("TRELLIS_HEX_API_URL")
                .unwrap_or_else(|_| "https://hex.pm/api".to_string()),
        }
    }

    /// All published versions of a package; empty when the package has never
    /// been published (a 404 from Hex).
    pub fn published_versions(&self, name: &str) -> Result<Vec<String>> {
        let url = format!("{}/packages/{name}", self.base);
        let response = ureq::get(&url)
            .header("accept", "application/json")
            .header("user-agent", concat!("trellis/", env!("CARGO_PKG_VERSION")))
            .call();
        match response {
            Ok(mut response) => {
                let body: serde_json::Value = response
                    .body_mut()
                    .read_json()
                    .with_context(|| format!("invalid JSON from {url}"))?;
                Ok(body["releases"]
                    .as_array()
                    .map(|releases| {
                        releases
                            .iter()
                            .filter_map(|release| release["version"].as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default())
            }
            Err(ureq::Error::StatusCode(404)) => Ok(Vec::new()),
            Err(err) => Err(err).with_context(|| format!("Hex API request failed: GET {url}")),
        }
    }
}
