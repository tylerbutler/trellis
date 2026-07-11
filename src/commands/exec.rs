//! `trellis exec [pkgs...] -- <command...>` — run an arbitrary command in each
//! selected member directory, in dependency order.

use crate::runner::{self, CommandSpec, Job, RunOptions};
use crate::workspace::{SelectionFilter, Workspace};
use anyhow::{Result, bail};

pub struct ExecOptions {
    pub packages: Vec<String>,
    pub command: Vec<String>,
    pub since: Option<String>,
    pub serial: bool,
    pub keep_going: bool,
    pub jobs: Option<usize>,
}

pub fn run(workspace: &Workspace, options: &ExecOptions) -> Result<bool> {
    let Some((program, args)) = options.command.split_first() else {
        bail!("no command given; usage: trellis exec [pkgs...] -- <command...>");
    };

    let selected = workspace.select(&SelectionFilter {
        names: options.packages.clone(),
        since: options.since.clone(),
        with_dependents: false,
        releasable_only: false,
    })?;

    let jobs = selected
        .into_iter()
        .map(|idx| Job {
            member: idx,
            commands: vec![CommandSpec {
                program: program.clone(),
                args: args.to_vec(),
                cwd: workspace.members[idx].path.clone(),
            }],
        })
        .collect();

    let parallelism = if options.serial {
        1
    } else {
        options.jobs.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        })
    };
    let results = runner::run_jobs(
        workspace,
        jobs,
        &RunOptions {
            parallelism,
            keep_going: options.keep_going,
        },
    )?;
    Ok(runner::all_succeeded(&results))
}
