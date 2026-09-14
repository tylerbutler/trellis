//! Process-wide presentation settings, resolved once from the global flags,
//! and the color vocabulary every command paints with.
//!
//! Color and verbosity are decided in `main` and read from anywhere, rather
//! than threaded through every call. The alternative — passing a settings
//! struct down — dies at [`crate::runner::Output`], which is constructed deep
//! inside the scheduler, and at the tool-spawn sites in `git`, `gleam`, and
//! `tools`, which are leaf functions several layers below any command struct.
//!
//! [`init`] is called once, before anything prints. A reader that runs first
//! (a unit test, say) gets the same auto-detection a bare invocation would.
//!
//! # The color system
//!
//! Color is semantic, never decorative, and the words always carry the
//! meaning on their own — a `--color never` transcript says everything the
//! colored one does. Six roles, in the cargo/rustc vernacular:
//!
//! - [`err`]: `error:`, `FAILED:`, `invalid:` — bold red
//! - [`warn`]: `warning:` — bold yellow
//! - [`note`]: `note:` and other advisories — bold cyan
//! - [`ok`]: `ok:` and completed-action verbs (`tagged`, `published`,
//!   `bumped`, …) — bold green
//! - [`package`]: member names, bold in a stable hash-picked color, so one
//!   package keeps one color across `run`, `list`, `graph`, `version`, …
//! - [`dim`]: structure and metadata — tree glyphs, `$ command` echoes,
//!   versions-in-parens, field labels — and entire dry-run `would …` lines,
//!   so a plan never reads as an action
//!
//! Only the 16 named ANSI colors, so output follows the user's terminal
//! theme; bold for meaning, faint for structure, nothing else.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::OnceLock;

/// `--color`. `Auto` defers to the terminal and the environment; the other two
/// are the user overriding that detection in either direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

/// How much to say. `-q` and `-v` are mutually exclusive at the clap layer, so
/// this is one axis rather than two flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    /// `-q`: drop every command's normal-path narration — progress lines,
    /// summaries, confirmations. `--json`/`--format json`/`github` payloads,
    /// generated output (`man`, `completions`, `markdown-help`), fatal
    /// errors, and the exit code are all unaffected.
    Quiet,
    #[default]
    Normal,
    /// `-v`: trace every command trellis shells out to, on stderr.
    Verbose,
}

struct Settings {
    colors: bool,
    verbosity: Verbosity,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Resolve the global flags. Call once, from `main`, before anything prints.
///
/// A second call is ignored rather than fatal: the first writer wins, and the
/// only way to reach one is a caller that printed before initializing, which
/// the auto-detected defaults already handle sensibly.
pub fn init(color: ColorChoice, verbosity: Verbosity) {
    let _ = SETTINGS.set(Settings {
        colors: resolve_colors(color),
        verbosity,
    });
}

/// `--color` wins over the environment, which wins over TTY detection. The
/// environment half is the de-facto convention: `NO_COLOR` at any value, and
/// `CLICOLOR=0`, both mean no color.
fn resolve_colors(choice: ColorChoice) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => auto_colors(std::io::stdout().is_terminal()),
    }
}

fn auto_colors(is_terminal: bool) -> bool {
    is_terminal
        && std::env::var("TERM").as_deref() != Ok("dumb")
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("CLICOLOR").as_deref() != Ok("0")
}

fn settings() -> &'static Settings {
    SETTINGS.get_or_init(|| Settings {
        colors: resolve_colors(ColorChoice::Auto),
        verbosity: Verbosity::Normal,
    })
}

pub fn colors_enabled() -> bool {
    settings().colors
}

pub fn quiet() -> bool {
    settings().verbosity == Verbosity::Quiet
}

pub fn verbose() -> bool {
    settings().verbosity == Verbosity::Verbose
}

/// The hash-picked palette for [`package`]: every named ANSI color except
/// the ones that read as pure state (plain red/green stay, since bold-on-name
/// is visually distinct from the status words), across normal and bright.
const NAME_COLOR_CODES: &[u8] = &[31, 32, 33, 34, 35, 36, 91, 92, 93, 94, 95, 96];

