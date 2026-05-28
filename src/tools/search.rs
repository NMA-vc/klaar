use regex::Regex;
use std::path::Path;
use std::process::Command;

/// A single grep match returned to the agent.
#[derive(Debug)]
pub struct GrepMatch {
    pub file: String,
    pub line: u32,
    pub text: String,
}

/// Hard cap on matches returned to prevent context blowout.
const MAX_MATCHES: usize = 50;

/// Run a codebase search. Tries ripgrep first; falls back to native Rust regex walking.
/// Returns a token-optimized, grouped output string.
pub fn search(
    pattern: &str,
    root: &str,
    case_insensitive: bool,
    include_glob: Option<&str>,
) -> Result<String, anyhow::Error> {
    if pattern.len() > 1024 {
        anyhow::bail!("Search pattern is too long (maximum 1024 characters)");
    }

    let matches = if rg_available() {
        run_rg(pattern, root, case_insensitive, include_glob)?
    } else {
        run_native(pattern, root, case_insensitive, include_glob)?
    };

    Ok(format_output(&matches, pattern))
}

// ---------------------------------------------------------------------------
// ripgrep path
// ---------------------------------------------------------------------------

fn rg_available() -> bool {
    static RG_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RG_AVAILABLE.get_or_init(|| {
        Command::new("rg").arg("--version").output().is_ok()
    })
}

fn run_rg(
    pattern: &str,
    root: &str,
    case_insensitive: bool,
    include_glob: Option<&str>,
) -> Result<Vec<GrepMatch>, anyhow::Error> {
    let mut cmd = Command::new("rg");
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--max-count=1") // one match per line (we aggregate ourselves)
        .arg("--max-filesize=1M");

    if case_insensitive {
        cmd.arg("--ignore-case");
    }
    if let Some(glob) = include_glob {
        cmd.arg("--glob").arg(glob);
    }

    cmd.arg(pattern).arg(root);

    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .spawn()?;

    let mut child_stdout = child.stdout.take().ok_or_else(|| {
        anyhow::anyhow!("Failed to open stdout of ripgrep subprocess")
    })?;

    // Spawn a background thread to read stdout to prevent deadlock on pipe buffer
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = String::new();
        use std::io::Read;
        let _ = child_stdout.read_to_string(&mut buffer);
        buffer
    });

    // Poll the child with try_wait and a strict 10-second timeout
    let timeout = std::time::Duration::from_secs(10);
    let start = std::time::Instant::now();
    let _status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    anyhow::bail!("ripgrep search timed out after 10 seconds");
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };

    let stdout = reader_thread.join().unwrap_or_default();

    let mut matches = Vec::new();
    for line in stdout.lines() {
        // rg format: path:line_number:content
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() == 3 {
            if let Ok(lineno) = parts[1].parse::<u32>() {
                matches.push(GrepMatch {
                    file: parts[0].to_string(),
                    line: lineno,
                    text: parts[2].trim().to_string(),
                });
            }
        }
        if matches.len() >= MAX_MATCHES {
            break;
        }
    }

    Ok(matches)
}

// ---------------------------------------------------------------------------
// Native Rust fallback (no rg required)
// ---------------------------------------------------------------------------

fn run_native(
    pattern: &str,
    root: &str,
    case_insensitive: bool,
    include_glob: Option<&str>,
) -> Result<Vec<GrepMatch>, anyhow::Error> {
    let re = if case_insensitive {
        Regex::new(&format!("(?i){}", pattern))?
    } else {
        Regex::new(pattern)?
    };

    // Directories we never want to search inside.
    let skip_dirs = ["node_modules", "target", ".git", ".next", "dist", "__pycache__", ".turbo", "build"];

    let mut matches = Vec::new();

    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip known noisy directories
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

        // Apply glob filter if provided (simple extension check)
        if let Some(glob) = include_glob {
            let ext_filter = glob.trim_start_matches("*.").trim_start_matches("**/*.");
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext != ext_filter {
                    continue;
                }
            } else {
                continue;
            }
        }

        // Only read text-like files
        if !is_text_file(path) {
            continue;
        }

        // Enforce 1MB file size ceiling to match ripgrep limits and avoid memory exhaustion
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > 1_000_000 {
                continue;
            }
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for (idx, line) in content.lines().enumerate() {
            if re.is_match(line) {
                matches.push(GrepMatch {
                    file: path.to_string_lossy().to_string(),
                    line: (idx + 1) as u32,
                    text: line.trim().to_string(),
                });
                if matches.len() >= MAX_MATCHES {
                    return Ok(matches);
                }
            }
        }
    }

    Ok(matches)
}

fn is_text_file(path: &Path) -> bool {
    const TEXT_EXTENSIONS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "toml", "yaml", "yml",
        "json", "md", "txt", "sh", "sql", "html", "css", "env", "lock",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEXT_EXTENSIONS.contains(&e))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Token-optimized output formatter
// ---------------------------------------------------------------------------

fn format_output(matches: &[GrepMatch], pattern: &str) -> String {
    if matches.is_empty() {
        return format!("No matches found for `{}`", pattern);
    }

    // Group by file for compact presentation
    let mut current_file = "";
    let mut out = String::new();
    let total = matches.len();

    for m in matches.iter().take(MAX_MATCHES) {
        if m.file != current_file {
            if !current_file.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("── {}\n", m.file));
            current_file = &m.file;
        }
        out.push_str(&format!("  L{}: {}\n", m.line, m.text));
    }

    if total >= MAX_MATCHES {
        out.push_str(&format!("\n[truncated — showing {MAX_MATCHES} of {total}+ matches. Refine your query.]"));
    }

    out
}
