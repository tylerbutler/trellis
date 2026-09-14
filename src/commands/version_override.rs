//! Overriding the version a plan would otherwise derive.
//!
//! The next version normally comes entirely from the kinds of the pending
//! fragments — the largest bump wins. Three things that rule cannot express:
//!
//! - a change filed as `Fixed` that is actually breaking, when the fragment is
//!   already merged (`--bump`);
//! - a jump straight to `1.0.0` from `0.9.3`, or a one-off version matching an
//!   upstream number (`--set`);
//! - a release candidate on the way to a final (`--pre`).
//!
//! All three are the same shape of problem — the fragment kinds don't determine
//! the version I want — so they share one flag surface, parsed here once and
//! used identically by `version plan` and `version apply`.

use crate::changelog;
use crate::config::Bump;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

/// What `--pre` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreChoice {
    /// `--pre rc` — cut (or continue) a prerelease cycle under this label.
    Label(String),
    /// `--pre none` — promote the current prerelease to its final version.
    Promote,
}

/// The reserved `--pre` value meaning "promote". A label is a semver
/// identifier, and `none` is a legal one, so this shadows a label nobody
/// sensibly uses rather than colliding with one.
const PROMOTE: &str = "none";

#[derive(Debug, Default)]
pub struct Overrides {
    /// `--bump <level>`, applying to every package in the plan.
    global_bump: Option<Bump>,
    /// `--bump <pkg>=<level>`.
    per_package_bump: BTreeMap<String, Bump>,
    /// `--set <pkg>=<version>`.
    pinned: BTreeMap<String, semver::Version>,
    pre: Option<PreChoice>,
}

impl Overrides {
    /// Parse the raw flag values. Everything checkable without a workspace is
    /// checked here; whether the named packages exist and are releasable is
    /// `version::validate_named_packages`, which has one.
    pub fn parse(bump: &[String], set: &[String], pre: Option<&str>) -> Result<Self> {
        let mut overrides = Self::default();
        for spec in bump {
            match spec.split_once('=') {
                None => {
                    let level = parse_bump(spec)?;
                    if overrides.global_bump.replace(level).is_some() {
                        bail!("--bump was given more than one workspace-wide level");
                    }
                }
                Some((package, level)) => {
                    let level = parse_bump(level)?;
                    if overrides
                        .per_package_bump
                        .insert(package.to_string(), level)
                        .is_some()
                    {
                        bail!("--bump names `{package}` more than once");
                    }
                }
            }
        }
        for spec in set {
            let Some((package, version)) = spec.split_once('=') else {
                bail!("--set needs `<package>=<version>`, got `{spec}`");
            };
            let version = semver::Version::parse(version).map_err(|err| {
                anyhow::anyhow!("--set {package}: `{version}` is not semver: {err}")
            })?;
            if overrides
                .pinned
                .insert(package.to_string(), version)
                .is_some()
            {
                bail!("--set names `{package}` more than once");
            }
        }
        // Both name one package's next version, so honoring both would mean
        // picking a winner silently.
        for package in overrides.pinned.keys() {
            if overrides.per_package_bump.contains_key(package) {
                bail!("`{package}` is named by both --bump and --set; use one");
            }
        }
        overrides.pre = match pre {
            None => None,
            Some(PROMOTE) => Some(PreChoice::Promote),
            Some(label) => {
                validate_label(label)?;
                Some(PreChoice::Label(label.to_string()))
            }
        };
        Ok(overrides)
    }

    /// Whether fragments survive this release.
    ///
    /// A prerelease renders its changelog section but does not retire the
    /// fragments behind it: they are still unreleased as far as the final
    /// version is concerned, and consuming them at `rc.1` would leave the
    /// eventual `1.0.0` with nothing to say. The final release — including a
    /// `--pre none` promotion — consumes them normally.
    pub fn retains_fragments(&self) -> bool {
        matches!(self.pre, Some(PreChoice::Label(_)))
    }

