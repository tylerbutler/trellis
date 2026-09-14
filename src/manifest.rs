//! Surgical reads and writes of a single string scalar in a manifest, at a
//! dotted key path, for the three formats a workspace member's manifest can be
//! written in.
//!
//! "Surgical" is the same promise [`crate::lockfile`] makes for `manifest.toml`:
//! every byte the edit does not target survives verbatim — comments, key order,
//! indentation, quote style, trailing newline. A hand-maintained
//! `.claude-plugin/plugin.json` or `apm.yml` is a file its owner reads, so a
//! version bump may not reflow it.
//!
//! TOML goes through `toml_edit` and the decor-clone idiom used everywhere else
//! in the codebase. JSON and YAML share one implementation: YAML 1.2 is a strict
//! superset of JSON, so a single `saphyr-parser` event walk locates the target
//! scalar in both, and the edit is a byte splice.
//!
//! Two facts about `saphyr-parser` that this module works around, both verified
//! against 0.0.12 rather than taken from its docs:
//!
//! * [`Marker::index`] is a **char** index, not a byte offset, despite what its
//!   doc comment says — hence [`byte_offset`].
//! * A quoted scalar's `span.end` in YAML block context runs to end-of-line,
//!   swallowing trailing whitespace and any comment. Only `span.start` and the
//!   [`ScalarStyle`] are trustworthy, so [`scalar_extent`] re-lexes the scalar
//!   from its start rather than believing the end marker.

use anyhow::{Context, Result, bail};
use saphyr_parser::{Event, Parser, ScalarStyle, Span, SpannedEventReceiver};
use std::path::Path;

/// The manifest formats a member's manifest may be written in, derived from the
/// file extension — there is no format key to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Toml,
    Json,
    Yaml,
}

impl Format {
    /// Every extension trellis recognises, for error messages.
    pub const EXTENSIONS: [&'static str; 4] = ["toml", "json", "yaml", "yml"];

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("toml") => Ok(Self::Toml),
            Some("json") => Ok(Self::Json),
            Some("yaml" | "yml") => Ok(Self::Yaml),
            _ => bail!(
                "cannot tell the manifest format of `{}` from its extension; expected one of {}",
                path.display(),
                Self::EXTENSIONS.join(", ")
            ),
        }
    }
}

/// The string scalar at `path`, or `None` when no such key exists.
///
/// A path resolving to a mapping, a sequence, or a non-string scalar is an
/// error rather than a `None` — silently reporting "absent" for a key that is
/// present but the wrong shape would turn a typo'd adapter path into a missing
/// version.
pub fn read_string(text: &str, format: Format, path: &str) -> Result<Option<String>> {
    match format {
        Format::Toml => toml_read(text, path),
        Format::Json | Format::Yaml => Ok(locate(text, path)?.map(|hit| hit.value)),
    }
}

/// Replace the string scalar at `path` with `value`, leaving every other byte
/// of `text` untouched.
///
/// Errors when `path` names nothing: this never inserts a key. A manifest
/// missing the field the adapter points at is a configuration problem to
/// report, not a hole to fill in silently.
pub fn write_string(text: &str, format: Format, path: &str, value: &str) -> Result<String> {
    match format {
        Format::Toml => toml_write(text, path, value),
        Format::Json | Format::Yaml => {
            let Some(hit) = locate(text, path)? else {
                bail!("no `{path}` field");
            };
            let range = scalar_extent(text, &hit)?;
            let replacement = match format {
                // A JSON scalar is always a double-quoted string, whatever the
                // YAML scanner called its style.
                Format::Json => serde_json::to_string(value).expect("a str always serialises"),
                _ => yaml_scalar(value, hit.style),
            };
            let mut out = String::with_capacity(text.len() + replacement.len());
            out.push_str(&text[..range.0]);
            out.push_str(&replacement);
            out.push_str(&text[range.1..]);
            Ok(out)
        }
    }
}

// --- TOML -------------------------------------------------------------------

fn toml_read(text: &str, path: &str) -> Result<Option<String>> {
    let doc: toml_edit::DocumentMut = text.parse().context("failed to parse TOML")?;
    let mut item = doc.as_item();
    for key in path.split('.') {
        match item.get(key) {
            // `Item::None` is toml_edit's placeholder for a key that was never
            // there, which indexing hands back rather than a `None`.
            Some(next) if !next.is_none() => item = next,
            _ => return Ok(None),
        }
    }
    match item.as_str() {
        Some(value) => Ok(Some(value.to_string())),
        None => bail!("`{path}` is not a string"),
    }
}

