use walkdir::WalkDir;
use std::path::Path;

/// Maximum depth for directory listing to prevent context blowout.
const DEFAULT_MAX_DEPTH: usize = 2;

/// Noisy directories to exclude from the listing.
const SKIP_DIRS: &[&str] = &[
    "node_modules", "target", ".git", ".next", "dist",
    "__pycache__", ".turbo", "build", "vendor"
];

/// List directory contents with smart noise filtering and indentation-based tree output.
pub fn list_dir(
    dir_path: &str,
    max_depth: Option<usize>,
) -> Result<String, anyhow::Error> {
    let root = Path::new(dir_path);
    if !root.exists() {
        return Err(anyhow::anyhow!("Directory does not exist: {}", dir_path));
    }

    let max_depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH).min(20);
    let mut output = String::new();
    output.push_str(&format!("📂 {}\n", root.display()));

    for entry in WalkDir::new(root)
        .max_depth(max_depth)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    return !SKIP_DIRS.contains(&name);
                }
            }
            true
        })
        .filter_map(|e| e.ok())
    {
        let depth = entry.depth();
        let indent = "  ".repeat(depth);
        let name = entry.file_name().to_string_lossy();

        let icon = if entry.file_type().is_dir() {
            "📁"
        } else {
            "📄"
        };

        output.push_str(&format!("{}{}{} {}\n", indent, icon, if entry.file_type().is_dir() { "/" } else { "" }, name));
    }

    crate::utils::safe_truncate(&mut output, 10000);
    Ok(output)
}
