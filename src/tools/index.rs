use std::collections::HashMap;
use walkdir::WalkDir;
use crate::parser::{detect_language, extract_symbols};

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

/// Build a symbol index for the given project root.
pub fn build_index(root: &str) -> ProjectIndex {
    let mut index = ProjectIndex::default();
    
    // Directories to skip
    let skip_dirs = ["node_modules", "target", ".git", ".next", "dist", "__pycache__", ".turbo", "build"];

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    return !skip_dirs.contains(&name);
                }
            }
            true
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if let Some(lang) = detect_language(path) {
            // Skip files larger than 1 MB to prevent heap exhaustion on generated files
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() > 1_048_576 {
                    continue;
                }
            }
            if let Ok(content) = std::fs::read_to_string(path) {
                let symbols = extract_symbols(&content, lang);
                let relative_path = path.strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string();

                for s in symbols {
                    index.symbols.entry(s.name).or_default().push(SymbolLocation {
                        file: relative_path.clone(),
                        line: s.line,
                        kind: s.kind,
                    });
                }
            }
        }
    }

    index
}