fn toml_write(text: &str, path: &str, value: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = text.parse().context("failed to parse TOML")?;
    let mut item = doc.as_item_mut();
    for key in path.split('.') {
        match item.get_mut(key) {
            Some(next) if !next.is_none() => item = next,
            _ => bail!("no `{path}` field"),
        }
    }
    let Some(existing) = item.as_value_mut() else {
        bail!("`{path}` is not a string");
    };
    if existing.as_str().is_none() {
        bail!("`{path}` is not a string");
    }
    let mut replacement = toml_edit::Value::from(value);
    // Carry the original's surrounding whitespace and comments across, the way
    // `lockfile::patch_locked_versions` and `rewrite::rewrite_path_deps` do.
    *replacement.decor_mut() = existing.decor().clone();
    *existing = replacement;
    Ok(doc.to_string())
}

// --- JSON and YAML ----------------------------------------------------------

/// Path segment standing for "an element of a sequence". Dotted paths address
/// mapping keys only, and no real key can contain a NUL, so pushing this makes
/// everything under a sequence unmatchable rather than accidentally matched.
const SEQUENCE_SEGMENT: &str = "\0";

struct Hit {
    value: String,
    style: ScalarStyle,
    /// Char index of the scalar's first character in the source.
    start: usize,
}

/// What a container in the event stream is, and — for a mapping — whether the
/// next scalar is a key or a value.
enum Container {
    Mapping { expecting_key: bool },
    Sequence,
}

struct Locator<'a> {
    target: Vec<&'a str>,
    path: Vec<String>,
    stack: Vec<Container>,
    /// Whether each open container pushed a segment onto `path` (the document
    /// root does not).
    owns_segment: Vec<bool>,
    pending_key: Option<String>,
    hit: Option<Hit>,
    /// A value at the target path that is not a string scalar.
    wrong_shape: bool,
}

impl<'a> Locator<'a> {
    fn new(target: &'a str) -> Self {
        Self {
            target: target.split('.').collect(),
            path: Vec::new(),
            stack: Vec::new(),
            owns_segment: Vec::new(),
            pending_key: None,
            hit: None,
            wrong_shape: false,
        }
    }

    /// The path segment naming the value about to be visited, consuming the
    /// pending mapping key when there is one.
    fn value_segment(&mut self) -> String {
        match self.stack.last() {
            Some(Container::Mapping { .. }) => self.pending_key.take().unwrap_or_default(),
            _ => SEQUENCE_SEGMENT.to_string(),
        }
    }

    fn at_target(&self) -> bool {
        self.path.len() == self.target.len()
            && self.path.iter().zip(&self.target).all(|(a, b)| a == b)
    }

    /// A mapping's next scalar is a key again, once its current value is done.
    fn value_done(&mut self) {
        if let Some(Container::Mapping { expecting_key }) = self.stack.last_mut() {
            *expecting_key = true;
        }
    }
}

impl<'i> SpannedEventReceiver<'i> for Locator<'_> {
    fn on_event(&mut self, event: Event<'i>, span: Span) {
        if self.hit.is_some() || self.wrong_shape {
            return;
        }
        match event {
            Event::Scalar(value, style, _, _) => {
                if let Some(Container::Mapping { expecting_key }) = self.stack.last_mut()
                    && *expecting_key
                {
                    *expecting_key = false;
                    self.pending_key = Some(value.into_owned());
                    return;
                }
                let segment = self.value_segment();
                self.path.push(segment);
                if self.at_target() {
                    self.hit = Some(Hit {
                        value: value.into_owned(),
                        style,
                        start: span.start.index(),
                    });
                }
                self.path.pop();
                self.value_done();
            }
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                let owns = !self.stack.is_empty();
                if owns {
                    let segment = self.value_segment();
                    self.path.push(segment);
                    // A container sitting at the target path is a value of the
                    // wrong shape, not an absent key.
                    if self.at_target() {
                        self.wrong_shape = true;
                    }
                }
                self.owns_segment.push(owns);
                self.stack.push(match event {
                    Event::MappingStart(..) => Container::Mapping {
                        expecting_key: true,
                    },
                    _ => Container::Sequence,
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                self.stack.pop();
                if self.owns_segment.pop().unwrap_or(false) {
                    self.path.pop();
                }
                self.value_done();
            }
            // An alias resolves to a value trellis cannot splice in place; the
            // path simply does not match through one.
            Event::Alias(_) => {
                let segment = self.value_segment();
                self.path.push(segment);
                if self.at_target() {
                    self.wrong_shape = true;
                }
                self.path.pop();
                self.value_done();
            }
            _ => {}
        }
    }
}

