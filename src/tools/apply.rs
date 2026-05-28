use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::config::{check_path_allowed, GlobalConfig};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Hunk {
    pub line_start: usize, // 1-indexed, inclusive
    pub line_end: usize,   // 1-indexed, inclusive
    pub new_content: String,
}

#[derive(Debug, Serialize)]
pub struct ApplyResult {
    pub status: &'static str,
    pub lines_changed: usize,
    pub file_path: String,
}

/// Apply surgical line replacements to a file.
/// Enforces `allowed_roots` from GlobalConfig (fail-closed by default when list is empty).
/// Hunks are processed bottom-up so earlier line numbers stay stable.
pub fn fast_apply(
    file_path: &str,
    mut hunks: Vec<Hunk>,
    config: &Arc<GlobalConfig>,
) -> Result<ApplyResult> {
    let path = Path::new(file_path);
    let canonical_path = path.canonicalize()
        .with_context(|| format!("Cannot canonicalize path: {}", file_path))?;

    // Check boundary on both the read and write path
    check_path_allowed(&canonical_path, &config.allowed_roots, "fast_apply", "allowed_roots", config.deny_when_unconfigured)?;

    let source = std::fs::read_to_string(&canonical_path)
        .with_context(|| format!("Cannot read file: {}", file_path))?;

    let mut lines: Vec<String> = source.lines().map(|l| l.to_string()).collect();
    let total_lines = lines.len();

    // Validate all hunks before applying any
    for hunk in &hunks {
        if hunk.line_start < 1 {
            anyhow::bail!(
                "Hunk line_start {} is invalid (must be >= 1)",
                hunk.line_start
            );
        }
        if hunk.line_end > total_lines {
            anyhow::bail!(
                "Hunk line_end {} exceeds file length {} — file may have changed",
                hunk.line_end,
                total_lines
            );
        }
        if hunk.line_start > hunk.line_end {
            anyhow::bail!(
                "Hunk line_start {} > line_end {}",
                hunk.line_start,
                hunk.line_end
            );
        }
    }

    // Validate overlapping hunks
    let mut sorted_hunks = hunks.clone();
    sorted_hunks.sort_by_key(|h| h.line_start);
    for i in 1..sorted_hunks.len() {
        if sorted_hunks[i].line_start <= sorted_hunks[i - 1].line_end {
            anyhow::bail!(
                "Hunks overlap: hunk {} (lines {}-{}) overlaps with hunk {} (lines {}-{})",
                i + 1, sorted_hunks[i].line_start, sorted_hunks[i].line_end,
                i, sorted_hunks[i - 1].line_start, sorted_hunks[i - 1].line_end
            );
        }
    }

    // Sort bottom-up so applying one hunk doesn't shift subsequent line numbers
    hunks.sort_by_key(|h| std::cmp::Reverse(h.line_start));

    let mut lines_changed = 0;
    for hunk in &hunks {
        let start = hunk.line_start - 1; // convert to 0-indexed
        let end = hunk.line_end - 1;     // inclusive, 0-indexed

        let replacement: Vec<String> = hunk
            .new_content
            .lines()
            .map(|l| l.to_string())
            .collect();

        let removed = end - start + 1;
        lines.splice(start..=end, replacement.iter().cloned());
        lines_changed += removed;
    }

    // Reconstruct file — preserve trailing newline if original had one
    let mut output = lines.join("\n");
    if source.ends_with('\n') {
        output.push('\n');
    }

    std::fs::write(&canonical_path, output)
        .with_context(|| format!("Cannot write file: {}", file_path))?;

    Ok(ApplyResult {
        status: "ok",
        lines_changed,
        file_path: file_path.to_string(),
    })
}
