mod changelog;
mod commands;
mod completion;
mod config;
mod git;
mod gleam;
mod hex;
mod json;
mod lockfile;
mod rewrite;
mod runner;
mod term;
mod tools;
mod update_check;
mod workspace;

use anyhow::Result;
use clap::{Parser, Subcommand};
use commands::changelog::CheckFormat;
use commands::doctor::DoctorFormat;
use commands::graph::GraphFormat;
use commands::run::Target;
use config::Strictness;
use std::path::PathBuf;
use std::process::ExitCode;
use term::{ColorChoice, Verbosity};
use workspace::Workspace;

/// Trellis itself could not run: unparseable config, not a git repository, a
/// missing `gleam`/`gh`, Hex unreachable after retries. Distinct from 1, which
/// means the command ran and found problems.
const EXIT_INTERNAL_ERROR: u8 = 3;

/// Crate version, with `git describe` output appended for builds that aren't
/// a clean release tag. "VERGEN_IDEMPOTENT_OUTPUT" is the placeholder build.rs
/// emits when git metadata is unavailable (e.g. a crates.io tarball).
fn version() -> String {
    let base = env!("CARGO_PKG_VERSION");
    match option_env!("VERGEN_GIT_DESCRIBE") {
        Some(describe)
            if describe != "VERGEN_IDEMPOTENT_OUTPUT" && describe != format!("v{base}") =>
        {
            format!("{base} ({describe})")
        }
        _ => base.to_string(),
    }
}

