//! Exact tag ownership across every valid SemVer, not just current versions.

use anyhow::{Context, Result};
use regex_automata::{
    Anchored, Input,
    dfa::{Automaton, dense},
};
use std::collections::{HashSet, VecDeque};

pub fn overlap(template: &str, first: &str, second: &str) -> Result<bool> {
    let (prefix, suffix) = template
        .split_once("{version}")
        .context("exact_tag_format has no {version}")?;
    let first_prefix = prefix.replace("{name}", first);
    let second_prefix = prefix.replace("{name}", second);
    let first_suffix = suffix.replace("{name}", first);
    let second_suffix = suffix.replace("{name}", second);
    if !(first_prefix.starts_with(&second_prefix) || second_prefix.starts_with(&first_prefix))
        || !(first_suffix.ends_with(&second_suffix) || second_suffix.ends_with(&first_suffix))
    {
        return Ok(false);
    }
    let version = version_pattern();
    let build = |prefix: &str, suffix: &str| {
        dense::DFA::new(&format!(
            r"\A{}{}{}\z",
            regex::escape(prefix),
            version,
            regex::escape(suffix),
        ))
        .context("cannot check exact tag ownership")
    };
    let first = build(&first_prefix, &first_suffix)?;
    let second = build(&second_prefix, &second_suffix)?;
    let input = Input::new(b"").anchored(Anchored::Yes);
    let start = (
        first.start_state_forward(&input)?,
        second.start_state_forward(&input)?,
    );
    let mut seen = HashSet::from([start]);
    let mut pending = VecDeque::from([start]);
    while let Some((a, b)) = pending.pop_front() {
        if first.is_match_state(first.next_eoi_state(a))
            && second.is_match_state(second.next_eoi_state(b))
        {
            return Ok(true);
        }
        for byte in 0..=u8::MAX {
            let next = (first.next_state(a, byte), second.next_state(b, byte));
            if !first.is_dead_state(next.0) && !second.is_dead_state(next.1) && seen.insert(next) {
                pending.push_back(next);
            }
        }
    }
    Ok(false)
}

fn version_pattern() -> String {
    // semver stores each core component as u64; an overflowing capture has no owner.
    let max = u64::MAX.to_string();
    let mut numbers = vec![
        format!(r"[1-9][0-9]{{0,{}}}", max.len() - 2),
        max.clone(),
        "0".into(),
    ];
    for (index, digit) in max.bytes().enumerate() {
        let low = if index == 0 { b'1' } else { b'0' };
        if digit > low {
            numbers.push(format!(
                "{}[{}-{}][0-9]{{{}}}",
                &max[..index],
                char::from(low),
                char::from(digit - 1),
                max.len() - index - 1,
            ));
        }
    }
    let number = format!("(?:{})", numbers.join("|"));
    let pre = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)";
    let build = r"[0-9A-Za-z-]+";
    format!(r"{number}\.{number}\.{number}(?:-{pre}(?:\.{pre})*)?(?:\+{build}(?:\.{build})*)?")
}

#[cfg(test)]
mod tests {
    use super::{overlap, version_pattern};

    #[test]
    fn namespace_versions_match_the_semver_parser_at_numeric_boundaries() {
        let pattern = regex::Regex::new(&format!(r"\A{}\z", version_pattern())).unwrap();
        for version in [
            "0.0.0",
            "18446744073709551615.18446744073709551615.18446744073709551615",
            "18446744073709551616.0.0",
            "1.01.0",
            "1.2.3-01",
            "1.2.3-01a",
            "1.2.3+01",
            "1.2.3-a+b",
            "1.2.3+a+b",
            "1.2.3-a.",
        ] {
            assert_eq!(
                pattern.is_match(version),
                semver::Version::parse(version).is_ok(),
                "{version}"
            );
        }
    }

    #[test]
    fn exact_ownership_respects_semver_and_both_tag_boundaries() {
        for (template, other, ambiguous) in [
            ("{name}{version}", "app_extra", false),
            ("{name}{version}", "app0", false),
            ("{name}{version}", "app1", true),
            ("{name}{version}", "app1844674407370955162", false),
            ("{name}{version}{name}", "app1.0.0+aapp", false),
            ("{name}{version}{name}", "app1.0.0-aapp", true),
            ("{name}-v{version}", "app_extra", false),
            ("v{version}", "other", true),
        ] {
            assert_eq!(
                overlap(template, "app", other).unwrap(),
                ambiguous,
                "{template}: {other}"
            );
        }
    }
}