    pub fn promoting(&self) -> bool {
        self.pre == Some(PreChoice::Promote)
    }

    /// Every package named by a `--bump pkg=` or `--set pkg=`, so the caller can
    /// reject names that are not releasable members before anything is written.
    pub fn named_packages(&self) -> impl Iterator<Item = &str> {
        self.per_package_bump
            .keys()
            .chain(self.pinned.keys())
            .map(String::as_str)
    }

    /// The next version for one package.
    ///
    /// `derived` is what the fragments alone call for; it is ignored on the
    /// paths that do not consult them (`--set`, and continuing or promoting a
    /// prerelease cycle).
    pub fn resolve(
        &self,
        package: &str,
        current: &semver::Version,
        derived: Bump,
    ) -> Result<semver::Version> {
        let next = match &self.pre {
            Some(PreChoice::Promote) => self.promote(package, current, derived)?,
            Some(PreChoice::Label(label)) => self.prerelease(package, current, derived, label)?,
            None => {
                // Reachable only once prereleases exist, and silently bumping
                // from the base would drop the cycle on the floor.
                if !current.pre.is_empty() {
                    bail!(
                        "`{package}` is at prerelease {current}; pass --pre <label> for another \
                         prerelease or --pre none to promote it"
                    );
                }
                self.base(package, current, derived)
            }
        };
        // Catches an override that would move a version backwards — `--set` to
        // an older number, or a label whose cycle has already passed.
        if next <= *current {
            bail!("`{package}` would go from {current} to {next}, which is not forward");
        }
        Ok(next)
    }

    /// The release version before any prerelease label is attached.
    fn base(&self, package: &str, current: &semver::Version, derived: Bump) -> semver::Version {
        if let Some(pinned) = self.pinned.get(package) {
            return pinned.clone();
        }
        let bump = self
            .per_package_bump
            .get(package)
            .or(self.global_bump.as_ref())
            .copied()
            .unwrap_or(derived);
        changelog::apply_bump(current, bump)
    }

    /// Whether `--bump pkg=` or `--set pkg=` named this package.
    fn names(&self, package: &str) -> bool {
        self.pinned.contains_key(package) || self.per_package_bump.contains_key(package)
    }

    /// `--pre none`: a package already in a cycle drops its label and releases;
    /// one that gained fragments after the RC was cut bumps normally, so a late
    /// arrival does not block the promotion.
    fn promote(
        &self,
        package: &str,
        current: &semver::Version,
        derived: Bump,
    ) -> Result<semver::Version> {
        if current.pre.is_empty() {
            return Ok(self.base(package, current, derived));
        }
        if self.names(package) {
            bail!(
                "`{package}` is named by --bump or --set as well as --pre none; promoting takes \
                 the version the prerelease was already working toward"
            );
        }
        Ok(release_of(current))
    }

    /// `--pre <label>`: compute the base being worked toward, then number the
    /// candidate within it.
    fn prerelease(
        &self,
        package: &str,
        current: &semver::Version,
        derived: Bump,
        label: &str,
    ) -> Result<semver::Version> {
        let base = if self.names(package) {
            // An explicit --bump/--set retargets the cycle even mid-flight,
            // measured from the base rather than from `rc.1` itself.
            self.base(package, &release_of(current), derived)
        } else if !current.pre.is_empty() {
            // Already in a cycle: the base was decided when it was cut, and
            // re-deriving it from the fragments would bump twice.
            release_of(current)
        } else {
            self.base(package, current, derived)
        };

        let counter = match previous_counter(current, label) {
            Some(previous) if base == release_of(current) => previous + 1,
            _ => 1,
        };
        let mut next = base;
        next.pre = semver::Prerelease::new(&format!("{label}.{counter}"))
            .map_err(|err| anyhow::anyhow!("`{label}` is not a usable prerelease label: {err}"))?;
        Ok(next)
    }
}

/// The release version `current` is a prerelease of — or itself, if it is one.
fn release_of(current: &semver::Version) -> semver::Version {
    semver::Version::new(current.major, current.minor, current.patch)
}

