//! Best-effort "a newer trellis is available" notice.
//!
//! The check hits crates.io for the published crate (`trellis-gleam`; the
//! installed binary is `trellis`), caches the result for a day, and prints a
//! notice to stderr when a newer version exists. It never blocks a command's
//! own output and never turns a failure into an error — any problem (offline,
//! slow network, unparseable response) is swallowed silently.
//!
//! It only runs for interactive humans: suppressed when stderr isn't a
//! terminal, in CI, when `DO_NOT_TRACK` is set (handled by the crate), or when
//! `TRELLIS_NO_UPDATE_CHECK` is set.

use std::io::IsTerminal;
use std::time::Duration;
use tiny_update_check::DetailedUpdateInfo;

/// crates.io crate name. The binary is `trellis`, but the crate publishes as
/// `trellis-gleam` (see Cargo.toml), so the version lives under that name.
const CRATE_NAME: &str = "trellis-gleam";

/// Where to point people once an update exists — the install docs cover every
/// distribution channel (binary, Homebrew, mise), so it beats a bare
/// `cargo install` line.
const RELEASES_URL: &str = "https://github.com/tylerbutler/trellis/releases/latest";

/// Cap the first-of-the-day network call so a slow crates.io can't hang the
/// CLI. On a timeout the crate returns an error, we swallow it, and the next
/// invocation simply tries again.
const CHECK_TIMEOUT: Duration = Duration::from_millis(800);

/// Decide whether an interactive update notice should even be attempted, given
/// the terminal state and an environment lookup. Pure so it can be tested
/// without touching the real environment. `DO_NOT_TRACK` is not checked here —
/// the crate honors it internally and returns no update.
fn notice_enabled(stderr_is_terminal: bool, env: impl Fn(&str) -> Option<String>) -> bool {
    if !stderr_is_terminal {
        return false;
    }
    // Empty values still count as "set" — `CI=` in a workflow means CI.
    if env("CI").is_some() || env("TRELLIS_NO_UPDATE_CHECK").is_some() {
        return false;
    }
    true
}

fn notice_versions(info: &DetailedUpdateInfo) -> Option<(&str, &str)> {
    // The response body is absent on cache hits, so only a fresh daily check
    // produces a notice.
    info.response_body
        .as_ref()
        .map(|_| (info.current.as_str(), info.latest.as_str()))
}

/// Print a notice to stderr if a newer trellis has been published. Best-effort:
/// returns quietly on any error or when suppressed.
pub fn notify() {
    if !notice_enabled(std::io::stderr().is_terminal(), |key| {
        std::env::var(key).ok()
    }) {
        return;
    }

    let checker = tiny_update_check::UpdateChecker::new(CRATE_NAME, env!("CARGO_PKG_VERSION"))
        .timeout(CHECK_TIMEOUT);

    if let Ok(Some(info)) = checker.check_detailed()
        && let Some((current, latest)) = notice_versions(&info)
    {
        eprintln!();
        eprintln!("A new release of trellis is available: {current} → {latest}");
        eprintln!("{RELEASES_URL}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tiny_update_check::UpdateInfo;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn enabled_for_interactive_terminal_without_ci() {
        assert!(notice_enabled(true, env_from(&[])));
    }

    #[test]
    fn suppressed_when_not_a_terminal() {
        assert!(!notice_enabled(false, env_from(&[])));
    }

    #[test]
    fn suppressed_in_ci() {
        assert!(!notice_enabled(true, env_from(&[("CI", "true")])));
    }

    #[test]
    fn suppressed_when_ci_is_set_but_empty() {
        assert!(!notice_enabled(true, env_from(&[("CI", "")])));
    }

    #[test]
    fn suppressed_by_opt_out_env() {
        assert!(!notice_enabled(
            true,
            env_from(&[("TRELLIS_NO_UPDATE_CHECK", "1")])
        ));
    }

    fn update_info(response_body: Option<&str>) -> DetailedUpdateInfo {
        let mut info = DetailedUpdateInfo::from(UpdateInfo {
            current: "0.13.0".to_string(),
            latest: "0.14.0".to_string(),
        });
        info.response_body = response_body.map(str::to_string);
        info
    }

    #[test]
    fn suppresses_update_notice_on_cache_hit() {
        assert_eq!(notice_versions(&update_info(None)), None);
    }

    #[test]
    fn shows_update_notice_after_fresh_check() {
        assert_eq!(
            notice_versions(&update_info(Some("{}"))),
            Some(("0.13.0", "0.14.0"))
        );
    }
}
