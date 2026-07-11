//! External tool resolution and the retry/backoff policy for Hex-touching
//! steps. The env overrides exist so the e2e suite can substitute fake
//! binaries; they also help unusual installs.

use crate::config::RetryConfig;
use anyhow::{Context, Result, bail};
use std::time::Duration;

pub fn gleam_bin() -> String {
    std::env::var("TRELLIS_GLEAM_BIN").unwrap_or_else(|_| "gleam".to_string())
}

pub fn gh_bin() -> String {
    std::env::var("TRELLIS_GH_BIN").unwrap_or_else(|_| "gh".to_string())
}

/// Parse `"30s"`, `"500ms"`, `"2m"`, or a bare number of seconds.
pub fn parse_duration(text: &str) -> Result<Duration> {
    let text = text.trim();
    let (digits, unit): (&str, &str) = match text.find(|c: char| !c.is_ascii_digit()) {
        Some(pos) => (&text[..pos], text[pos..].trim()),
        None => (text, "s"),
    };
    let value: u64 = digits
        .parse()
        .with_context(|| format!("invalid duration `{text}`"))?;
    match unit {
        "ms" => Ok(Duration::from_millis(value)),
        "s" | "" => Ok(Duration::from_secs(value)),
        "m" => Ok(Duration::from_secs(value * 60)),
        other => bail!("invalid duration unit `{other}` in `{text}` (use ms, s, or m)"),
    }
}

/// Run `operation` up to `policy.attempts` times with exponential backoff —
/// publish.yml's inline `retry()` function as a library. Hex rate limits are
/// why this exists; every Hex-touching step goes through here.
pub fn with_retry<T>(
    policy: &RetryConfig,
    what: &str,
    mut operation: impl FnMut() -> Result<T>,
) -> Result<T> {
    let attempts = policy.attempts.max(1);
    let mut delay = parse_duration(&policy.initial_delay)?;
    let mut last_attempt_error = None;
    for attempt in 1..=attempts {
        match operation() {
            Ok(value) => return Ok(value),
            Err(err) if attempt < attempts => {
                eprintln!(
                    "warning: {what} failed (attempt {attempt}/{attempts}): {err:#}; retrying in {delay:?}"
                );
                std::thread::sleep(delay);
                delay *= policy.multiplier;
            }
            Err(err) => last_attempt_error = Some(err),
        }
    }
    Err(last_attempt_error.expect("loop ends with an error"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_policy(attempts: u32) -> RetryConfig {
        RetryConfig {
            attempts,
            initial_delay: "1ms".to_string(),
            multiplier: 2,
        }
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
        assert!(parse_duration("5h").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn retries_until_success() {
        let mut calls = 0;
        let result = with_retry(&fast_policy(5), "op", || {
            calls += 1;
            if calls < 3 {
                bail!("transient")
            }
            Ok(calls)
        })
        .unwrap();
        assert_eq!(result, 3);
    }

    #[test]
    fn gives_up_after_attempts() {
        let mut calls = 0;
        let err = with_retry(&fast_policy(3), "op", || -> Result<()> {
            calls += 1;
            bail!("always fails")
        })
        .unwrap_err();
        assert_eq!(calls, 3);
        assert!(err.to_string().contains("always fails"));
    }
}
