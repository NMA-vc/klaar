use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;

use crate::config::{check_path_allowed, GlobalConfig};
use crate::parser::{self, detect_language};

/// Read a file in full or skeleton mode.
/// Enforces `allowed_roots` from GlobalConfig (fail-closed by default when list is empty).
/// Returns `(output_string, raw_bytes_saved)`.
pub fn surgical_read(file_path: &str, mode: &str, config: &Arc<GlobalConfig>) -> Result<(String, usize, usize)> {
    let path = Path::new(file_path);
    let canonical_path = path.canonicalize()
        .with_context(|| format!("Cannot canonicalize path: {}", file_path))?;
    check_path_allowed(&canonical_path, &config.allowed_roots, "surgical_read", "allowed_roots", config.deny_when_unconfigured)?;

    let source = std::fs::read_to_string(&canonical_path)
        .with_context(|| format!("Cannot read file: {}", file_path))?;
    
    let raw_bytes = source.len();

    match mode {
        "skeleton" => {
            let lang = detect_language(&canonical_path);
            match lang {
                Some(lang) => {
                    let skeleton = parser::extract_skeleton(&source, lang);
                    let out = format!(
                        "// skeleton of {} ({} lines total)\n\n{}",
                        file_path,
                        source.lines().count(),
                        skeleton
                    );
                    let saved = raw_bytes.saturating_sub(out.len());
                    Ok((out, saved, raw_bytes))
                }
                None => {
                    let out = format!(
                        "// No tree-sitter grammar for this file type; returning full content.\n{}",
                        source
                    );
                    Ok((out, 0, raw_bytes))
                }
            }
        }
        "compressed" => {
            let lang = detect_language(&canonical_path);
            match lang {
                Some(lang) => {
                    let compressed = parser::strip_comments(&source, lang);
                    let saved = raw_bytes.saturating_sub(compressed.len());
                    Ok((compressed, saved, raw_bytes))
                }
                None => Ok((source, 0, raw_bytes)),
            }
        }
        "full" => Ok((source, 0, raw_bytes)),
        other => anyhow::bail!("Unknown mode '{}'. Use 'skeleton', 'compressed', or 'full'.", other),
    }
}