/// A workspace CLI for Gleam monorepos: task fan-out, introspection, and
/// release orchestration derived entirely from gleam.toml files — the
/// workspace root's [tools.trellis] table and each member's manifest.
/// Configure nothing that can be derived; verify anything that must be
/// duplicated.
#[derive(Parser)]
#[command(name = "trellis", version = version(), about, max_term_width = 100)]
struct Cli {
    /// Run as if started in this directory.
    #[arg(
        short = 'C',
        long = "directory",
        global = true,
        value_name = "DIR",
        value_hint = clap::ValueHint::DirPath
    )]
    directory: Option<PathBuf>,

    /// When to color output. `auto` follows the terminal, `NO_COLOR`, and
    /// `CLICOLOR=0`; the other two override that detection.
    #[arg(long, global = true, value_name = "WHEN", default_value = "auto")]
    color: ColorChoice,

    /// Suppress normal-path output; JSON/report payloads and errors still print
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Trace every command trellis shells out to, on stderr
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Don't check whether a newer trellis release is available
    ///
    /// The check is also skipped in CI, when not attached to a terminal, and
    /// when `TRELLIS_NO_UPDATE_CHECK` or `DO_NOT_TRACK` is set.
    #[arg(long, global = true)]
    no_update_check: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List packages in topological order (dependencies first)
    List {
        /// Emit JSON instead of name/lifecycle columns
        #[arg(long)]
        json: bool,
        /// Only packages owning files changed since this git ref
        #[arg(long, value_name = "REF")]
        since: Option<String>,
        /// Add the reverse-dependency closure of the selection
        #[arg(long)]
        with_dependents: bool,
        /// Only packages whose release lifecycle is `git_only` or `hex`
        #[arg(long)]
        releasable: bool,
    },
    /// Render the dependency graph
    Graph {
        #[arg(long, value_enum, default_value = "text")]
        format: GraphFormat,
    },
    /// Show details for one package
    Info {
        #[arg(add = completion::packages())]
        package: String,
        /// Emit JSON instead of the text summary
        #[arg(long)]
        json: bool,
    },
    /// Run a task across packages, graph-parallel by default
    Run {
        /// Built-in (build, test, check, format, docs, deps, clean) or a [tools.trellis.tasks] entry
        #[arg(add = completion::tasks())]
        task: String,
        /// Packages to run in; all workspace packages when omitted
        #[arg(add = completion::packages())]
        packages: Vec<String>,
        /// Only packages owning files changed since this git ref
        #[arg(long, value_name = "REF")]
        since: Option<String>,
        /// Add the reverse-dependency closure of the selection
        #[arg(long)]
        with_dependents: bool,
        /// Gleam compile target; `all` runs the task once per target
        #[arg(long, value_enum)]
        target: Option<Target>,
        /// Treat warnings as errors (build)
        #[arg(long)]
        strict: bool,
        /// Check instead of write (format)
        #[arg(long)]
        check: bool,
        /// Run one package at a time, in dependency order
        #[arg(long)]
        serial: bool,
        /// Keep scheduling packages after a failure
        #[arg(long)]
        keep_going: bool,
        /// Maximum concurrent packages (default: CPU count)
        #[arg(short, long, value_name = "N")]
        jobs: Option<usize>,
        /// Emit the `trellis.run/1` payload instead of the summary table;
        /// package output moves to stderr
        #[arg(long)]
        json: bool,
    },
    /// Run an arbitrary command in each package directory
    Exec {
        /// Packages to run in; all workspace packages when omitted
        #[arg(add = completion::packages())]
        packages: Vec<String>,
        /// Only packages owning files changed since this git ref
        #[arg(long, value_name = "REF")]
        since: Option<String>,
        /// Run one package at a time, in dependency order
        #[arg(long)]
        serial: bool,
        /// Keep scheduling packages after a failure
        #[arg(long)]
        keep_going: bool,
        /// Maximum concurrent packages (default: CPU count)
        #[arg(short, long, value_name = "N")]
        jobs: Option<usize>,
        /// Emit the `trellis.exec/1` payload instead of the summary table;
        /// package output moves to stderr
        #[arg(long)]
        json: bool,
        /// The command to run (after `--`)
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Changelog fragment management (see [tools.trellis.changelog])
    Changelog {
        #[command(subcommand)]
        command: ChangelogCommand,
    },
    /// Plan and apply version bumps from unreleased changelog fragments
    Version {
        #[command(subcommand)]
        command: VersionCommand,
    },
    /// Bootstrap a workspace: write a [tools.trellis] table at the repo root
    ///
    /// Everything trellis can derive it derives, so the table this writes is
    /// nearly empty by design — its presence is what marks the workspace root.
    /// Members stay auto-discovered from git; the comments it leaves point at
    /// what can be configured. Refuses if the repository is already a trellis
    /// workspace, and finishes by running `doctor`.
    Init,
    /// Scaffold a new package in the workspace
    New {
        /// Package name (lowercase letters, digits, and _)
        name: String,
        /// Template to scaffold from
        #[arg(long, default_value = "lib")]
        template: String,
        /// Parent directory relative to the workspace root (derived from
        /// existing members when omitted)
        #[arg(long)]
        path: Option<String>,
    },
    /// Release orchestration
    Release {
        #[command(subcommand)]
        command: ReleaseCommand,
    },
    /// Compare package versions against git tags; create what's missing
    Tag {
        #[command(subcommand)]
        command: TagCommand,
    },
    /// Publish packages to Hex, in dependency order, with path deps rewritten
    Publish {
        /// A single package to publish
        #[arg(add = completion::hex_packages())]
        package: Option<String>,
        /// Resolve a pushed tag (e.g. lat_core-v1.2.0) to its package
        #[arg(long, conflicts_with = "package")]
        tag: Option<String>,
        /// Every `hex`-lifecycle package whose version isn't on Hex yet
        #[arg(long, conflicts_with_all = ["package", "tag"])]
        all_untagged: bool,
        /// Show what would be published (and rewritten) without doing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Lockfile maintenance
    Lockfile {
        #[command(subcommand)]
        command: LockfileCommand,
    },
    /// Validate workspace invariants; non-zero exit on any error
    Doctor {
        /// Apply the mechanically-fixable findings (seed changelog stubs,
        /// patch stale locked versions), then re-report what remains
        #[arg(long)]
        fix: bool,
        /// List the fixes `--fix` would apply without writing anything
        #[arg(long)]
        dry_run: bool,
        /// How to report findings: prose, the `trellis.doctor/1` JSON payload,
        /// or GitHub Actions annotations that land on the file in a PR
        #[arg(long, value_enum, default_value = "text")]
        format: DoctorFormat,
    },
    /// Structured output for CI
    Ci {
        #[command(subcommand)]
        command: CiCommand,
    },
    /// Print the shell snippet that enables tab-completion
    ///
    /// The snippet asks trellis for candidates on each tab-press, so completions
    /// offer real package and task names from the surrounding workspace and can
    /// never drift from the flags you have. Evaluate it on shell startup rather
    /// than saving it to a completions directory — it talks to trellis over an
    /// interface that changes between releases, so an `eval` stays in sync where
    /// a saved copy goes stale. For zsh, in ~/.zshrc after compinit:
    ///
    ///     eval "$(trellis completions zsh)"
    Completions {
        /// Shell to emit a registration snippet for
        #[arg(value_enum)]
        shell: clap_complete::aot::Shell,
    },
    /// Print the full command reference as Markdown (used to regenerate the
    /// website's CLI reference page; hidden from normal help)
    #[command(hide = true)]
    MarkdownHelp,
    /// Write the man page tree (used to regenerate assets/man; hidden from
    /// normal help)
    #[command(hide = true)]
    Man {
        /// Directory to write `trellis.1` and the per-subcommand pages into
        #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum ChangelogCommand {
    /// Add an unreleased changelog fragment
    New {
        /// The package the change belongs to (optional when the workspace
        /// has exactly one releasable package)
        #[arg(long, add = completion::releasable_packages())]
        package: Option<String>,
        /// Change kind (see [tools.trellis.changelog] kinds; defaults include Added,
        /// Fixed, Breaking, …)
        #[arg(long, add = completion::changelog_kinds())]
        kind: String,
        /// Change category, grouping entries above the kind headings (see
        /// [tools.trellis.changelog] categories; none are configured by default)
        #[arg(long, add = completion::changelog_categories())]
        category: Option<String>,
        /// The changelog entry text
        #[arg(long)]
        body: String,
    },
    /// Verify changed packages have changelog fragments; non-zero exit if not
    Check {
        /// Base ref of the change range
        #[arg(long)]
        base: String,
        /// Head ref of the change range
        #[arg(long, default_value = "HEAD")]
        head: String,
        /// How to report: prose, the `trellis.changelog_check/1` JSON payload
        /// (including a Markdown `preview` for a PR comment), or `key=value`
        /// lines for $GITHUB_OUTPUT
        #[arg(long, value_enum, default_value = "text")]
        format: CheckFormat,
        /// Override the workspace's changelog.strictness for this run: fail on
        /// a missing entry, report it advisorily, or don't check
        #[arg(long, value_enum)]
        strictness: Option<Strictness>,
        /// Deprecated alias for `--format json`
        #[arg(long, conflicts_with = "format")]
        json: bool,
    },
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Dry-run: show what `version apply` would bump
    Plan {
        #[command(flatten)]
        overrides: VersionOverrideArgs,
        /// Emit JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Bump versions, render changelogs, patch manifest.toml locked versions
    Apply {
        #[command(flatten)]
        overrides: VersionOverrideArgs,
        /// Emit JSON listing every bump and patched lockfile
        #[arg(long)]
        json: bool,
    },
}

/// Overrides for the version a plan would otherwise derive from fragment kinds.
///
/// Flattened into both `plan` and `apply` so an override is visible in the
/// dry-run before it is applied — the two must accept exactly the same flags or
/// `plan` stops being a preview of `apply`.
#[derive(clap::Args)]
struct VersionOverrideArgs {
    /// Override the derived bump level, workspace-wide (`--bump major`) or for
    /// one package (`--bump lat_core=major`). Repeatable.
    #[arg(long, value_name = "LEVEL|PKG=LEVEL", add = completion::bump_levels())]
    bump: Vec<String>,
    /// Pin a package's next version exactly (`--set lat_core=1.0.0`).
    /// Repeatable.
    #[arg(long, value_name = "PKG=VERSION")]
    set: Vec<String>,
    /// Cut a prerelease: `--pre rc` gives 1.0.0-rc.1, and again 1.0.0-rc.2.
    /// Fragments stay unreleased until the final version. `--pre none` promotes
    /// the current prerelease to its final version and consumes them.
    #[arg(long, value_name = "LABEL")]
    pre: Option<String>,
}

impl VersionOverrideArgs {
    fn parse(&self) -> Result<commands::version_override::Overrides> {
        commands::version_override::Overrides::parse(&self.bump, &self.set, self.pre.as_deref())
    }
}

#[derive(Subcommand)]
enum ReleaseCommand {
    /// Create or update the release PR: version apply on a branch, push, gh pr
    Pr {
        /// Base branch the PR targets
        #[arg(long, default_value = "main")]
        base: String,
        /// Branch the release commit is force-pushed to
        #[arg(long, default_value = "release/pending")]
        branch: String,
    },
}

#[derive(Subcommand)]
enum TagCommand {
    /// List releasable packages whose current version has no tag yet
    Plan {
        /// Emit JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Create missing tags in topological order
    Create {
        /// Push each created tag to origin
        #[arg(long)]
        push: bool,
        /// Also create a GitHub Release per tag, with the matching CHANGELOG
        /// section as the body (implies --push; requires the gh CLI)
        #[arg(long)]
        github_release: bool,
    },
}

#[derive(Subcommand)]
enum LockfileCommand {
    /// Run `gleam deps download`, scoped to one package (with retry/backoff)
    Refresh {
        /// Refresh only this package instead of the whole workspace
        #[arg(long, add = completion::packages())]
        package: Option<String>,
    },
}

#[derive(Subcommand)]
enum CiCommand {
    /// Emit a GitHub Actions strategy matrix: {"include":[{name,path,version},…]}
    Matrix {
        /// Only packages affected by changes since this git ref (dependents included)
        #[arg(long, value_name = "REF")]
        since: Option<String>,
        /// Only packages that participate in releases
        #[arg(long)]
        releasable: bool,
    },
    /// Emit workspace facts as key=value lines for $GITHUB_OUTPUT
    Outputs,
    /// Resolve a pushed tag (e.g. $GITHUB_REF_NAME) to its package name
    TagPackage {
        tag: String,
        /// Emit JSON with the resolved package, version, and tag kind
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    // Answers the shell when $COMPLETE is set and exits; a normal run falls
    // through untouched. Must come before anything writes to stdout, and before
    // `Cli::parse`, since a completion request is not a valid command line.
    clap_complete::env::CompleteEnv::with_factory(<Cli as clap::CommandFactory>::command)
        .var(commands::generate::COMPLETE_VAR)
        .complete();

    let cli = Cli::parse();
    // Before anything prints, so every writer sees the same resolved settings.
    term::init(
        cli.color,
        match (cli.quiet, cli.verbose) {
            (true, _) => Verbosity::Quiet,
            (_, true) => Verbosity::Verbose,
            _ => Verbosity::Normal,
        },
    );
    // Machine-consumed commands never get the interactive update notice, and it
    // only prints when the command itself succeeded, so it never clutters error
    // output or corrupts structured stdout.
    let notify_update = !cli.no_update_check
        && !matches!(
            cli.command,
            Command::MarkdownHelp
                | Command::Man { .. }
                | Command::Completions { .. }
                | Command::Ci { .. }
                | Command::Doctor {
                    format: DoctorFormat::Json | DoctorFormat::Github,
                    ..
                }
                | Command::Run { json: true, .. }
                | Command::Exec { json: true, .. }
                | Command::Changelog {
                    command: ChangelogCommand::Check {
                        format: CheckFormat::Json | CheckFormat::Github,
                        ..
                    } | ChangelogCommand::Check { json: true, .. },
                }
        );
    let result = dispatch(cli);
    if notify_update && result.is_ok() {
        update_check::notify();
    }
    // The exit-code contract: 0 success, 1 the command ran and found problems,
    // 2 usage (clap's own), 3 trellis could not run. See the Compatibility page
    // in website/src/content/docs/docs/.
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(EXIT_INTERNAL_ERROR)
        }
    }
}

fn dispatch(cli: Cli) -> Result<bool> {
    let start = match &cli.directory {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };

    // Doctor loads leniently so it can report every problem instead of
    // failing on the first one.
    // Reference generation needs no workspace — it reflects on the CLI itself.
    match &cli.command {
        Command::MarkdownHelp => {
            print!("{}", commands::markdown_help());
            return Ok(true);
        }
        Command::Completions { shell } => {
            commands::generate::completions(*shell)?;
            return Ok(true);
        }
        Command::Man { out } => {
            commands::generate::man_pages(out)?;
            return Ok(true);
        }
        // `init` creates the workspace root, so it cannot be found by loading
        // one — it finds the repository root itself.
        Command::Init => return commands::init::run(&start),
        _ => {}
    }

    if let Command::Doctor {
        fix,
        dry_run,
        format,
    } = cli.command
    {
        let root = Workspace::find_root(&start)?;
        return commands::doctor::run(
            &root,
            &commands::doctor::DoctorOptions {
                fix,
                dry_run,
                format,
            },
        );
    }

    let workspace = Workspace::load(&start)?;
    match cli.command {
        Command::List {
            json,
            since,
            with_dependents,
            releasable,
        } => {
            commands::list::run(
                &workspace,
                &commands::list::ListOptions {
                    json,
                    since,
                    with_dependents,
                    releasable,
                },
            )?;
            Ok(true)
        }
        Command::Graph { format } => {
            commands::graph::run(&workspace, format)?;
            Ok(true)
        }
        Command::Info { package, json } => {
            commands::info::run(&workspace, &package, json)?;
            Ok(true)
        }
        Command::Run {
            task,
            packages,
            since,
            with_dependents,
            target,
            strict,
            check,
            serial,
            keep_going,
            jobs,
            json,
        } => commands::run::run(
            &workspace,
            &commands::run::TaskOptions {
                task,
                packages,
                since,
                with_dependents,
                target,
                strict,
                check,
                serial,
                keep_going,
                jobs,
                json,
            },
        ),
        Command::Exec {
            packages,
            since,
            serial,
            keep_going,
            jobs,
            json,
            command,
        } => commands::exec::run(
            &workspace,
            &commands::exec::ExecOptions {
                packages,
                command,
                since,
                serial,
                keep_going,
                jobs,
                json,
            },
        ),
        Command::Changelog { command } => match command {
            ChangelogCommand::New {
                package,
                kind,
                category,
                body,
            } => {
                commands::changelog::new_fragment(
                    &workspace,
                    package.as_deref(),
                    &kind,
                    category.as_deref(),
                    &body,
                )?;
                Ok(true)
            }
            ChangelogCommand::Check {
                base,
                head,
                format,
                strictness,
                json,
            } => {
                // `--json` predates `--format` and clap rejects the two
                // together, so this only ever upgrades the default.
                let format = if json { CheckFormat::Json } else { format };
                commands::changelog::check(
                    &workspace,
                    &commands::changelog::CheckOptions {
                        base,
                        head,
                        format,
                        strictness,
                    },
                )
            }
        },
        Command::Version { command } => match command {
            VersionCommand::Plan { overrides, json } => {
                commands::version::plan(&workspace, &overrides.parse()?, json)?;
                Ok(true)
            }
            VersionCommand::Apply { overrides, json } => {
                commands::version::apply(&workspace, &overrides.parse()?, json)
            }
        },
        Command::New {
            name,
            template,
            path,
        } => {
            commands::new::run(
                &workspace,
                &commands::new::NewOptions {
                    name,
                    template,
                    path,
                },
            )?;
            Ok(true)
        }
        Command::Release { command } => match command {
            ReleaseCommand::Pr { base, branch } => {
                commands::release::pr(&workspace, &commands::release::PrOptions { base, branch })
            }
        },
        Command::Tag { command } => match command {
            TagCommand::Plan { json } => {
                commands::tag::plan(&workspace, json)?;
                Ok(true)
            }
            TagCommand::Create {
                push,
                github_release,
            } => {
                commands::tag::create(
                    &workspace,
                    &commands::tag::CreateOptions {
                        push,
                        github_release,
                    },
                )?;
                Ok(true)
            }
        },
        Command::Publish {
            package,
            tag,
            all_untagged,
            dry_run,
        } => {
            let selector = match (package, tag, all_untagged) {
                (Some(name), None, false) => commands::publish::Selector::Package(name),
                (None, Some(tag), false) => commands::publish::Selector::Tag(tag),
                (None, None, true) => commands::publish::Selector::AllUntagged,
                _ => anyhow::bail!(
                    "specify what to publish: a package name, --tag <tag>, or --all-untagged"
                ),
            };
            commands::publish::run(
                &workspace,
                &commands::publish::PublishOptions { selector, dry_run },
            )
        }
        Command::Lockfile { command } => match command {
            LockfileCommand::Refresh { package } => {
                commands::lockfile::refresh(&workspace, package.as_deref())
            }
        },
        Command::Doctor { .. }
        | Command::Init
        | Command::MarkdownHelp
        | Command::Completions { .. }
        | Command::Man { .. } => unreachable!("handled above"),
        Command::Ci { command } => {
            match command {
                CiCommand::Matrix { since, releasable } => {
                    commands::ci::matrix(&workspace, since, releasable)?
                }
                CiCommand::Outputs => commands::ci::outputs(&workspace)?,
                CiCommand::TagPackage { tag, json } => {
                    commands::ci::tag_package(&workspace, &tag, json)?
                }
            }
            Ok(true)
        }
    }
}
