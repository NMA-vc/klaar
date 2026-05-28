/// Safe string truncation that ensures we never split a multi-byte UTF-8 character.
pub fn safe_truncate(s: &mut String, max_bytes: usize) {
    if s.len() > max_bytes {
        let mut target = max_bytes;
        // Walk backwards until we hit a valid char boundary
        while target > 0 && !s.is_char_boundary(target) {
            target -= 1;
        }
        s.truncate(target);
        s.push_str("\n... [output truncated]");
    }
}

use similar::{ChangeTag, TextDiff};

pub struct DiffStats {
    pub ratio: f32,
    pub diff: String,
}

/// Cheaply computes only the diff ratio without any String allocations or formatting.
pub fn compute_diff_ratio(old: &str, new: &str) -> f32 {
    let diff = TextDiff::from_lines(old, new);
    let mut changes = 0;
    let mut total = 0;

    for change in diff.iter_all_changes() {
        total += 1;
        match change.tag() {
            ChangeTag::Delete | ChangeTag::Insert => {
                changes += 1;
            }
            ChangeTag::Equal => {}
        }
    }

    if total == 0 {
        0.0
    } else {
        (changes as f32) / (total as f32)
    }
}

/// Computes diff stats and ratio in a single pass.
pub fn compute_diff_stats(old: &str, new: &str) -> DiffStats {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    let mut changes = 0;
    let mut total = 0;

    for change in diff.iter_all_changes() {
        total += 1;
        let sign = match change.tag() {
            ChangeTag::Delete => {
                changes += 1;
                "-"
            }
            ChangeTag::Insert => {
                changes += 1;
                "+"
            }
            ChangeTag::Equal => " ",
        };
        out.push_str(&format!("{}{}", sign, change));
    }

    let ratio = if total == 0 {
        0.0
    } else {
        changes as f32 / total as f32
    };

    DiffStats { ratio, diff: out }
}

/// Detects the project root by looking for .git, .klaar, or Cargo.toml.
pub fn find_project_root(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    for ancestor in p.ancestors() {
        if ancestor.join(".git").exists() || ancestor.join(".klaar").exists() || ancestor.join("Cargo.toml").exists() {
            return Some(ancestor.to_string_lossy().to_string());
        }
    }
    None
}
