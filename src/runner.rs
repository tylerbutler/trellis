//! Graph-parallel task scheduler: a package's job starts as soon as every
//! selected package it (transitively) depends on has finished, up to
//! `--jobs N` at once. Output is streamed with a `pkg ▏` prefix and a summary
//! table is printed at the end.

use crate::workspace::Workspace;
use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl CommandSpec {
    pub fn shell(command: &str, cwd: PathBuf) -> Self {
        Self {
            program: "sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
            cwd,
        }
    }

    fn display(&self) -> String {
        if self.program == "sh" && self.args.first().map(String::as_str) == Some("-c") {
            return self.args.get(1).cloned().unwrap_or_default();
        }
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// One unit of scheduled work: a member and the commands to run in it,
/// sequentially, stopping at the first failure.
#[derive(Debug)]
pub struct Job {
    pub member: usize,
    pub commands: Vec<CommandSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Success,
    Failed(String),
    /// Not run because scheduling stopped after an earlier failure.
    Skipped,
}

#[derive(Debug)]
pub struct JobResult {
    pub member: usize,
    pub status: JobStatus,
    pub duration: Duration,
}

pub struct RunOptions {
    pub parallelism: usize,
    pub keep_going: bool,
}

/// Run jobs respecting workspace dependency order among the selected members.
/// Ordering constraints follow *transitive* deps, so order is preserved even
/// when intermediate packages aren't part of the selection.
pub fn run_jobs(
    workspace: &Workspace,
    jobs: Vec<Job>,
    options: &RunOptions,
) -> Result<Vec<JobResult>> {
    if jobs.is_empty() {
        println!("no packages selected");
        return Ok(Vec::new());
    }

    let prefix_width = jobs
        .iter()
        .map(|job| workspace.members[job.member].name.len())
        .max()
        .unwrap_or(0);

    let selected: HashMap<usize, usize> = jobs
        .iter()
        .enumerate()
        .map(|(job_idx, job)| (job.member, job_idx))
        .collect();
    let mut remaining = vec![0usize; jobs.len()];
    let mut waiters: Vec<Vec<usize>> = vec![Vec::new(); jobs.len()];
    for (job_idx, job) in jobs.iter().enumerate() {
        let deps: HashSet<usize> = workspace
            .transitive_deps(job.member)
            .into_iter()
            .filter_map(|member| selected.get(&member).copied())
            .collect();
        remaining[job_idx] = deps.len();
        for dep_job in deps {
            waiters[dep_job].push(job_idx);
        }
    }

    // Jobs arrive in topological order, so a FIFO ready queue keeps starts
    // deterministic when parallelism is 1.
    let mut ready: VecDeque<usize> = (0..jobs.len()).filter(|&j| remaining[j] == 0).collect();
    let mut results: Vec<Option<JobResult>> = (0..jobs.len()).map(|_| None).collect();
    let (sender, receiver) = mpsc::channel::<JobResult>();
    let mut running = 0usize;
    let mut halted = false;

    std::thread::scope(|scope| -> Result<()> {
        loop {
            while !halted && running < options.parallelism.max(1) {
                let Some(job_idx) = ready.pop_front() else {
                    break;
                };
                let job = &jobs[job_idx];
                let name = workspace.members[job.member].name.clone();
                let sender = sender.clone();
                running += 1;
                scope.spawn(move || {
                    let status = execute_job(job, &name, prefix_width);
                    let _ = sender.send(JobResult {
                        member: job_idx, // job index in-flight; remapped below
                        status: status.0,
                        duration: status.1,
                    });
                });
            }
            if running == 0 {
                break;
            }
            let done = receiver.recv().expect("worker threads outlive the loop");
            running -= 1;
            let job_idx = done.member;
            let failed = matches!(done.status, JobStatus::Failed(_));
            results[job_idx] = Some(JobResult {
                member: jobs[job_idx].member,
                ..done
            });
            if failed && !options.keep_going {
                halted = true;
            }
            for &waiter in &waiters[job_idx] {
                remaining[waiter] -= 1;
                if remaining[waiter] == 0 {
                    ready.push_back(waiter);
                }
            }
        }
        Ok(())
    })?;

    let results: Vec<JobResult> = jobs
        .iter()
        .enumerate()
        .map(|(job_idx, job)| {
            results[job_idx].take().unwrap_or(JobResult {
                member: job.member,
                status: JobStatus::Skipped,
                duration: Duration::ZERO,
            })
        })
        .collect();

    print_summary(workspace, &results);
    Ok(results)
}

fn execute_job(job: &Job, name: &str, width: usize) -> (JobStatus, Duration) {
    let started = Instant::now();
    for spec in &job.commands {
        emit(name, width, &format!("$ {}", spec.display()));
        match run_streaming(spec, name, width) {
            Ok(true) => {}
            Ok(false) => {
                return (
                    JobStatus::Failed(format!("`{}` failed", spec.display())),
                    started.elapsed(),
                );
            }
            Err(err) => {
                emit(name, width, &format!("error: {err:#}"));
                return (JobStatus::Failed(format!("{err:#}")), started.elapsed());
            }
        }
    }
    (JobStatus::Success, started.elapsed())
}

/// Run one command, streaming stdout and stderr lines with the `pkg ▏` prefix.
fn run_streaming(spec: &CommandSpec, name: &str, width: usize) -> Result<bool> {
    let mut child = Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| anyhow::anyhow!("failed to start `{}`: {err}", spec.display()))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    std::thread::scope(|scope| {
        for pipe in [
            Box::new(stdout) as Box<dyn std::io::Read + Send>,
            Box::new(stderr),
        ] {
            scope.spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    emit(name, width, &line);
                }
            });
        }
    });
    Ok(child.wait()?.success())
}

fn emit(name: &str, width: usize, line: &str) {
    println!("{name:width$} ▏ {line}");
}

fn print_summary(workspace: &Workspace, results: &[JobResult]) {
    let width = results
        .iter()
        .map(|r| workspace.members[r.member].name.len())
        .max()
        .unwrap_or(0)
        .max("package".len());
    println!();
    println!("{:width$}  {:8}  time", "package", "status");
    for result in results {
        let name = &workspace.members[result.member].name;
        let (status, detail) = match &result.status {
            JobStatus::Success => ("ok", String::new()),
            JobStatus::Failed(reason) => ("FAILED", format!("  {reason}")),
            JobStatus::Skipped => ("skipped", String::new()),
        };
        let time = if result.status == JobStatus::Skipped {
            String::new()
        } else {
            format!("{:.1}s", result.duration.as_secs_f64())
        };
        println!("{name:width$}  {status:8}  {time}{detail}");
    }
}

/// True when every job succeeded (skipped counts as failure for exit codes).
pub fn all_succeeded(results: &[JobResult]) -> bool {
    results.iter().all(|r| r.status == JobStatus::Success)
}
