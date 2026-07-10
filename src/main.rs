mod changie;
mod commands;
mod config;
mod git;
mod gleam;
mod hex;
mod lockfile;
mod rewrite;
mod runner;
mod tools;
mod workspace;

use anyhow::Result;
use clap::{Parser, Subcommand};
use commands::graph::GraphFormat;
use commands::run::Target;
use std::path::PathBuf;
use std::process::ExitCode;
use workspace::Workspace;

/// A workspace CLI for Gleam monorepos: task fan-out, introspection, and
/// release orchestration derived from workspace.toml and member gleam.toml
/// files. Configure nothing that can be derived; verify anything that must
/// be duplicated.
#[derive(Parser)]
#[command(name = "trellis", version, about, max_term_width = 100)]
struct Cli {
    /// Run as if started in this directory.
    #[arg(short = 'C', long = "directory", global = true, value_name = "DIR")]
    directory: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List members in topological order (dependencies first)
    List {
        /// Emit JSON instead of names
        #[arg(long)]
        json: bool,
        /// Only members owning files changed since this git ref
        #[arg(long, value_name = "REF")]
        since: Option<String>,
        /// Add the reverse-dependency closure of the selection
        #[arg(long)]
        with_dependents: bool,
        /// Only members that participate in releases (excludes ignore-release)
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
        package: String,
        #[arg(long)]
        json: bool,
    },
    /// Run a task across members, graph-parallel by default
    Run {
        /// Built-in (build, test, check, format, docs, deps, clean) or a [tasks] entry
        task: String,
        /// Packages to run in; all members when omitted
        packages: Vec<String>,
        /// Only members owning files changed since this git ref
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
    },
    /// Run an arbitrary command in each member directory
    Exec {
        /// Packages to run in; all members when omitted
        packages: Vec<String>,
        /// Only members owning files changed since this git ref
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
        /// The command to run (after `--`)
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Changelog management (wraps changie; see [changelog] in workspace.toml)
    Changelog {
        #[command(subcommand)]
        command: ChangelogCommand,
    },
    /// Plan and apply version bumps from unreleased changelog fragments
    Version {
        #[command(subcommand)]
        command: VersionCommand,
    },
    /// Compare package versions against git tags; create what's missing
    Tag {
        #[command(subcommand)]
        command: TagCommand,
    },
    /// Publish packages to Hex, in dependency order, with path deps rewritten
    Publish {
        /// A single package to publish
        package: Option<String>,
        /// Resolve a pushed tag (e.g. lat_core-v1.2.0) to its package
        #[arg(long, conflicts_with = "package")]
        tag: Option<String>,
        /// Every releasable package whose version isn't on Hex yet
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
        /// Regenerate out-of-date generated files (.changie.yaml projects)
        #[arg(long)]
        fix: bool,
    },
    /// Structured output for CI
    Ci {
        #[command(subcommand)]
        command: CiCommand,
    },
}

#[derive(Subcommand)]
enum ChangelogCommand {
    /// Add a changelog fragment (wraps `changie new`, which prompts)
    New {
        /// Route the fragment to this package
        #[arg(long)]
        package: Option<String>,
    },
    /// Verify changed packages have changelog fragments; non-zero exit if not
    Check {
        /// Base ref of the change range
        #[arg(long)]
        base: String,
        /// Head ref of the change range
        #[arg(long, default_value = "HEAD")]
        head: String,
        #[arg(long)]
        json: bool,
    },
    /// Regenerate the derived projects section of .changie.yaml
    Sync {
        /// Verify instead of write; non-zero exit on drift
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Dry-run: show what `version apply` would bump
    Plan {
        #[arg(long)]
        json: bool,
    },
    /// Batch + merge via changie, then patch manifest.toml locked versions
    Apply {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TagCommand {
    /// List releasable packages whose current version has no tag yet
    Plan {
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
        /// Refresh only this package instead of every member
        #[arg(long)]
        package: Option<String>,
    },
}

#[derive(Subcommand)]
enum CiCommand {
    /// Emit a GitHub Actions strategy matrix: {"include":[{name,path,version},…]}
    Matrix {
        /// Only members affected by changes since this git ref (dependents included)
        #[arg(long, value_name = "REF")]
        since: Option<String>,
        /// Only members that participate in releases
        #[arg(long)]
        releasable: bool,
    },
    /// Emit workspace facts as key=value lines for $GITHUB_OUTPUT
    Outputs,
    /// Resolve a pushed tag (e.g. $GITHUB_REF_NAME) to its package name
    TagPackage {
        tag: String,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
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
    if let Command::Doctor { fix } = cli.command {
        let root = Workspace::find_root(&start)?;
        return commands::doctor::run(&root, fix);
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
            },
        ),
        Command::Exec {
            packages,
            since,
            serial,
            keep_going,
            jobs,
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
            },
        ),
        Command::Changelog { command } => match command {
            ChangelogCommand::New { package } => {
                commands::changelog::new_fragment(&workspace, package.as_deref())?;
                Ok(true)
            }
            ChangelogCommand::Check { base, head, json } => commands::changelog::check(
                &workspace,
                &commands::changelog::CheckOptions { base, head, json },
            ),
            ChangelogCommand::Sync { check } => commands::changelog::sync(&workspace, check),
        },
        Command::Version { command } => match command {
            VersionCommand::Plan { json } => {
                commands::version::plan(&workspace, json)?;
                Ok(true)
            }
            VersionCommand::Apply { json } => commands::version::apply(&workspace, json),
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
        Command::Doctor { .. } => unreachable!("handled above"),
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