/// The `N` in a current version of `X.Y.Z-<label>.N`, when the label matches.
fn previous_counter(current: &semver::Version, label: &str) -> Option<u64> {
    current
        .pre
        .as_str()
        .strip_prefix(label)?
        .strip_prefix('.')?
        .parse()
        .ok()
}

fn parse_bump(level: &str) -> Result<Bump> {
    match level {
        "major" => Ok(Bump::Major),
        "minor" => Ok(Bump::Minor),
        "patch" => Ok(Bump::Patch),
        other => bail!("unknown bump level `{other}`; expected major, minor, or patch"),
    }
}

/// Reject a label that would not survive being written into a version. The
/// numeric counter is appended by trellis, so the label itself carries no dot.
fn validate_label(label: &str) -> Result<()> {
    if label.is_empty() {
        bail!("--pre needs a label, e.g. `--pre rc`");
    }
    if label.contains('.') {
        bail!("prerelease label `{label}` must not contain a dot; trellis appends the counter");
    }
    semver::Prerelease::new(&format!("{label}.1"))
        .map_err(|err| anyhow::anyhow!("`{label}` is not a usable prerelease label: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> semver::Version {
        semver::Version::parse(text).unwrap()
    }

    fn overrides(bump: &[&str], set: &[&str], pre: Option<&str>) -> Overrides {
        let bump: Vec<String> = bump.iter().map(|&s| s.to_string()).collect();
        let set: Vec<String> = set.iter().map(|&s| s.to_string()).collect();
        Overrides::parse(&bump, &set, pre).unwrap()
    }

    #[test]
    fn nothing_given_leaves_the_derived_bump_alone() {
        let o = overrides(&[], &[], None);
        assert_eq!(
            o.resolve("p", &v("1.2.0"), Bump::Minor).unwrap(),
            v("1.3.0")
        );
    }

    #[test]
    fn global_bump_overrides_the_derived_level() {
        // The motivating case: a breaking change filed as `Fixed`.
        let o = overrides(&["major"], &[], None);
        assert_eq!(
            o.resolve("p", &v("1.2.3"), Bump::Patch).unwrap(),
            v("2.0.0")
        );
    }

    #[test]
    fn per_package_bump_wins_over_the_global_one() {
        let o = overrides(&["minor", "p=major"], &[], None);
        assert_eq!(
            o.resolve("p", &v("1.2.3"), Bump::Patch).unwrap(),
            v("2.0.0")
        );
        assert_eq!(
            o.resolve("q", &v("1.2.3"), Bump::Patch).unwrap(),
            v("1.3.0")
        );
    }

    #[test]
    fn set_pins_the_exact_version() {
        // 0.9.3 -> 1.0.0 is a minor under the pre-1.0 rule, not the release
        // anyone means.
        let o = overrides(&[], &["p=1.0.0"], None);
        assert_eq!(
            o.resolve("p", &v("0.9.3"), Bump::Minor).unwrap(),
            v("1.0.0")
        );
    }

    #[test]
    fn a_backwards_override_is_refused() {
        let o = overrides(&[], &["p=0.9.0"], None);
        let err = o.resolve("p", &v("1.2.3"), Bump::Patch).unwrap_err();
        assert!(err.to_string().contains("not forward"), "{err}");
    }

    #[test]
    fn pre_labels_the_derived_version() {
        let o = overrides(&[], &[], Some("rc"));
        assert_eq!(
            o.resolve("p", &v("0.9.3"), Bump::Minor).unwrap(),
            v("0.10.0-rc.1")
        );
    }

    #[test]
    fn pre_combines_with_an_exact_version() {
        let o = overrides(&[], &["p=1.0.0"], Some("rc"));
        assert_eq!(
            o.resolve("p", &v("0.9.3"), Bump::Minor).unwrap(),
            v("1.0.0-rc.1")
        );
    }

    #[test]
    fn repeating_pre_increments_within_the_same_base() {
        // The whole point of the cycle: rc.1 -> rc.2 stays on 1.0.0 instead of
        // deriving a fresh bump from the same fragments every time.
        let o = overrides(&[], &[], Some("rc"));
        assert_eq!(
            o.resolve("p", &v("1.0.0-rc.1"), Bump::Minor).unwrap(),
            v("1.0.0-rc.2")
        );
        assert_eq!(
            o.resolve("p", &v("1.0.0-rc.9"), Bump::Minor).unwrap(),
            v("1.0.0-rc.10")
        );
    }

    #[test]
    fn switching_label_restarts_the_counter_on_the_same_base() {
        let o = overrides(&[], &[], Some("rc"));
        assert_eq!(
            o.resolve("p", &v("1.0.0-beta.3"), Bump::Minor).unwrap(),
            v("1.0.0-rc.1")
        );
    }

    #[test]
    fn retargeting_mid_cycle_restarts_the_counter() {
        let o = overrides(&["p=major"], &[], Some("rc"));
        assert_eq!(
            o.resolve("p", &v("1.0.0-rc.4"), Bump::Minor).unwrap(),
            v("2.0.0-rc.1")
        );
    }

    #[test]
    fn promote_drops_the_label_without_bumping() {
        let o = overrides(&[], &[], Some("none"));
        assert_eq!(
            o.resolve("p", &v("1.0.0-rc.2"), Bump::Minor).unwrap(),
            v("1.0.0")
        );
    }

    #[test]
    fn promote_still_bumps_a_package_that_was_not_in_the_cycle() {
        // A package that gained fragments after the RC was cut must not block
        // the promotion of the ones that were.
        let o = overrides(&[], &[], Some("none"));
        assert_eq!(
            o.resolve("p", &v("0.5.0"), Bump::Patch).unwrap(),
            v("0.5.1")
        );
    }

    #[test]
    fn promote_refuses_to_also_take_a_bump() {
        let o = overrides(&["p=major"], &[], Some("none"));
        let err = o.resolve("p", &v("1.0.0-rc.1"), Bump::Minor).unwrap_err();
        assert!(err.to_string().contains("--pre none"), "{err}");
    }

    #[test]
    fn a_prerelease_must_be_resolved_explicitly() {
        // Without this, a plain `version apply` on 1.0.0-rc.1 would compute
        // 1.1.0 and lose the cycle silently.
        let o = overrides(&[], &[], None);
        let err = o.resolve("p", &v("1.0.0-rc.1"), Bump::Minor).unwrap_err();
        assert!(err.to_string().contains("--pre none"), "{err}");
    }

    #[test]
    fn conflicting_overrides_are_rejected_at_parse_time() {
        assert!(
            Overrides::parse(&["p=major".into()], &["p=1.0.0".into()], None)
                .unwrap_err()
                .to_string()
                .contains("both --bump and --set")
        );
        assert!(
            Overrides::parse(&["major".into(), "minor".into()], &[], None)
                .unwrap_err()
                .to_string()
                .contains("more than one workspace-wide level")
        );
        assert!(
            Overrides::parse(&["sideways".into()], &[], None)
                .unwrap_err()
                .to_string()
                .contains("unknown bump level")
        );
        assert!(
            Overrides::parse(&[], &["p=notaversion".into()], None)
                .unwrap_err()
                .to_string()
                .contains("not semver")
        );
        assert!(
            Overrides::parse(&[], &["lat_core".into()], None)
                .unwrap_err()
                .to_string()
                .contains("<package>=<version>")
        );
        assert!(
            Overrides::parse(&[], &[], Some("rc.1"))
                .unwrap_err()
                .to_string()
                .contains("must not contain a dot")
        );
    }

    #[test]
    fn fragments_survive_a_prerelease_but_not_a_promotion() {
        assert!(overrides(&[], &[], Some("rc")).retains_fragments());
        assert!(!overrides(&[], &[], Some("none")).retains_fragments());
        assert!(!overrides(&[], &[], None).retains_fragments());
    }
}