/// Wrap `text` in one SGR sequence, or return it untouched when color is off.
fn paint(text: &str, sgr: &str, enabled: bool) -> String {
    if enabled {
        format!("\x1b[{sgr}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// `error:` / `FAILED:` / `invalid:` — bold red.
pub fn err(text: &str) -> String {
    paint(text, "1;31", colors_enabled())
}

/// `warning:` — bold yellow.
pub fn warn(text: &str) -> String {
    paint(text, "1;33", colors_enabled())
}

/// `note:` and other advisories — bold cyan.
pub fn note(text: &str) -> String {
    paint(text, "1;36", colors_enabled())
}

/// `ok:` and completed-action verbs (`tagged`, `published`, `bumped`) — bold
/// green.
pub fn ok(text: &str) -> String {
    paint(text, "1;32", colors_enabled())
}

/// Structure and metadata: tree glyphs, `$ command` echoes, field labels,
/// and whole dry-run `would …` lines — faint.
pub fn dim(text: &str) -> String {
    paint(text, "2", colors_enabled())
}

/// A member name, bold in its stable hash-picked color.
pub fn package(name: &str) -> String {
    package_padded(name, name)
}

/// A member name padded to column width: the color is picked from `name`,
/// but `display` (name plus trailing spaces) is what gets painted — ANSI
/// codes inside a `{:width$}` would defeat the padding.
pub fn package_padded(name: &str, display: &str) -> String {
    paint(
        display,
        &format!("1;{}", name_color_code(name)),
        colors_enabled(),
    )
}

fn name_color_code(name: &str) -> u8 {
    let index = stable_name_hash(name) as usize % NAME_COLOR_CODES.len();
    NAME_COLOR_CODES[index]
}

/// FNV-1a, so a package keeps its color across runs, machines, and releases.
fn stable_name_hash(name: &str) -> u64 {
    name.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

/// Print a line of human-facing narration, unless `-q` suppressed it.
///
/// Every command's normal-path prose goes through this — progress lines,
/// summaries, confirmations. It never gates a `--json`/`--format json` (or
/// `github`) payload, or `man`/`completions`/`markdown-help`: those are the
/// thing a command exists to produce for a script or a file, not narration
/// about producing it, so `-q` leaves them alone. Fatal errors bypass it too:
/// they print in `main`, outside any command's quiet-gated text.
#[macro_export]
macro_rules! status {
    ($($arg:tt)*) => {
        if !$crate::term::quiet() {
            println!($($arg)*);
        }
    };
}

/// Echo a command trellis is about to run, on stderr, when `-v` is set.
///
/// stderr rather than stdout so the trace never lands in a `--json` payload or
/// a piped package list. The `+ ` prefix follows `set -x`.
pub fn trace_command(program: &str, args: &[impl AsRef<str>], cwd: &Path) {
    if !verbose() {
        return;
    }
    let mut line = shell_quote(program);
    for arg in args {
        line.push(' ');
        line.push_str(&shell_quote(arg.as_ref()));
    }
    eprintln!("{}", dim(&format!("+ {line}  ({})", cwd.display())));
}

/// Echo an HTTP request trellis is about to make, on stderr, when `-v` is
/// set. The API sibling of [`trace_command`]; bodies are elided.
pub fn trace_http(method: &str, url: &str) {
    if !verbose() {
        return;
    }
    eprintln!("{}", dim(&format!("+ {method} {url}")));
}

/// Quote an argument well enough that the trace can be pasted back into a
/// shell. Single quotes, since the arguments that need it are prose — a
/// changelog body passed to `git tag -m`, say.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains(['\'', ' ', '\t', '\n', '"', '$', '`', '\\']) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_color_choice_ignores_the_environment() {
        // The point of `--color always` is that it survives a pipe, and of
        // `--color never` that it survives a TTY.
        assert!(resolve_colors(ColorChoice::Always));
        assert!(!resolve_colors(ColorChoice::Never));
    }

    #[test]
    fn auto_is_off_when_output_is_not_a_terminal() {
        assert!(!auto_colors(false));
    }

    #[test]
    fn painting_wraps_in_one_sgr_sequence() {
        assert_eq!(paint("error:", "1;31", true), "\x1b[1;31merror:\x1b[0m");
        assert_eq!(
            paint("lat_core   ", "1;35", true),
            "\x1b[1;35mlat_core   \x1b[0m"
        );
    }

    #[test]
    fn painting_disabled_is_a_passthrough() {
        assert_eq!(paint("error:", "1;31", false), "error:");
        assert_eq!(paint("lat_core   ", "1;35", false), "lat_core   ");
    }

    #[test]
    fn package_name_colors_are_stable_and_hash_based() {
        assert_eq!(name_color_code("lat_core"), 35);
        assert_eq!(name_color_code("lat_core"), name_color_code("lat_core"));
        assert_ne!(name_color_code("lat_core"), name_color_code("lat_mid"));
    }

    #[test]
    fn quoting_leaves_ordinary_arguments_alone() {
        assert_eq!(shell_quote("--json"), "--json");
        assert_eq!(shell_quote("lat_core-v1.2.0"), "lat_core-v1.2.0");
    }

    #[test]
    fn quoting_wraps_what_a_shell_would_resplit() {
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
