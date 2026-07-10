//! `trellis graph` — render the computed dependency graph. The mermaid output
//! keeps docs diagrams generated instead of hand-drawn.

use crate::workspace::Workspace;
use anyhow::Result;
use serde_json::json;

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
                println!("{} ({})", member.name, member.version());
                let deps = workspace.deps_of(idx);
                for (pos, &dep) in deps.iter().enumerate() {
                    let branch = if pos + 1 == deps.len() {
                        "└─"
                    } else {
                        "├─"
                    };
                    println!("  {branch} {}", workspace.members[dep].name);
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
            let nodes: Vec<_> = workspace
                .members
                .iter()
                .map(|member| {
                    json!({
                        "name": member.name,
                        "version": member.version(),
                        "path": member.rel_path,
                        "releasable": member.releasable,
                    })
                })
                .collect();
            let mut edges = Vec::new();
            for (idx, member) in workspace.members.iter().enumerate() {
                for &dep in workspace.deps_of(idx) {
                    edges.push(json!({
                        "from": member.name,
                        "to": workspace.members[dep].name,
                    }));
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "nodes": nodes, "edges": edges }))?
            );
        }
    }
    Ok(())
}
