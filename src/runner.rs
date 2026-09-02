//! Graph-parallel task scheduler: a package's job starts as soon as every
//! selected package it (transitively) depends on has finished, up to
//! `--jobs N` at once. Interactive output keeps active jobs in live progress
//! rows while logs scroll above; non-interactive output stays as a plain
//! `pkg ▏`-prefixed stream. A summary table is printed at the end.

use crate::workspace::Workspace;
use anyhow::Result;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, IsTerminal};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const SPINNER_TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
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
    /// Exit code of the command that failed. `None` when the job succeeded or
    /// was skipped, and also when the process left no code of its own — killed
    /// by a signal, or never started because the program was not found.
    pub exit_code: Option<i32>,
    /// The command that failed, as it was run. `None` unless the job failed.
    pub failed_command: Option<String>,
}

pub struct RunOptions {
    pub parallelism: usize,
    pub keep_going: bool,
    /// Keep stdout clear for the caller's JSON document: no progress rows, no
    /// summary table, and the package stream on stderr.
    pub json: bool,
}

impl RunOptions {
    /// `--serial` wins; then `--jobs N`; then one job per available core.
    pub fn parallelism(serial: bool, jobs: Option<usize>) -> usize {
        if serial {
            return 1;
        }
        jobs.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        })
    }
}

#[derive(Clone)]
struct Output {
    progress: Option<Arc<MultiProgress>>,
    /// `-q`: drop the package stream and the summary table entirely.
    quiet: bool,
    /// `--json`: keep the package stream, but off stdout.
    to_stderr: bool,
}

impl Output {
    fn new(json: bool) -> Self {
        let quiet = crate::term::quiet();
        // Progress rows are a terminal affordance, so they stay tied to TTY
        // detection rather than to `--color` — forcing color into a pipe should
        // not start drawing spinners into it.
        let live = !quiet
            && !json
            && std::io::stdout().is_terminal()
            && std::env::var("TERM").as_deref() != Ok("dumb");
        Self {
            progress: live
                .then(|| Arc::new(MultiProgress::with_draw_target(ProgressDrawTarget::stdout()))),
            quiet,
            to_stderr: json,
        }
    }

    fn start_job(&self, name: &str) -> JobDisplay {
        let progress = self.progress.as_ref().map(|multi| {
            let progress = multi.add(ProgressBar::new_spinner());
            progress.set_style(
                ProgressStyle::with_template(
                    "{spinner:.cyan} {prefix}  {msg}  [{elapsed_precise}]",
                )
                .expect("progress template is valid")
                .tick_strings(SPINNER_TICKS),
            );
            progress.set_prefix(crate::term::package(name));
            progress.set_message("starting");
            progress.enable_steady_tick(Duration::from_millis(80));
            progress
        });
        JobDisplay { progress }
    }

    fn emit(&self, name: &str, width: usize, line: &str) {
        if self.quiet {
            return;
        }
        let padded = format!("{name:width$}");
        let name = crate::term::package_padded(name, &padded);
        self.println(format!("{name} {} {line}", crate::term::dim("▏")));
    }

