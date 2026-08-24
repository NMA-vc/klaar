use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const DEFAULT_MAX_NODES: usize = 30;
const MAX_NODES: usize = 60;
#[derive(Debug)]
struct SourceFile {
    relative: String,
}

#[derive(Debug, Default)]
struct CodeGraph {
    files: Vec<SourceFile>,
    edges: HashSet<(usize, usize)>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
enum DiagramView {
    Overview,
    Dependencies,
    Hotspots,
}

impl DiagramView {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "overview" => Ok(Self::Overview),
            "dependencies" => Ok(Self::Dependencies),
            "hotspots" => Ok(Self::Hotspots),
            other => anyhow::bail!(
                "Unknown diagram view '{}'. Use 'overview', 'dependencies', or 'hotspots'.",
                other
            ),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Dependencies => "dependencies",
            Self::Hotspots => "hotspots",
        }
    }
}

/// Generate a bounded Mermaid diagram from static module/import evidence.
pub fn generate_codebase_diagram(
    project_path: &str,
    view: &str,
    focus: Option<&str>,
    max_nodes: Option<usize>,
) -> Result<String> {
    let root = Path::new(project_path)
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize project path: {}", project_path))?;
    if !root.is_dir() {
        anyhow::bail!("Project path is not a directory: {}", root.display());
    }

    let view = DiagramView::parse(view)?;
    let max_nodes = max_nodes.unwrap_or(DEFAULT_MAX_NODES).clamp(5, MAX_NODES);
    let graph = scan_project(&root)?;
    if graph.files.is_empty() {
        anyhow::bail!("No supported source files found in {}", root.display());
    }

    let body = match view {
        DiagramView::Overview => render_overview(&graph, max_nodes),
        DiagramView::Dependencies => render_file_graph(&graph, focus, max_nodes, false)?,
        DiagramView::Hotspots => render_file_graph(&graph, focus, max_nodes, true)?,
    };

    let diagnostics = if graph.diagnostics.is_empty() {
        String::new()
    } else {
        format!("\n\nScan diagnostics: {}.", graph.diagnostics.join("; "))
    };
    Ok(format!(
        "# Codebase diagram: {}\n\n{}\n\nEvidence: scanned {} supported source files and inferred {} static import/module edges. Runtime dispatch, reflection, generated code, and external package internals are not represented.{}",
        view.label(),
        body,
        graph.files.len(),
        graph.edges.len(),
        diagnostics
    ))
}

fn scan_project(root: &Path) -> Result<CodeGraph> {
    let facts = crate::repo::RepositoryFacts::scan(root)?;
    let mut files = Vec::new();
    let mut file_indexes = HashMap::new();
    for file in &facts.files {
        let index = files.len();
        files.push(SourceFile {
            relative: file.path.clone(),
        });
        file_indexes.insert(file.id.as_str(), index);
    }

    let mut edges = HashSet::new();
    for edge in &facts.dependencies {
        if let (Some(from), Some(to)) = (
            file_indexes.get(edge.from_file_id.as_str()),
            file_indexes.get(edge.to_file_id.as_str()),
        ) {
            edges.insert((*from, *to));
        }
    }
    let diagnostics = facts
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.truncated)
        .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
        .collect();
    Ok(CodeGraph {
        files,
        edges,
        diagnostics,
    })
}
fn render_overview(graph: &CodeGraph, max_nodes: usize) -> String {
    let mut file_groups = Vec::with_capacity(graph.files.len());
    let mut group_counts: HashMap<String, usize> = HashMap::new();
    for file in &graph.files {
        let group = architecture_group(&file.relative);
        *group_counts.entry(group.clone()).or_default() += 1;
        file_groups.push(group);
    }

    let mut group_edges: HashMap<(String, String), usize> = HashMap::new();
    for (from, to) in &graph.edges {
        let source = &file_groups[*from];
        let target = &file_groups[*to];
        if source != target {
            *group_edges
                .entry((source.clone(), target.clone()))
                .or_default() += 1;
        }
    }

    let mut groups: Vec<String> = group_counts.keys().cloned().collect();
    groups.sort_by_key(|group| {
        let edge_degree = group_edges
            .iter()
            .filter(|((from, to), _)| from == group || to == group)
            .map(|(_, count)| count)
            .sum::<usize>();
        (std::cmp::Reverse(edge_degree), group.clone())
    });
    let total_groups = groups.len();
    groups.truncate(max_nodes);
    groups.sort();
    let selected: HashSet<&str> = groups.iter().map(String::as_str).collect();
    let ids: HashMap<&str, String> = groups
        .iter()
        .enumerate()
        .map(|(index, group)| (group.as_str(), format!("n{}", index)))
        .collect();

    let mut out = String::from("```mermaid\nflowchart LR\n");
    for group in &groups {
        out.push_str(&format!(
            "    {}[\"{}<br/>{} source files\"]\n",
            ids[group.as_str()],
            safe_label(group),
            group_counts[group]
        ));
    }
    let mut edges: Vec<_> = group_edges.into_iter().collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0));
    for ((from, to), count) in edges {
        if selected.contains(from.as_str()) && selected.contains(to.as_str()) {
            out.push_str(&format!(
                "    {} -->|{} imports| {}\n",
                ids[from.as_str()],
                count,
                ids[to.as_str()]
            ));
        }
    }
    out.push_str("```\n");
    if total_groups > groups.len() {
        out.push_str(&format!(
            "\nTruncated to {} of {} subsystems. Increase `max_nodes` or request a focused dependency view.",
            groups.len(), total_groups
        ));
    }
    out
}

