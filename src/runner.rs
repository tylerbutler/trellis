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

const NAME_COLOR_CODES: &[u8] = &[31, 32, 33, 34, 35, 36, 91, 92, 93, 94, 95, 96];
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

#[derive(Clone)]
struct Output {
    progress: Option<Arc<MultiProgress>>,
    colors: bool,
}

impl Output {
    fn new() -> Self {
        let live =
            std::io::stdout().is_terminal() && std::env::var("TERM").as_deref() != Ok("dumb");
        Self {
            progress: live
                .then(|| Arc::new(MultiProgress::with_draw_target(ProgressDrawTarget::stdout()))),
            colors: live
                && std::env::var_os("NO_COLOR").is_none()
                && std::env::var("CLICOLOR").as_deref() != Ok("0"),
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
            progress.set_prefix(self.format_name(name, name));
            progress.set_message("starting");
            progress.enable_steady_tick(Duration::from_millis(80));
            progress
        });
        JobDisplay {
            progress,
            colors: self.colors,
        }
    }

    fn emit(&self, name: &str, width: usize, line: &str) {
        let padded = format!("{name:width$}");
        let name = self.format_name(name, &padded);
        self.println(format!("{name} ▏ {line}"));
    }

    fn println(&self, line: String) {
        if let Some(progress) = &self.progress {
            progress
                .println(line)
                .expect("failed to write progress output");
        } else {
            println!("{line}");
        }
    }

    fn format_name(&self, name: &str, display: &str) -> String {
        colorize_name(name, display, self.colors)
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
    colors: bool,
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
        let (symbol, label, color) = match status {
            JobStatus::Success => ("✓", "ok", 32),
            JobStatus::Failed(_) => ("✗", "FAILED", 31),
            JobStatus::Skipped => ("-", "skipped", 33),
        };
        let status = colorize_text(&format!("{symbol} {label}"), color, self.colors);
        progress.finish_with_message(format!("{status}  {:.1}s", duration.as_secs_f64()));
    }
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
    let output = Output::new();

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
    // Keep finished bars alive so later log lines do not erase their rows.
    let mut live_displays = Vec::new();

    std::thread::scope(|scope| -> Result<()> {
        loop {
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
                    let status = execute_job(job, &name, prefix_width, &output, &display);
                    display.finish(&status.0, status.1);
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

    output.clear_live();
    print_summary(workspace, &results, &output);
    Ok(results)
}

fn execute_job(
    job: &Job,
    name: &str,
    width: usize,
    output: &Output,
    display: &JobDisplay,
) -> (JobStatus, Duration) {
    let started = Instant::now();
    for spec in &job.commands {
        let command = spec.display();
        display.set_command(&command);
        output.emit(name, width, &format!("$ {command}"));
        match run_streaming(spec, name, width, output) {
            Ok(true) => {}
            Ok(false) => {
                return (
                    JobStatus::Failed(format!("`{command}` failed")),
                    started.elapsed(),
                );
            }
            Err(err) => {
                output.emit(name, width, &format!("error: {err:#}"));
                return (JobStatus::Failed(format!("{err:#}")), started.elapsed());
            }
        }
    }
    (JobStatus::Success, started.elapsed())
}

/// Run one command, streaming stdout and stderr lines with the `pkg ▏` prefix.
fn run_streaming(spec: &CommandSpec, name: &str, width: usize, output: &Output) -> Result<bool> {
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
    Ok(child.wait()?.success())
}

fn stable_name_hash(name: &str) -> u64 {
    name.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn colorize_name(name: &str, display: &str, enabled: bool) -> String {
    colorize_text(display, name_color_code(name), enabled)
}

fn name_color_code(name: &str) -> u8 {
    let index = stable_name_hash(name) as usize % NAME_COLOR_CODES.len();
    NAME_COLOR_CODES[index]
}

fn colorize_text(text: &str, color: u8, enabled: bool) -> String {
    if enabled {
        format!("\x1b[1;{color}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn print_summary(workspace: &Workspace, results: &[JobResult], output: &Output) {
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
        let padded_name = format!("{name:width$}");
        let display_name = output.format_name(name, &padded_name);
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
        println!("{display_name}  {status:8}  {time}{detail}");
    }
}

/// True when every job succeeded (skipped counts as failure for exit codes).
pub fn all_succeeded(results: &[JobResult]) -> bool {
    results.iter().all(|r| r.status == JobStatus::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_colors_are_stable_and_hash_based() {
        assert_eq!(name_color_code("lat_core"), 35);
        assert_eq!(name_color_code("lat_core"), name_color_code("lat_core"));
        assert_ne!(name_color_code("lat_core"), name_color_code("lat_mid"));
    }

    #[test]
    fn package_name_colors_can_be_disabled() {
        assert_eq!(colorize_name("lat_core", "lat_core", false), "lat_core");
    }
}
