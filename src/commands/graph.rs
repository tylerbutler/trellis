//! `trellis graph` — render the computed dependency graph as text, Graphviz
//! dot, Mermaid, or JSON.

use crate::json::GraphDocument;
use crate::workspace::Workspace;
use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GraphFormat {
    Text,
    Dot,
    Mermaid,
    Json,
}

pub fn run(workspace: &Workspace, format: GraphFormat) -> Result<()> {
    match format {
        GraphFormat::Text => {
            for (idx, member) in workspace.members.iter().enumerate() {
                crate::status!(
                    "{} {}",
                    crate::term::package(&member.name),
                    crate::term::dim(&format!("({})", member.version()))
                );
                let deps = workspace.deps_of(idx);
                for (pos, &dep) in deps.iter().enumerate() {
                    let branch = if pos + 1 == deps.len() {
                        "└─"
                    } else {
                        "├─"
                    };
                    crate::status!(
                        "  {} {}",
                        crate::term::dim(branch),
                        crate::term::package(&workspace.members[dep].name)
                    );
                }
            }
        }
        GraphFormat::Dot => {
            println!("digraph workspace {{");
            println!("  rankdir=BT;");
            for member in &workspace.members {
                println!("  \"{}\";", member.name);
            }
            for (idx, member) in workspace.members.iter().enumerate() {
                for &dep in workspace.deps_of(idx) {
                    // Arrow points at the dependency.
                    println!(
                        "  \"{}\" -> \"{}\";",
                        member.name, workspace.members[dep].name
                    );
                }
            }
            println!("}}");
        }
        GraphFormat::Mermaid => {
            println!("graph TD");
            let mut printed_any_edge = false;
            for (idx, member) in workspace.members.iter().enumerate() {
                for &dep in workspace.deps_of(idx) {
                    println!("    {} --> {}", member.name, workspace.members[dep].name);
                    printed_any_edge = true;
                }
            }
            for (idx, member) in workspace.members.iter().enumerate() {
                if workspace.deps_of(idx).is_empty() && workspace.dependents_of(idx).is_empty() {
                    println!("    {}", member.name);
                    printed_any_edge = true;
                }
            }
            if !printed_any_edge {
                println!("    %% empty workspace");
            }
        }
        GraphFormat::Json => {
            let document = GraphDocument::new(workspace);
            println!("{}", serde_json::to_string_pretty(&document)?);
        }
    }
    Ok(())
}
