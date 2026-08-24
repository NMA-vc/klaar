use crate::repo::RepositoryFacts;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct ProjectIndex {
    /// Key: Symbol name. Value: List of locations.
    pub symbols: HashMap<String, Vec<SymbolLocation>>,
}

#[derive(Debug, Clone)]
pub struct SymbolLocation {
    pub file: String,
    pub line: usize,
    pub kind: String,
}

/// Build the legacy symbol projection from the shared repository facts scanner.
pub fn build_index(root: &str) -> ProjectIndex {
    let Ok(facts) = RepositoryFacts::scan(std::path::Path::new(root)) else {
        return ProjectIndex::default();
    };
    index_from_facts(&facts)
}

fn index_from_facts(facts: &RepositoryFacts) -> ProjectIndex {
    let mut index = ProjectIndex::default();
    let mut file_paths = HashMap::with_capacity(facts.files.len());
    for file in &facts.files {
        file_paths.insert(file.id.as_str(), file.path.as_str());
    }
    for symbol in &facts.symbols {
        let Some(file) = file_paths.get(symbol.file_id.as_str()) else {
            continue;
        };
        index
            .symbols
            .entry(symbol.name.clone())
            .or_default()
            .push(SymbolLocation {
                file: (*file).to_string(),
                line: symbol.range.start_line,
                kind: symbol.kind.clone(),
            });
    }
    for locations in index.symbols.values_mut() {
        locations.sort_by(|a, b| (&a.file, a.line, &a.kind).cmp(&(&b.file, b.line, &b.kind)));
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_is_projected_from_shared_facts_in_stable_order() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir_all(temp.path().join("src"))?;
        std::fs::write(temp.path().join("src/a.rs"), "pub fn same() {}\n")?;
        std::fs::write(temp.path().join("src/b.rs"), "pub fn same() {}\n")?;

        let facts = RepositoryFacts::scan(temp.path())?;
        let index = index_from_facts(&facts);
        let locations = index.symbols.get("same").expect("symbol should exist");
        assert_eq!(locations.len(), 2);
        assert_eq!(locations[0].file, "src/a.rs");
        assert_eq!(locations[1].file, "src/b.rs");
        Ok(())
    }
}