    fn println(&self, line: String) {
        if let Some(progress) = &self.progress {
            progress
                .println(line)
                .expect("failed to write progress output");
        } else if self.to_stderr {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    fn clear_live(&self) {
        if let Some(progress) = &self.progress {
            progress.clear().expect("failed to clear progress output");
        }
    }
}

#[derive(Clone)]
struct JobDisplay {
    progress: Option<ProgressBar>,
}

impl JobDisplay {
    fn set_command(&self, command: &str) {
        if let Some(progress) = &self.progress {
            progress.set_message(format!("$ {command}"));
        }
    }

    fn finish(&self, status: &JobStatus, duration: Duration) {
        let Some(progress) = &self.progress else {
            return;
        };
        progress.set_style(
            ProgressStyle::with_template("{prefix}  {msg}").expect("progress template is valid"),
        );
        let text = match status {
            JobStatus::Success => "✓ ok",
            JobStatus::Failed(_) => "✗ FAILED",
            JobStatus::Skipped => "- skipped",
        };
        progress.finish_with_message(format!(
            "{}  {:.1}s",
            paint_status(status, text),
            duration.as_secs_f64()
        ));
    }
}

/// Run jobs respecting workspace dependency order among the selected members.
/// Ordering constraints follow *transitive* deps, so order is preserved even
/// when intermediate packages aren't part of the selection.
///
/// This is Kahn's algorithm driving a bounded worker pool. Each job carries a
/// count of how many *selected* dependencies it is still waiting on; a job
/// whose count is zero is runnable. Finished jobs decrement their dependents'
/// counts, releasing new work. Up to `parallelism` jobs run at once, so the
/// schedule is a topological order widened into levels rather than a strict
/// sequence.
pub fn run_jobs(
    workspace: &Workspace,
    jobs: &[Job],
    options: &RunOptions,
) -> Result<Vec<JobResult>> {
    if jobs.is_empty() {
        // Under `--json` the caller still emits a document with no results, so
        // this notice would be the one thing corrupting it.
        if !options.json && !crate::term::quiet() {
            println!("no packages selected");
        }
        return Ok(Vec::new());
    }

    let prefix_width = jobs
        .iter()
        .map(|job| workspace.members[job.member].name.len())
        .max()
        .unwrap_or(0);
    let output = Output::new(options.json);

    // The schedule is a Kahn-style ready queue over the selection, and every
    // index below is a *job* index (position in `jobs`), not a member index.
    // `selected` is the member → job reverse map used to translate workspace
    // dependency edges into that space; members outside the selection have no
    // entry and so contribute no edge.
    let selected: HashMap<usize, usize> = jobs
        .iter()
        .enumerate()
        .map(|(job_idx, job)| (job.member, job_idx))
        .collect();
    // `remaining[j]`: how many of j's dependencies have yet to finish — j is
    // ready to start at zero. `waiters[d]`: the jobs to decrement once d
    // finishes, i.e. the dependency edges pointing away from d.
    let mut remaining = vec![0usize; jobs.len()];
    let mut waiters: Vec<Vec<usize>> = vec![Vec::new(); jobs.len()];
    for (job_idx, job) in jobs.iter().enumerate() {
        // Transitive rather than direct deps, then narrowed to the selection.
        // An unselected package imposes no ordering of its own, but a selected
        // package *behind* one still has to wait, and only the transitive walk
        // sees through the gap. `transitive_deps` already collapses the many
        // paths that can reach one dep into a single set entry, so each dep is
        // counted once here no matter how it was reached.
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
    // Sparse until the run ends: a slot stays `None` if that job never started,
    // which is what distinguishes "skipped" from "ran" in the final pass.
    let mut results: Vec<Option<JobResult>> = (0..jobs.len()).map(|_| None).collect();
    // Results arrive in completion order, so each carries its job index.
    let (sender, receiver) = mpsc::channel::<(usize, JobOutcome)>();
    let mut running = 0usize;
    // Set once a job fails without `--keep-going`. It stops *new* starts only;
    // jobs already in flight are still awaited, never killed.
    let mut halted = false;
    // Keep finished bars alive so later log lines do not erase their rows.
    let mut live_displays = Vec::new();

    std::thread::scope(|scope| -> Result<()> {
        // Each turn of the loop does two things: fill the pool from the ready
        // queue, then block until exactly one job reports back. Because a
        // completion is the only event that can make another job ready, there is
        // nothing to do between the two — no polling, no sleeping.
        loop {
            // Fill phase: start jobs until the pool is full or nothing is ready.
            // A non-empty pool with an empty ready queue is the normal state, not
            // a stall; the jobs still running are what will unblock the rest.
            while !halted && running < options.parallelism.max(1) {
                let Some(job_idx) = ready.pop_front() else {
                    break;
                };
                let job = &jobs[job_idx];
                let name = workspace.members[job.member].name.clone();
                let sender = sender.clone();
                let output = output.clone();
                let display = output.start_job(&name);
                live_displays.push(display.clone());
                running += 1;
                scope.spawn(move || {
                    let outcome = execute_job(job, &name, prefix_width, &output, &display);
                    display.finish(&outcome.status, outcome.duration);
                    let _ = sender.send((job_idx, outcome));
                });
            }
            // Nothing running and nothing startable: either every job finished, or
            // `halted` cut the run short and the stragglers have now drained.
            // Checked after the fill phase and before the blocking receive, this
            // is the loop's only exit — so it cannot leave a sent result unread.
            if running == 0 {
                break;
            }
            // Reap phase. Blocking on one completion is what bounds the pool —
            // control returns to the fill phase with exactly one free slot.
            let (job_idx, outcome) = receiver.recv().expect("worker threads outlive the loop");
            running -= 1;
            let failed = matches!(outcome.status, JobStatus::Failed(_));
            results[job_idx] = Some(JobResult {
                member: jobs[job_idx].member,
                status: outcome.status,
                duration: outcome.duration,
                exit_code: outcome.exit_code,
                failed_command: outcome.failed_command,
            });
            if failed && !options.keep_going {
                halted = true;
            }
            // Release the dependents. Even when halted, this keeps `remaining`
            // truthful; `halted` gates the starts, so a newly ready job simply
            // waits in the queue and is reported as skipped at the end.
            for &waiter in &waiters[job_idx] {
                remaining[waiter] -= 1;
                if remaining[waiter] == 0 {
                    ready.push_back(waiter);
                }
            }
        }
        Ok(())
    })?;

    // Walking `jobs` rather than `results` restores the caller's input order,
    // discarding the completion order the channel imposed. Any slot still `None`
    // is a job that never started — blocked behind a failure, or still queued
    // when the run halted — and is reported as skipped with a zero duration.
    let results: Vec<JobResult> = jobs
        .iter()
        .enumerate()
        .map(|(job_idx, job)| {
            results[job_idx].take().unwrap_or(JobResult {
                member: job.member,
                status: JobStatus::Skipped,
                duration: Duration::ZERO,
                exit_code: None,
                failed_command: None,
            })
        })
        .collect();

    output.clear_live();
    print_summary(workspace, &results, &output, prefix_width);
    Ok(results)
}

/// [`run_jobs`], then under `--json` print the document `render` builds from
/// the outcome. Returns whether every job succeeded.
pub fn run_and_report(
    workspace: &Workspace,
    jobs: &[Job],
    options: &RunOptions,
    render: impl FnOnce(bool, Vec<crate::json::TaskResult<'_>>) -> serde_json::Result<String>,
) -> Result<bool> {
    let results = run_jobs(workspace, jobs, options)?;
    let ok = all_succeeded(&results);
    if options.json {
        let results = results
            .iter()
            .map(|result| crate::json::TaskResult::new(workspace, result))
            .collect();
        println!("{}", render(ok, results)?);
    }
    Ok(ok)
}

fn paint_status(status: &JobStatus, text: &str) -> String {
    match status {
        JobStatus::Success => crate::term::ok(text),
        JobStatus::Failed(_) => crate::term::err(text),
        JobStatus::Skipped => crate::term::warn(text),
    }
}

/// What one job produced, before it is paired back up with its member index.
struct JobOutcome {
    status: JobStatus,
    duration: Duration,
    exit_code: Option<i32>,
    failed_command: Option<String>,
}

impl JobOutcome {
    fn succeeded(duration: Duration) -> Self {
        Self {
            status: JobStatus::Success,
            duration,
            exit_code: None,
            failed_command: None,
        }
    }

    fn failed(reason: String, command: String, exit_code: Option<i32>, duration: Duration) -> Self {
        Self {
            status: JobStatus::Failed(reason),
            duration,
            exit_code,
            failed_command: Some(command),
        }
    }
}

fn execute_job(
    job: &Job,
    name: &str,
    width: usize,
    output: &Output,
    display: &JobDisplay,
) -> JobOutcome {
    let started = Instant::now();
    for spec in &job.commands {
        let command = spec.display();
        display.set_command(&command);
        output.emit(name, width, &format!("$ {command}"));
        match run_streaming(spec, name, width, output) {
            Ok(status) if status.success() => {}
            Ok(status) => {
                return JobOutcome::failed(
                    format!("`{command}` failed"),
                    command,
                    status.code(),
                    started.elapsed(),
                );
            }
            Err(err) => {
                output.emit(name, width, &format!("error: {err:#}"));
                // No exit code: the program never ran, so there is nothing to
                // report but the spawn failure itself.
                return JobOutcome::failed(format!("{err:#}"), command, None, started.elapsed());
            }
        }
    }
    JobOutcome::succeeded(started.elapsed())
}

/// Run one command, streaming stdout and stderr lines with the `pkg ▏` prefix.
fn run_streaming(
    spec: &CommandSpec,
    name: &str,
    width: usize,
    output: &Output,
) -> Result<std::process::ExitStatus> {
    crate::term::trace_command(&spec.program, &spec.args, &spec.cwd);
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
            let output = output.clone();
            scope.spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    output.emit(name, width, &line);
                }
            });
        }
    });
    Ok(child.wait()?)
}

