//! Generators for the man page tree and the shell completion registration
//! snippets. Both reflect on the clap definition, so — like the CLI reference
//! page that [`super::markdown_help`] produces — they can never drift from the
//! actual flags.
//!
//! The man pages are committed under `assets/man` and shipped in the release
//! archives; `just docs` regenerates them and a test in `tests/cli.rs` fails if
//! they're stale. Completion snippets are *not* committed — they're produced on
//! demand by `trellis completions <shell>`, since the shim talks to the binary
//! over an interface that changes between releases and a saved copy would go
//! stale after an upgrade.

use anyhow::{Context, Result};
use clap::CommandFactory;
use clap_complete::aot::Shell;
use std::fs;
use std::path::Path;

/// The environment variable the registration scripts set to ask `trellis` for
/// candidates. `clap_complete`'s default; named here because both the shim
/// generator and the `CompleteEnv` call in `main` must agree on it.
pub const COMPLETE_VAR: &str = "COMPLETE";

/// The `clap::Command` every generated artifact reflects on.
///
/// [`crate::version`] appends `git describe` output for builds that aren't a
/// clean release tag, which would make the committed artifacts differ from one
/// checkout to the next. Pin to the crate version so generation is
/// reproducible. Note this only affects the *generated* docs — the real
/// `--version` output still carries the git metadata.
///
/// Don't be tempted to clear the version instead: clap treats a `None` version
/// as "disable the version flag", so the generated docs would stop mentioning
/// `-V/--version` even though the binary still accepts it.
fn docs_command() -> clap::Command {
    crate::Cli::command().version(env!("CARGO_PKG_VERSION"))
}

/// Write the shell registration script for `shell` to stdout.
///
/// These are the thin shims for `clap_complete`'s environment-activated
/// completion: they don't enumerate anything themselves, they just tell the
/// shell to ask `trellis` for candidates on each tab-press (see the
/// `CompleteEnv` call in `main`). That indirection is what lets completions
/// offer real package and task names from the surrounding workspace.
///
/// `bin` and `completer` are pinned to the bare name so the script resolves
/// `trellis` from `$PATH`. Left to default they'd come from `argv[0]`, baking
/// whatever path invoked the generator — `./target/debug/trellis` — into the
/// script.
///
/// `aot::Shell` is used purely as the CLI value enum: its value names are
/// exactly the [`EnvCompleter::name`]s of the five built-in shells.
pub fn completions(shell: Shell) -> Result<()> {
    let name = shell.to_string();
    let shells = clap_complete::env::Shells::builtins();
    let completer = shells
        .completer(&name)
        .with_context(|| format!("no completion support for shell `{name}`"))?;
    completer
        .write_registration(
            COMPLETE_VAR,
            "trellis",
            "trellis",
            "trellis",
            &mut std::io::stdout().lock(),
        )
        .with_context(|| format!("generating the {name} completion script"))
}

/// The preamble `roff::Roff::to_writer` emits on every call, defining the `\*(Aq`
/// apostrophe string. We assemble pages from individual sections, so without
/// stripping it we'd repeat it once per section.
const ROFF_PREAMBLE: &[u8] = b".ie \\n(.g .ds Aq \\(aq\n.el .ds Aq '\n";

/// Write `trellis.1` plus a page per visible subcommand into `out_dir`.
pub fn man_pages(out_dir: &Path) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    // `disable_help_subcommand` is a global setting propagated during `build`,
    // so it has to be applied first. Man pages for `trellis help ...` would be
    // noise; completions still offer `help`, which is correct there.
    let mut root = docs_command().disable_help_subcommand(true);
    root.build();
    write_tree(&root, out_dir)
}

fn write_tree(cmd: &clap::Command, out_dir: &Path) -> Result<()> {
    write_page(cmd, out_dir)?;
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
        write_tree(sub, out_dir)?;
    }
    Ok(())
}

fn write_page(cmd: &clap::Command, out_dir: &Path) -> Result<()> {
    let man = clap_mangen::Man::new(cmd.clone())
        .section("1")
        // Defaults to "<name> <version>". A version here would make every
        // committed page churn on each release — and since CI runs the drift
        // test on the release PR that bumps Cargo.toml, that PR would go red
        // every time. The project name is the conventional value for .TH's
        // source field anyway, and `trellis --version` is the real answer.
        .source("trellis")
        // roff omits empty control arguments entirely, so the default empty
        // date collapses and shifts source and manual one field left (visible
        // in clap_mangen's own snapshots). A literal `""` survives escaping —
        // `escape_spaces` only quotes strings containing spaces — and reaches
        // groff as a genuine empty field.
        .date("\"\"")
        .manual("Trellis Manual");

    let mut page = Vec::new();
    page.extend_from_slice(ROFF_PREAMBLE);
    section(&mut page, |w| man.render_title(w))?;
    section(&mut page, |w| man.render_name_section(w))?;
    section(&mut page, |w| man.render_synopsis_section(w))?;
    section(&mut page, |w| man.render_description_section(w))?;
    // Mirrors the guards in `Man::render`, whose helpers are private.
    if cmd.get_arguments().any(|a| !a.is_hide_set()) {
        section(&mut page, |w| man.render_options_section(w))?;
    }
    if cmd.get_subcommands().any(|s| !s.is_hide_set()) {
        section(&mut page, |w| man.render_subcommands_section(w))?;
    }
    if cmd.get_after_long_help().is_some() || cmd.get_after_help().is_some() {
        section(&mut page, |w| man.render_extra_section(w))?;
    }
    // No version section — see the `.source` comment above.

    let path = out_dir.join(man.get_filename());
    fs::write(&path, page).with_context(|| format!("writing {}", path.display()))
}

/// Render one section and append it to `page` without its preamble.
fn section(
    page: &mut Vec<u8>,
    render: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>,
) -> Result<()> {
    let mut buf = Vec::new();
    render(&mut buf)?;
    page.extend_from_slice(buf.strip_prefix(ROFF_PREAMBLE).unwrap_or(&buf));
    Ok(())
}
