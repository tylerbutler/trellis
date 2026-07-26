//! Process-wide presentation settings, resolved once from the global flags.
//!
//! Color and verbosity are decided in `main` and read from anywhere, rather
//! than threaded through every call. The alternative — passing a settings
//! struct down — dies at [`crate::runner::Output`], which is constructed deep
//! inside the scheduler, and at the tool-spawn sites in `git`, `gleam`, and
//! `tools`, which are leaf functions several layers below any command struct.
//!
//! [`init`] is called once, before anything prints. A reader that runs first
//! (a unit test, say) gets the same auto-detection a bare invocation would.

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
        ColorChoice::Auto => {
            std::io::stdout().is_terminal()
                && std::env::var("TERM").as_deref() != Ok("dumb")
                && std::env::var_os("NO_COLOR").is_none()
                && std::env::var("CLICOLOR").as_deref() != Ok("0")
        }
    }
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
    eprintln!("+ {line}  ({})", cwd.display());
}

/// Quote an argument well enough that the trace can be pasted back into a
/// shell. Single quotes, since the arguments that need it are prose — a
/// changelog body in `gh release create --notes`, say.
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
    fn auto_is_off_when_not_a_terminal() {
        // The test harness pipes stdout, so auto-detection must say no.
        assert!(!resolve_colors(ColorChoice::Auto));
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