fn locate(text: &str, path: &str) -> Result<Option<Hit>> {
    if path.is_empty() {
        bail!("empty field path");
    }
    let mut locator = Locator::new(path);
    Parser::new_from_str(text)
        .load(&mut locator, false)
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    if locator.wrong_shape {
        bail!("`{path}` is not a string");
    }
    Ok(locator.hit)
}

/// The byte range the scalar at `hit` occupies in `text`, quotes included.
///
/// Re-lexed from the start marker rather than read off `span.end`, which a
/// quoted scalar in YAML block context reports as end-of-line.
fn scalar_extent(text: &str, hit: &Hit) -> Result<(usize, usize)> {
    let start = byte_offset(text, hit.start)
        .with_context(|| format!("scalar at char {} is out of range", hit.start))?;
    let rest = &text[start..];
    let end = match hit.style {
        ScalarStyle::Plain => {
            if !rest.starts_with(&hit.value) {
                bail!("cannot locate the end of the value (multi-line plain scalar?)");
            }
            start + hit.value.len()
        }
        ScalarStyle::SingleQuoted => start + quoted_len(rest, b'\'', false)?,
        ScalarStyle::DoubleQuoted => start + quoted_len(rest, b'"', true)?,
        ScalarStyle::Literal | ScalarStyle::Folded => {
            bail!("cannot rewrite a block scalar in place");
        }
    };
    Ok((start, end))
}

/// Length in bytes of the quoted scalar at the head of `text`, closing quote
/// included. `backslash_escapes` distinguishes double-quoted style (`\"`) from
/// single-quoted style, where a literal quote is written `''`.
fn quoted_len(text: &str, quote: u8, backslash_escapes: bool) -> Result<usize> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&quote) {
        bail!("expected a quoted value");
    }
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if backslash_escapes => i += 2,
            b if b == quote => {
                if !backslash_escapes && bytes.get(i + 1) == Some(&quote) {
                    i += 2;
                } else {
                    return Ok(i + 1);
                }
            }
            _ => i += 1,
        }
    }
    bail!("unterminated quoted value")
}

/// Byte offset of the `char_index`-th character, or the end of the string when
/// the index is exactly its char length.
fn byte_offset(text: &str, char_index: usize) -> Option<usize> {
    text.char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(text.len()))
        .nth(char_index)
}

/// `value` written in YAML, keeping `style` when it can still express the value.
fn yaml_scalar(value: &str, style: ScalarStyle) -> String {
    match style {
        ScalarStyle::SingleQuoted => format!("'{}'", value.replace('\'', "''")),
        ScalarStyle::Plain if plain_is_safe(value) => value.to_string(),
        // JSON's escaping is a subset of YAML's double-quoted style, so a JSON
        // string literal is always a valid YAML one.
        _ => serde_json::to_string(value).expect("a str always serialises"),
    }
}

/// Whether `value` survives being written as a YAML plain scalar — no quoting
/// needed, and no risk of the reader resolving it to a bool, null, or number.
fn plain_is_safe(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')) {
        return false;
    }
    // The YAML 1.1 boolean and null spellings, which readers in the wild still
    // resolve, plus anything a reader would hand back as a number.
    const RESERVED: [&str; 10] = [
        "true", "false", "yes", "no", "on", "off", "y", "n", "null", "nan",
    ];
    let lowered = value.to_ascii_lowercase();
    !RESERVED.contains(&lowered.as_str()) && value.parse::<f64>().is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_comes_from_the_extension() {
        assert_eq!(Format::from_path("gleam.toml").unwrap(), Format::Toml);
        assert_eq!(
            Format::from_path(".claude-plugin/plugin.json").unwrap(),
            Format::Json
        );
        assert_eq!(Format::from_path("apm.yml").unwrap(), Format::Yaml);
        assert_eq!(Format::from_path("a/b.YAML").unwrap(), Format::Yaml);
        let err = Format::from_path("Cargo.lock").unwrap_err().to_string();
        assert!(err.contains("toml, json, yaml, yml"), "{err}");
        assert!(Format::from_path("plugin").is_err());
    }

    // --- YAML ---------------------------------------------------------------

    const APM: &str = "\
# An APM package.
name: my-pkg
version: \"1.2.0\"  # keep this comment
dependencies:
  apm:
    - microsoft/apm-sample-package#v1.0.0
