use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ProjectStandards {
    pub global: Option<Vec<String>>,
    pub languages: Option<HashMap<String, Vec<String>>>,
}

#[derive(Clone)]
struct CachedStandards {
    standards: ProjectStandards,
    last_modified: Option<std::time::SystemTime>,
}

static STANDARDS_CACHE: OnceLock<Mutex<HashMap<String, CachedStandards>>> = OnceLock::new();

/// Loads standards from .klaar/standards.toml in the project root.
pub fn load_standards(project_root: &str) -> ProjectStandards {
    let path = Path::new(project_root).join(".klaar").join("standards.toml");
    let current_modified = path.metadata().and_then(|m| m.modified()).ok();

    let cache = STANDARDS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let map = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = map.get(project_root) {
            if cached.last_modified == current_modified {
                return cached.standards.clone();
            }
        }
    }

    let standards = if !path.exists() {
        ProjectStandards::default()
    } else {
        match std::fs::read_to_string(&path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to parse standards.toml: {}", e);
                    ProjectStandards::default()
                }
            },
            Err(e) => {
                warn!("Failed to read standards.toml: {}", e);
                ProjectStandards::default()
            }
        }
    };

    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(
        project_root.to_string(),
        CachedStandards {
            standards: standards.clone(),
            last_modified: current_modified,
        },
    );
    standards
}

/// Injects relevant standards into a tool response.
pub fn inject_standards(
    content: &mut String,
    file_path: &str,
    project_root: Option<&str>,
) {
    let mut standards_block = String::new();

    // 1. Project-specific standards
    if let Some(root) = project_root {
        let standards = load_standards(root);
        
        // Global standards
        if let Some(global) = standards.global {
            for s in global {
                standards_block.push_str(&format!("- {}\n", s));
            }
        }

        // Language-specific standards
        if let Some(langs) = standards.languages {
            let ext = Path::new(file_path).extension().and_then(|e| e.to_str()).unwrap_or("");
            if let Some(lang_standards) = langs.get(ext) {
                for s in lang_standards {
                    standards_block.push_str(&format!("- {}\n", s));
                }
            }
        }
    }

    // 2. Built-in Reference Cards (Hardcoded for maximum ROI)
    if file_path.contains("surreal") || content.contains("surreal") {
        standards_block.push_str("\n[REF CARD: SurrealDB Parameterized Queries]\n");
        standards_block.push_str("- Use `$var` placeholders: `db.query(\"SELECT * FROM user WHERE id = $id\").bind((\"id\", id_val))`\n");
        standards_block.push_str("- NEVER use string interpolation for queries.\n");
    }

    if file_path.ends_with(".rs") && content.contains("tokio") {
        standards_block.push_str("\n[REF CARD: Tokio Async]\n");
        standards_block.push_str("- Prefer `?` over `unwrap()` in async tasks.\n");
        standards_block.push_str("- Use `tokio::select!` for cancellation-aware loops.\n");
    }

    if !standards_block.is_empty() {
        content.push_str("\n\n--- [klaar standards & best practices] ---\n");
        content.push_str(&standards_block);
        content.push_str("------------------------------------------\n");
    }
}