fn print_summary(workspace: &Workspace, results: &[JobResult], output: &Output, name_width: usize) {
    // `-q` drops it; `--json` replaces it with the payload the caller prints.
    if output.quiet || output.to_stderr {
        return;
    }
    let width = name_width.max("package".len());
    println!();
    println!(
        "{}",
        crate::term::dim(&format!("{:width$}  {:8}  time", "package", "status"))
    );
    for result in results {
        let name = &workspace.members[result.member].name;
        let padded_name = format!("{name:width$}");
        let display_name = crate::term::package_padded(name, &padded_name);
        let (status, detail) = match &result.status {
            JobStatus::Success => ("ok", String::new()),
            JobStatus::Failed(reason) => ("FAILED", format!("  {reason}")),
            JobStatus::Skipped => ("skipped", String::new()),
        };
        // Padded before painting: ANSI codes inside `{:8}` would defeat it.
        let status = paint_status(&result.status, &format!("{status:8}"));
        let time = if result.status == JobStatus::Skipped {
            String::new()
        } else {
            format!("{:.1}s", result.duration.as_secs_f64())
        };
        println!("{display_name}  {status}  {time}{detail}");
    }
}

/// True when every job succeeded (skipped counts as failure for exit codes).
pub fn all_succeeded(results: &[JobResult]) -> bool {
    results.iter().all(|r| r.status == JobStatus::Success)
}