nested:
  deep:
    key: plain-value
";

    #[test]
    fn reads_yaml_scalars_at_dotted_paths() {
        assert_eq!(
            read_string(APM, Format::Yaml, "name").unwrap().as_deref(),
            Some("my-pkg")
        );
        assert_eq!(
            read_string(APM, Format::Yaml, "version")
                .unwrap()
                .as_deref(),
            Some("1.2.0")
        );
        assert_eq!(
            read_string(APM, Format::Yaml, "nested.deep.key")
                .unwrap()
                .as_deref(),
            Some("plain-value")
        );
        assert_eq!(read_string(APM, Format::Yaml, "missing").unwrap(), None);
        assert_eq!(
            read_string(APM, Format::Yaml, "nested.deep.nope").unwrap(),
            None
        );
    }

    /// The load-bearing case: a quoted scalar whose `span.end` the parser
    /// reports as end-of-line, comment and all.
    #[test]
    fn yaml_edit_keeps_the_trailing_comment_and_everything_else() {
        let out = write_string(APM, Format::Yaml, "version", "1.3.0").unwrap();
        assert_eq!(out, APM.replace("\"1.2.0\"", "\"1.3.0\""));
        assert!(out.contains("version: \"1.3.0\"  # keep this comment"));
    }

    #[test]
    fn yaml_edit_of_a_plain_scalar_stays_plain() {
        let out = write_string(APM, Format::Yaml, "nested.deep.key", "other").unwrap();
        assert!(out.contains("key: other\n"), "{out}");
        assert_eq!(out, APM.replace("plain-value", "other"));
    }

    #[test]
    fn yaml_quotes_a_plain_scalar_when_the_value_needs_it() {
        let out = write_string(APM, Format::Yaml, "nested.deep.key", "yes").unwrap();
        assert!(out.contains("key: \"yes\"\n"), "{out}");
        let out = write_string(APM, Format::Yaml, "nested.deep.key", "a: b").unwrap();
        assert!(out.contains("key: \"a: b\"\n"), "{out}");
    }

    #[test]
    fn yaml_single_quoted_style_survives() {
        let text = "version: '1.2.0' # c\n";
        let out = write_string(text, Format::Yaml, "version", "1.3.0").unwrap();
        assert_eq!(out, "version: '1.3.0' # c\n");
        let out = write_string(text, Format::Yaml, "version", "it's").unwrap();
        assert_eq!(out, "version: 'it''s' # c\n");
    }

    #[test]
    fn yaml_multibyte_content_before_the_target_does_not_shift_the_splice() {
        let text = "description: naïve — em dash, ümlaut\nversion: \"1.2.0\"\n";
        let out = write_string(text, Format::Yaml, "version", "1.3.0").unwrap();
        assert_eq!(out, text.replace("1.2.0", "1.3.0"));
    }

    // --- JSON ---------------------------------------------------------------

    const PLUGIN: &str = "{\n  \"name\": \"code-reviewer\",\n  \"version\": \"1.2.0\",\n  \
                          \"author\": { \"name\": \"someone\" },\n  \"keywords\": [\"a\", \"b\"]\n}\n";

    #[test]
    fn reads_and_writes_json() {
        assert_eq!(
            read_string(PLUGIN, Format::Json, "name")
                .unwrap()
                .as_deref(),
            Some("code-reviewer")
        );
        assert_eq!(
            read_string(PLUGIN, Format::Json, "author.name")
                .unwrap()
                .as_deref(),
            Some("someone")
        );
        let out = write_string(PLUGIN, Format::Json, "version", "1.3.0").unwrap();
        assert_eq!(out, PLUGIN.replace("1.2.0", "1.3.0"));
    }

    #[test]
    fn json_indentation_style_is_irrelevant() {
        for text in [
            "{\n\t\"version\": \"1.2.0\"\n}\n",
            "{\"version\":\"1.2.0\"}",
            "{\n    \"a\": 1,\n    \"version\": \"1.2.0\"\n}",
        ] {
            let out = write_string(text, Format::Json, "version", "1.3.0").unwrap();
            assert_eq!(out, text.replace("1.2.0", "1.3.0"));
        }
    }

    #[test]
    fn json_values_are_escaped_on_write() {
        let out = write_string(PLUGIN, Format::Json, "name", "a\"b\\c").unwrap();
        assert!(out.contains(r#""name": "a\"b\\c","#), "{out}");
        assert_eq!(
            read_string(&out, Format::Json, "name").unwrap().as_deref(),
            Some("a\"b\\c")
        );
    }

    #[test]
    fn json_escapes_before_the_target_do_not_confuse_the_scan() {
        let text = r#"{"a": "quote \" and backslash \\", "version": "1.2.0"}"#;
        let out = write_string(text, Format::Json, "version", "1.3.0").unwrap();
        assert_eq!(out, text.replace("1.2.0", "1.3.0"));
    }

    #[test]
    fn sequence_elements_never_satisfy_a_dotted_path() {
        // `keywords.a` would match if sequence entries were transparent.
        assert_eq!(
            read_string(PLUGIN, Format::Json, "keywords.a").unwrap(),
            None
        );
        assert_eq!(
            read_string(APM, Format::Yaml, "dependencies.apm.microsoft").unwrap(),
            None
        );
    }

    // --- shared behaviour ---------------------------------------------------

    #[test]
    fn a_path_naming_a_container_is_an_error_not_an_absence() {
        for (text, format, path) in [
            (PLUGIN, Format::Json, "author"),
            (PLUGIN, Format::Json, "keywords"),
            (APM, Format::Yaml, "nested"),
            (APM, Format::Yaml, "nested.deep"),
            (APM, Format::Yaml, "dependencies.apm"),
        ] {
            let err = read_string(text, format, path).unwrap_err().to_string();
            assert!(err.contains("is not a string"), "{path}: {err}");
        }
    }

    #[test]
    fn writing_a_missing_path_never_inserts() {
        for (text, format) in [
            (PLUGIN, Format::Json),
            (APM, Format::Yaml),
            ("version = \"1.2.0\"\n", Format::Toml),
        ] {
            let err = write_string(text, format, "nope.nowhere", "1.3.0")
                .unwrap_err()
                .to_string();
            assert!(err.contains("no `nope.nowhere` field"), "{format:?}: {err}");
        }
    }

    #[test]
    fn unparseable_input_is_an_error() {
        assert!(read_string("{ unterminated", Format::Json, "a").is_err());
        assert!(read_string("a:\n- b\n c: d\n", Format::Yaml, "a").is_err());
        assert!(read_string("nope = ", Format::Toml, "a").is_err());
    }

    // --- TOML ---------------------------------------------------------------

    const GLEAM: &str = "\
name = \"my_pkg\"
version = \"1.2.0\" # inline comment
gleam = \">= 1.0.0\"

[dependencies]
gleam_stdlib = \">= 0.34.0\"
";

    #[test]
    fn toml_edits_are_surgical() {
        assert_eq!(
            read_string(GLEAM, Format::Toml, "version")
                .unwrap()
                .as_deref(),
            Some("1.2.0")
        );
        assert_eq!(
            read_string(GLEAM, Format::Toml, "dependencies.gleam_stdlib")
                .unwrap()
                .as_deref(),
            Some(">= 0.34.0")
        );
        let out = write_string(GLEAM, Format::Toml, "version", "1.3.0").unwrap();
        assert_eq!(out, GLEAM.replace("1.2.0", "1.3.0"));
        assert!(out.contains("version = \"1.3.0\" # inline comment"));
    }

    #[test]
    fn toml_reports_a_missing_or_mistyped_field() {
        assert_eq!(read_string(GLEAM, Format::Toml, "missing").unwrap(), None);
        let err = read_string(GLEAM, Format::Toml, "dependencies")
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not a string"), "{err}");
    }
}
