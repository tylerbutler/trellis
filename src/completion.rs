//! Context-aware completion candidates, attached to the clap definition with
//! `#[arg(add = ...)]`. These closures run only inside a `COMPLETE=<shell>`
//! invocation — a normal run never calls them.
//!
//! This is the reason trellis uses clap_complete's runtime completion rather
//! than pre-generated scripts: a static script can only ever offer flags and
//! fixed value enums, while these read the workspace in front of you and offer
//! its actual package and task names.

use crate::workspace::Workspace;
use clap_complete::engine::{ArgValueCandidates, CompletionCandidate};

/// Load the workspace from the shell's current directory.
///
/// Completion fires with the cursor anywhere: outside a workspace, mid-edit,
/// in a half-broken repo. Every failure means "no candidates" — never an
/// error, never a panic, since a completer that fails loudly makes the shell
/// unusable.
///
/// The global `-C/--directory` flag is deliberately not consulted: the
/// completion engine hands custom completers only the partial word being
/// completed, so the process's cwd is the only context available.
fn workspace() -> Option<Workspace> {
    Workspace::load(&std::env::current_dir().ok()?).ok()
}

fn candidate(value: &str, help: String) -> CompletionCandidate {
    CompletionCandidate::new(value.to_owned()).help(Some(help.into()))
}

fn member_candidates(releasable_only: bool) -> Vec<CompletionCandidate> {
    let Some(workspace) = workspace() else {
        return Vec::new();
    };
    workspace
        .members
        .iter()
        .filter(|m| !releasable_only || m.releasable)
        .map(|m| candidate(&m.name, format!("{} v{}", m.rel_path, m.version())))
        .collect()
}

/// Every workspace member, by name.
pub fn packages() -> ArgValueCandidates {
    ArgValueCandidates::new(|| member_candidates(false))
}

/// Only members that participate in releases — the ones `publish` and
/// `changelog new` accept.
pub fn releasable_packages() -> ArgValueCandidates {
    ArgValueCandidates::new(|| member_candidates(true))
}

/// Built-in verbs plus every `[tools.trellis.tasks]` entry. The built-ins are
/// offered even outside a workspace, since they're always valid.
pub fn tasks() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        let mut out: Vec<CompletionCandidate> = crate::commands::run::BUILTIN_TASKS
            .iter()
            .map(|task| candidate(task, "built-in task".to_owned()))
            .collect();
        if let Some(workspace) = workspace() {
            out.extend(
                workspace
                    .config
                    .tasks
                    .keys()
                    .map(|task| candidate(task, "[tools.trellis.tasks]".to_owned())),
            );
        }
        out
    })
}

/// The three bump levels `--bump` accepts. The `<pkg>=<level>` form is not
/// offered: completing it would need the package name already typed, which the
/// completion engine does not hand a value completer.
pub fn bump_levels() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        [("major", "X.0.0"), ("minor", "X.Y.0"), ("patch", "X.Y.Z+1")]
            .iter()
            .map(|(level, shape)| candidate(level, (*shape).to_owned()))
            .collect()
    })
}

/// Change kinds from `[tools.trellis.changelog]`. Empty outside a workspace —
/// unlike tasks, there are no kinds that are valid without one.
pub fn changelog_kinds() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        let Some(workspace) = workspace() else {
            return Vec::new();
        };
        workspace
            .config
            .changelog
            .kinds
            .iter()
            .map(|kind| candidate(&kind.label, format!("{:?} bump", kind.bump)))
            .collect()
    })
}

/// Change categories from `[tools.trellis.changelog]`. Empty outside a
/// workspace, and inside one that configures none.
pub fn changelog_categories() -> ArgValueCandidates {
    ArgValueCandidates::new(|| {
        let Some(workspace) = workspace() else {
            return Vec::new();
        };
        workspace
            .config
            .changelog
            .categories
            .iter()
            .map(|category| candidate(category, String::new()))
            .collect()
    })
}
