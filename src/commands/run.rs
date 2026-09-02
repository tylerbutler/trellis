//! `trellis run <task>` — graph-parallel task fan-out. Built-in tasks map 1:1
//! onto gleam verbs; custom tasks come from `[tools.trellis.tasks]`.

use crate::runner::{self, CommandSpec, Job, RunOptions};
use crate::workspace::{SelectionFilter, Workspace};
use anyhow::{Result, bail};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Target {
    Erlang,
    Javascript,
    All,
}

pub struct TaskOptions {
    pub task: String,
    pub packages: Vec<String>,
    pub since: Option<String>,
    pub with_dependents: bool,
    pub target: Option<Target>,
    pub strict: bool,
    pub check: bool,
    pub serial: bool,
    pub keep_going: bool,
    pub jobs: Option<usize>,
    pub json: bool,
}

impl Target {
    /// The flag value as the user gave it, for the JSON payload.
    fn as_str(self) -> &'static str {
        match self {
            Target::Erlang => "erlang",
            Target::Javascript => "javascript",
            Target::All => "all",
        }
    }
}

pub(crate) const BUILTIN_TASKS: &[&str] =
    &["build", "test", "check", "format", "docs", "deps", "clean"];

pub fn run(workspace: &Workspace, options: &TaskOptions) -> Result<bool> {
    let mut selected = workspace.select(&SelectionFilter {
        names: options.packages.clone(),
        since: options.since.clone(),
        with_dependents: options.with_dependents,
        releasable_only: false,
    })?;
    if let Some(patterns) = workspace.config.exclude.get(&options.task) {
        let excluded = crate::workspace::build_globset(patterns)?;
        selected.retain(|&idx| !excluded.is_match(&workspace.members[idx].rel_path));
    }

    let mut jobs = Vec::new();
    for idx in selected {
        let member = &workspace.members[idx];
        let commands = commands_for(workspace, options, &member.path)?;
        jobs.push(Job {
            member: idx,
            commands,
        });
    }

    let run_options = RunOptions {
        parallelism: RunOptions::parallelism(options.serial, options.jobs),
        keep_going: options.keep_going,
        json: options.json,
    };
    runner::run_and_report(workspace, &jobs, &run_options, |ok, results| {
        serde_json::to_string_pretty(&crate::json::RunDocument {
            schema: crate::json::RunDocument::SCHEMA,
            ok,
            task: &options.task,
            target: options.target.map(Target::as_str),
            results,
        })
    })
}

/// The concrete targets a `--target` flag expands to. `None` in the result
/// means "let gleam use the package's default target".
fn expand_targets(target: Option<Target>) -> Vec<Option<&'static str>> {
    match target {
        None => vec![None],
        Some(Target::Erlang) => vec![Some("erlang")],
        Some(Target::Javascript) => vec![Some("javascript")],
        Some(Target::All) => vec![Some("erlang"), Some("javascript")],
    }
}

fn commands_for(
    workspace: &Workspace,
    options: &TaskOptions,
    package_dir: &Path,
) -> Result<Vec<CommandSpec>> {
    // A [tools.trellis.tasks] entry may shadow a built-in verb to customize it.
    if let Some(custom) = workspace.config.tasks.get(&options.task) {
        let mut commands = Vec::new();
        if custom.needs_deps && !package_dir.join("build").join("packages").is_dir() {
            commands.push(gleam(&["deps", "download"], package_dir));
        }
        commands.push(CommandSpec::shell(
            &custom.command,
            package_dir.to_path_buf(),
        ));
        return Ok(commands);
    }

    let targets = expand_targets(options.target);
    let commands = match options.task.as_str() {
        "build" => targeted(&["build"], &targets, options.strict, package_dir),
        "test" => targeted(&["test"], &targets, false, package_dir),
        "check" => targeted(&["check"], &targets, false, package_dir),
        "docs" => targeted(&["docs", "build"], &targets, false, package_dir),
        "format" => {
            let mut args = vec!["format"];
            if options.check {
                args.push("--check");
            }
            vec![gleam(&args, package_dir)]
        }
        "deps" => vec![gleam(&["deps", "download"], package_dir)],
        "clean" => vec![gleam(&["clean"], package_dir)],
        other => bail!(
            "unknown task `{other}`; built-ins: {}. Custom tasks are declared under [tools.trellis.tasks] in the root gleam.toml{}",
            BUILTIN_TASKS.join(", "),
            if workspace.config.tasks.is_empty() {
                String::new()
            } else {
                format!(
                    " (declared: {})",
                    workspace
                        .config
                        .tasks
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        ),
    };
    Ok(commands)
}

fn targeted(
    base: &[&str],
    targets: &[Option<&str>],
    strict: bool,
    package_dir: &Path,
) -> Vec<CommandSpec> {
    targets
        .iter()
        .map(|target| {
            let mut args: Vec<&str> = base.to_vec();
            if let Some(target) = target {
                args.push("--target");
                args.push(target);
            }
            if strict {
                args.push("--warnings-as-errors");
            }
            gleam(&args, package_dir)
        })
        .collect()
}

fn gleam(args: &[&str], package_dir: &Path) -> CommandSpec {
    CommandSpec {
        program: crate::tools::gleam_bin(),
        args: args.iter().map(|&s| s.to_string()).collect(),
        cwd: package_dir.to_path_buf(),
    }
}
