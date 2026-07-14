//! Path-dependency rewriting for publish: substitute the Hex requirement
//! derived from each workspace dep's *current* version, per the configured
//! `path-dep-requirement`. The rewrite map is computed from the graph — no
//! hand-maintained list. toml_edit keeps the rest of gleam.toml untouched
//! (the file is restored after publishing either way).

use crate::config::PathDepRequirement;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use toml_edit::{DocumentMut, Value};

#[derive(Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub name: String,
    pub requirement: String,
}

/// The Hex requirement for a path dep at version `X.Y.Z`:
/// minor → `>= X.Y.Z and < (X+1).0.0`, patch → `>= X.Y.Z and < X.(Y+1).0`,
/// exact → `== X.Y.Z`.
pub fn hex_requirement(version: &str, mode: PathDepRequirement) -> Result<String> {
    let version = semver::Version::parse(version)
        .with_context(|| format!("`{version}` is not valid semver"))?;
    Ok(match mode {
        PathDepRequirement::Minor => {
            format!(">= {version} and < {}.0.0", version.major + 1)
        }
        PathDepRequirement::Patch => {
            format!(
                ">= {version} and < {}.{}.0",
                version.major,
                version.minor + 1
            )
        }
        PathDepRequirement::Exact => format!("== {version}"),
    })
}

/// Rewrite every path dep in `[dependencies]` (and releasable ones in
/// `[dev-dependencies]`) to its Hex requirement. `releasable_versions` maps
/// workspace member name → current version for members that will exist on
/// Hex. A `[dependencies]` path dep with no entry there is a hard error —
/// the published package could never resolve it. Dev-only path deps to
/// unreleasable members are left alone: Hex packages don't ship dev deps.
pub fn rewrite_path_deps(
    text: &str,
    releasable_versions: &BTreeMap<String, String>,
    mode: PathDepRequirement,
) -> Result<(String, Vec<Rewrite>)> {
    let mut doc: DocumentMut = text.parse().context("failed to parse gleam.toml")?;
    let mut rewrites = Vec::new();

    for section in ["dependencies", "dev-dependencies"] {
        let Some(table) = doc
            .get_mut(section)
            .and_then(|item| item.as_table_like_mut())
        else {
            continue;
        };
        let dep_names: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
        for name in dep_names {
            let Some(item) = table.get_mut(&name) else {
                continue;
            };
            let is_path_dep = item
                .as_value()
                .and_then(|value| value.as_inline_table())
                .is_some_and(|dep| dep.contains_key("path"));
            if !is_path_dep {
                continue;
            }
            match releasable_versions.get(&name) {
                Some(version) => {
                    let requirement = hex_requirement(version, mode)?;
                    let mut value = Value::from(requirement.clone());
                    if let Some(old) = item.as_value() {
                        *value.decor_mut() = old.decor().clone();
                    }
                    *item = toml_edit::Item::Value(value);
                    rewrites.push(Rewrite { name, requirement });
                }
                None if section == "dependencies" => bail!(
                    "path dependency `{name}` cannot be rewritten: it is not a releasable \
                     workspace member, so the published package could never resolve it on Hex"
                ),
                None => {} // dev-only path dep to an unreleasable member
            }
        }
    }

    Ok((doc.to_string(), rewrites))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn minor_patch_and_exact_requirements() {
        assert_eq!(
            hex_requirement("1.2.3", PathDepRequirement::Minor).unwrap(),
            ">= 1.2.3 and < 2.0.0"
        );
        assert_eq!(
            hex_requirement("0.5.0", PathDepRequirement::Minor).unwrap(),
            ">= 0.5.0 and < 1.0.0"
        );
        assert_eq!(
            hex_requirement("1.2.3", PathDepRequirement::Patch).unwrap(),
            ">= 1.2.3 and < 1.3.0"
        );
        assert_eq!(
            hex_requirement("1.2.3", PathDepRequirement::Exact).unwrap(),
            "== 1.2.3"
        );
        assert!(hex_requirement("not-a-version", PathDepRequirement::Minor).is_err());
    }

    #[test]
    fn rewrites_path_deps_and_preserves_everything_else() {
        let text = concat!(
            "name = \"lat_mid\"\n",
            "version = \"0.5.0\"\n",
            "# keep this comment\n",
            "\n",
            "[dependencies]\n",
            "gleam_stdlib = \">= 0.34.0 and < 2.0.0\"\n",
            "lat_core = { path = \"../lat_core\" }\n",
        );
        let (rewritten, rewrites) = rewrite_path_deps(
            text,
            &versions(&[("lat_core", "1.2.0")]),
            PathDepRequirement::Minor,
        )
        .unwrap();
        assert_eq!(
            rewrites,
            vec![Rewrite {
                name: "lat_core".to_string(),
                requirement: ">= 1.2.0 and < 2.0.0".to_string(),
            }]
        );
        assert!(rewritten.contains("lat_core = \">= 1.2.0 and < 2.0.0\"\n"));
        assert!(rewritten.contains("# keep this comment"));
        assert!(rewritten.contains("gleam_stdlib = \">= 0.34.0 and < 2.0.0\""));
        assert!(!rewritten.contains("path"));
    }

    #[test]
    fn dev_only_path_dep_to_unreleasable_member_is_left_alone() {
        let text = concat!(
            "name = \"lat_cli\"\n",
            "[dependencies]\n",
            "lat_core = { path = \"../lat_core\" }\n",
            "[dev-dependencies]\n",
            "test_helpers = { path = \"../test_helpers\" }\n",
        );
        let (rewritten, rewrites) = rewrite_path_deps(
            text,
            &versions(&[("lat_core", "1.2.0")]),
            PathDepRequirement::Exact,
        )
        .unwrap();
        assert_eq!(rewrites.len(), 1);
        assert!(rewritten.contains("lat_core = \"== 1.2.0\""));
        assert!(rewritten.contains("test_helpers = { path = \"../test_helpers\" }"));
    }

    #[test]
    fn regular_path_dep_to_unreleasable_member_is_an_error() {
        let text = "name = \"app\"\n[dependencies]\nshared = { path = \"../shared\" }\n";
        let err = rewrite_path_deps(text, &versions(&[]), PathDepRequirement::Minor).unwrap_err();
        assert!(err.to_string().contains("shared"));
    }
}
