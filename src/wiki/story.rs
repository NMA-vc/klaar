use crate::repo::RepositoryFacts;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_STORY_NODES: usize = 30;
const MAX_STORY_EDGES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchitectureStory {
    pub schema_version: u32,
    pub context: String,
    pub subsystems: String,
    pub static_dependencies: String,
    pub configured_commands: String,
    pub security_boundaries: String,
    pub diagnostics: Vec<String>,
}

impl ArchitectureStory {
    /// Render bounded Mermaid views from static repository facts only.
    pub fn from_facts(facts: &RepositoryFacts) -> Self {
        let mut diagnostics = Vec::new();
        if facts.components.len() > MAX_STORY_NODES {
            diagnostics.push(format!(
                "component view truncated at {MAX_STORY_NODES} nodes"
            ));
        }
        if facts.dependencies.len() > MAX_STORY_EDGES {
            diagnostics.push(format!(
                "dependency view truncated at {MAX_STORY_EDGES} edges"
            ));
        }
        Self {
            schema_version: 1,
            context: context_diagram(facts),
            subsystems: subsystem_diagram(facts),
            static_dependencies: dependency_diagram(facts),
            configured_commands: command_diagram(facts),
            security_boundaries: security_diagram(),
            diagnostics,
        }
    }
}

fn context_diagram(facts: &RepositoryFacts) -> String {
    let language_count = facts
        .files
        .iter()
        .map(|file| format!("{:?}", file.language))
        .collect::<BTreeSet<_>>()
        .len();
    format!(
        "flowchart LR\n  developer[Developer or agent] --> repository[Repository: {} files]\n  repository --> artifacts[Compiled wiki]\n  repository -. static evidence .-> artifacts\n  languages[{} detected languages] --> repository\n",
        facts.files.len(),
        language_count
    )
}

fn subsystem_diagram(facts: &RepositoryFacts) -> String {
    let mut output = String::from("flowchart TD\n");
    let mut file_component = BTreeMap::new();
    for (index, component) in facts.components.iter().take(MAX_STORY_NODES).enumerate() {
        let node = format!("c{index}");
        output.push_str(&format!("  {node}[\"{}\"]\n", label(&component.name)));
        for file_id in &component.file_ids {
            file_component.insert(file_id.as_str(), node.clone());
        }
    }
    let mut edges = BTreeSet::new();
    for dependency in facts.dependencies.iter().take(MAX_STORY_EDGES) {
        let Some(from) = file_component.get(dependency.from_file_id.as_str()) else {
            continue;
        };
        let Some(to) = file_component.get(dependency.to_file_id.as_str()) else {
            continue;
        };
        if from != to {
            edges.insert((from.clone(), to.clone()));
        }
    }
    for (from, to) in edges {
        output.push_str(&format!("  {from} -. static import .-> {to}\n"));
    }
    output
}

fn dependency_diagram(facts: &RepositoryFacts) -> String {
    let paths: BTreeMap<_, _> = facts
        .files
        .iter()
        .enumerate()
        .take(MAX_STORY_NODES)
        .map(|(index, file)| (file.id.as_str(), (format!("f{index}"), file.path.as_str())))
        .collect();
    let mut output = String::from("flowchart LR\n");
    for (node, path) in paths.values() {
        output.push_str(&format!("  {node}[\"{}\"]\n", label(path)));
    }
    for dependency in facts.dependencies.iter().take(MAX_STORY_EDGES) {
        if let (Some((from, _)), Some((to, _))) = (
            paths.get(dependency.from_file_id.as_str()),
            paths.get(dependency.to_file_id.as_str()),
        ) {
            output.push_str(&format!(
                "  {from} -. \"{} (static)\" .-> {to}\n",
                label(&dependency.kind)
            ));
        }
    }
    output
}

fn command_diagram(facts: &RepositoryFacts) -> String {
    let mut output = String::from("flowchart LR\n  user[User or CI]\n");
    let mut command_index = 0usize;
    for manifest in &facts.manifests {
        for command in &manifest.commands {
            if command_index >= MAX_STORY_NODES {
                break;
            }
            output.push_str(&format!(
                "  user -->|configured command| cmd{command_index}[\"{}\"]\n",
                label(command)
            ));
            command_index += 1;
        }
    }
    if command_index == 0 {
        output.push_str("  user -. no trusted configured commands discovered .-> unknown[Unknown runtime flow]\n");
    }
    output
}

fn security_diagram() -> String {
    "flowchart LR\n  source[Untrusted repository content] --> filter[Exclusions and secret filter]\n  filter --> evidence[Bounded evidence pack]\n  evidence --> generator[Untrusted draft generator]\n  generator --> compiler[Safe output compiler]\n  compiler --> wiki[Portable wiki artifacts]\n"
        .to_string()
}

fn label(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(64));
    for character in value.chars().take(64) {
        match character {
            '"' | '<' | '>' | '[' | ']' | '{' | '}' | '`' => output.push(' '),
            '\n' | '\r' | '\t' => output.push(' '),
            character if character.is_control() => {}
            character => output.push(character),
        }
    }
    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn story_is_bounded_and_labels_static_edges() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("main.go"), "package main\n")?;
        let facts = RepositoryFacts::scan(root.path())?;
        let story = ArchitectureStory::from_facts(&facts);
        assert!(story.context.contains("Compiled wiki"));
        assert!(story.security_boundaries.contains("Safe output compiler"));
        assert!(!story.static_dependencies.contains("javascript:"));
        Ok(())
    }
}
