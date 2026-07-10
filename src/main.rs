mod commands;
mod config;
mod git;
mod gleam;
mod runner;
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
    /// Validate workspace invariants; non-zero exit on any error
    Doctor,
    /// Structured output for CI
    Ci {
        #[command(subcommand)]
        command: CiCommand,
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
    if let Command::Doctor = cli.command {
        let root = Workspace::find_root(&start)?;
        return commands::doctor::run(&root);
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
        Command::Doctor => unreachable!("handled above"),
        Command::Ci { command } => {
            match command {
                CiCommand::Matrix { since, releasable } => {
                    commands::ci::matrix(&workspace, since, releasable)?
                }
                CiCommand::Outputs => commands::ci::outputs(&workspace)?,
            }
            Ok(true)
        }
    }
}