fn render_file_graph(
    graph: &CodeGraph,
    focus: Option<&str>,
    max_nodes: usize,
    hotspots: bool,
) -> Result<String> {
    let degrees = file_degrees(graph);
    let focus_matches: HashSet<usize> = focus
        .map(|needle| {
            let needle = needle.to_lowercase();
            graph
                .files
                .iter()
                .enumerate()
                .filter(|(_, file)| file.relative.to_lowercase().contains(&needle))
                .map(|(index, _)| index)
                .collect()
        })
        .unwrap_or_default();
    if focus.is_some() && focus_matches.is_empty() {
        anyhow::bail!(
            "Focus '{}' did not match any supported source path.",
            focus.unwrap_or_default()
        );
    }

    let mut candidates: HashSet<usize> = if focus_matches.is_empty() {
        (0..graph.files.len()).collect()
    } else {
        let mut candidates = focus_matches.clone();
        for (from, to) in &graph.edges {
            if focus_matches.contains(from) || focus_matches.contains(to) {
                candidates.insert(*from);
                candidates.insert(*to);
            }
        }
        candidates
    };
    let total_candidates = candidates.len();
    let mut selected: Vec<usize> = candidates.drain().collect();
    selected.sort_by_key(|index| {
        (
            std::cmp::Reverse(focus_matches.contains(index)),
            std::cmp::Reverse(degrees[*index].0 + degrees[*index].1),
            graph.files[*index].relative.clone(),
        )
    });
    selected.truncate(max_nodes);
    selected.sort_by_key(|index| graph.files[*index].relative.clone());
    let selected_set: HashSet<usize> = selected.iter().copied().collect();
    let ids: HashMap<usize, String> = selected
        .iter()
        .enumerate()
        .map(|(position, index)| (*index, format!("n{}", position)))
        .collect();

    let mut out = String::from("```mermaid\nflowchart LR\n");
    for index in &selected {
        let (incoming, outgoing) = degrees[*index];
        let detail = if hotspots {
            format!("<br/>in {} / out {}", incoming, outgoing)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "    {}[\"{}{}\"]\n",
            ids[index],
            safe_label(&graph.files[*index].relative),
            detail
        ));
    }
    let mut edges: Vec<_> = graph.edges.iter().copied().collect();
    edges.sort();
    for (from, to) in edges {
        if selected_set.contains(&from) && selected_set.contains(&to) {
            out.push_str(&format!("    {} --> {}\n", ids[&from], ids[&to]));
        }
    }
    if hotspots && !selected.is_empty() {
        let threshold = selected
            .iter()
            .map(|index| degrees[*index].0 + degrees[*index].1)
            .max()
            .unwrap_or_default()
            .max(1);
        let hot: Vec<&str> = selected
            .iter()
            .filter(|index| degrees[**index].0 + degrees[**index].1 == threshold)
            .map(|index| ids[index].as_str())
            .collect();
        if !hot.is_empty() {
            out.push_str(
                "    classDef hotspot fill:#ffedd5,stroke:#c2410c,stroke-width:2px,color:#111827\n",
            );
            out.push_str(&format!("    class {} hotspot\n", hot.join(",")));
        }
    }
    out.push_str("```\n");
    if total_candidates > selected.len() {
        out.push_str(&format!(
            "\nTruncated to {} of {} candidate files. Use `focus` or increase `max_nodes` for a different slice.",
            selected.len(), total_candidates
        ));
    }
    Ok(out)
}

fn file_degrees(graph: &CodeGraph) -> Vec<(usize, usize)> {
    let mut degrees = vec![(0, 0); graph.files.len()];
    for (from, to) in &graph.edges {
        degrees[*from].1 += 1;
        degrees[*to].0 += 1;
    }
    degrees
}

fn architecture_group(relative: &str) -> String {
    let parts: Vec<&str> = relative.split('/').collect();
    match parts.as_slice() {
        ["src" | "app" | "lib" | "packages" | "pkg", second, ..] => {
            format!("{}/{}", parts[0], second)
        }
        [first, ..] => (*first).to_string(),
        [] => "project".to_string(),
    }
}

fn safe_label(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('[', "(")
        .replace(']', ")")
        .replace(['\r', '\n'], " ")
        .replace('`', "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_rust_overview_and_dependencies() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let src = temp.path().join("src");
        std::fs::create_dir_all(src.join("tools"))?;
        std::fs::write(src.join("lib.rs"), "pub mod db;\npub mod tools;\n")?;
        std::fs::write(src.join("db.rs"), "pub struct Store;\n")?;
        std::fs::write(src.join("tools/mod.rs"), "pub mod read;\n")?;
        std::fs::write(
            src.join("tools/read.rs"),
            "use crate::db::Store;\npub fn read(_: Store) {}\n",
        )?;

        let root = temp.path().to_string_lossy();
        let overview = generate_codebase_diagram(&root, "overview", None, Some(20))?;
        assert!(overview.contains("src/tools"));
        assert!(overview.contains("src/db.rs"));
        assert!(overview.contains("imports"));

        let dependencies =
            generate_codebase_diagram(&root, "dependencies", Some("read.rs"), Some(20))?;
        assert!(dependencies.contains("src/tools/read.rs"));
        assert!(dependencies.contains("src/db.rs"));
        assert!(dependencies.contains("-->"));
        Ok(())
    }

    #[test]
    fn rejects_unknown_view() {
        let temp = tempfile::tempdir().unwrap();
        let error =
            generate_codebase_diagram(&temp.path().to_string_lossy(), "sequence", None, None)
                .unwrap_err();
        assert!(error.to_string().contains("Unknown diagram view"));
    }
}
