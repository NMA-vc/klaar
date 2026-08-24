use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

const MAX_GIT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Copied,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathChange {
    pub kind: ChangeKind,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDelta {
    pub base: String,
    pub head: String,
    pub changes: Vec<PathChange>,
}

impl GitDelta {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn affected_paths(&self) -> Vec<&str> {
        let mut paths = Vec::new();
        for change in &self.changes {
            if let Some(previous) = change.previous_path.as_deref() {
                paths.push(previous);
            }
            paths.push(change.path.as_str());
        }
        paths.sort_unstable();
        paths.dedup();
        paths
    }
}

pub async fn compute_delta(project_root: &Path, base: &str, head: &str) -> Result<GitDelta> {
    compute_delta_with_timeout(project_root, base, head, DEFAULT_GIT_TIMEOUT).await
}

pub async fn compute_delta_with_timeout(
    project_root: &Path,
    base: &str,
    head: &str,
    timeout: Duration,
) -> Result<GitDelta> {
    validate_revision(base)?;
    validate_revision(head)?;

    let root = project_root.canonicalize().with_context(|| {
        format!(
            "cannot canonicalize project root {}",
            project_root.display()
        )
    })?;
    if !root.is_dir() {
        anyhow::bail!("project root is not a directory: {}", root.display());
    }
    if !root.join(".git").exists() {
        anyhow::bail!("project root is not a Git repository: {}", root.display());
    }

    let mut child = Command::new("git")
        .current_dir(&root)
        .args([
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--find-copies",
            "--diff-filter=ACDMR",
            base,
            head,
            "--",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to start git diff")?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture git diff stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture git diff stderr")?;
    let stdout_task = tokio::spawn(read_capped(stdout, MAX_GIT_OUTPUT_BYTES));
    let stderr_task = tokio::spawn(read_capped(stderr, 64 * 1024));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.context("failed to wait for git diff")?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            anyhow::bail!("git diff timed out after {} seconds", timeout.as_secs());
        }
    };

    let stdout = stdout_task.await.context("git diff stdout task failed")??;
    let stderr = stderr_task.await.context("git diff stderr task failed")??;

    if stdout.truncated {
        anyhow::bail!(
            "git diff output exceeded the {} byte limit",
            MAX_GIT_OUTPUT_BYTES
        );
    }
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr.bytes);
        anyhow::bail!("git diff failed: {}", message.trim());
    }

    let changes = parse_name_status_z(&stdout.bytes)?;
    Ok(GitDelta {
        base: base.to_string(),
        head: head.to_string(),
        changes,
    })
}

#[derive(Debug)]
struct CappedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_capped<R>(mut reader: R, cap: usize) -> std::io::Result<CappedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = cap.saturating_sub(bytes.len());
        if remaining > 0 {
            let accepted = remaining.min(read);
            bytes.extend_from_slice(&buffer[..accepted]);
        }
        if read > remaining {
            truncated = true;
        }
    }
    Ok(CappedOutput { bytes, truncated })
}

fn validate_revision(revision: &str) -> Result<()> {
    if revision.is_empty() || revision.len() > 256 {
        anyhow::bail!("Git revision must contain 1 to 256 characters");
    }
    if revision.starts_with('-')
        || revision
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        anyhow::bail!("invalid Git revision: revisions cannot be options or contain whitespace");
    }
    if !revision.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'/' | b'.' | b'_' | b'-' | b'+' | b'~' | b'^' | b'@' | b'{' | b'}'
            )
    }) {
        anyhow::bail!("invalid character in Git revision");
    }
    Ok(())
}

fn parse_name_status_z(output: &[u8]) -> Result<Vec<PathChange>> {
    if output.is_empty() {
        return Ok(Vec::new());
    }
    if !output.ends_with(&[0]) {
        anyhow::bail!("malformed git diff output: missing NUL terminator");
    }

    let mut fields = output[..output.len() - 1].split(|byte| *byte == 0);
    let mut changes = Vec::new();
    while let Some(raw_status) = fields.next() {
        let status =
            std::str::from_utf8(raw_status).context("git diff returned a non-UTF-8 status")?;
        let kind = status
            .as_bytes()
            .first()
            .copied()
            .context("git diff returned an empty status")?;

        match kind {
            b'R' | b'C' => {
                let previous = parse_path_field(fields.next(), "previous path")?;
                let path = parse_path_field(fields.next(), "new path")?;
                changes.push(PathChange {
                    kind: if kind == b'R' {
                        ChangeKind::Renamed
                    } else {
                        ChangeKind::Copied
                    },
                    path,
                    previous_path: Some(previous),
                });
            }
            b'A' | b'D' | b'M' => {
                let path = parse_path_field(fields.next(), "path")?;
                changes.push(PathChange {
                    kind: match kind {
                        b'A' => ChangeKind::Added,
                        b'D' => ChangeKind::Deleted,
                        _ => ChangeKind::Modified,
                    },
                    path,
                    previous_path: None,
                });
            }
            _ => anyhow::bail!("unsupported git diff status: {}", status),
        }
    }

    changes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.previous_path.cmp(&right.previous_path))
    });
    Ok(changes)
}

fn parse_path_field(field: Option<&[u8]>, label: &str) -> Result<String> {
    let raw = field.with_context(|| format!("malformed git diff output: missing {}", label))?;
    let value = std::str::from_utf8(raw)
        .with_context(|| format!("git diff returned a non-UTF-8 {}", label))?;
    normalize_repo_path(value)
}

fn normalize_repo_path(value: &str) -> Result<String> {
    if value.is_empty() || value.contains('\\') {
        anyhow::bail!("invalid repository-relative path in git diff");
    }
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("git diff path escapes the repository: {}", value);
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_modify_delete_rename_and_copy() {
        let output = b"M\0src/lib.rs\0A\0src/new.rs\0D\0src/old.rs\0R100\0src/a.rs\0src/b.rs\0C090\0src/c.rs\0src/d.rs\0";
        let changes = parse_name_status_z(output).unwrap();
        assert_eq!(changes.len(), 5);
        assert!(changes.iter().any(|change| {
            change.kind == ChangeKind::Renamed
                && change.previous_path.as_deref() == Some("src/a.rs")
                && change.path == "src/b.rs"
        }));
        assert!(changes.iter().any(|change| {
            change.kind == ChangeKind::Copied
                && change.previous_path.as_deref() == Some("src/c.rs")
                && change.path == "src/d.rs"
        }));
    }

    #[test]
    fn rejects_malformed_and_escaping_paths() {
        assert!(parse_name_status_z(b"M\0../secret\0").is_err());
        assert!(parse_name_status_z(b"M\0/absolute\0").is_err());
        assert!(parse_name_status_z(b"M\0missing-terminator").is_err());
    }

    #[test]
    fn validates_revisions_as_arguments_not_options() {
        assert!(validate_revision("origin/main").is_ok());
        assert!(validate_revision("HEAD~2").is_ok());
        assert!(validate_revision("-p").is_err());
        assert!(validate_revision("main HEAD").is_err());
        assert!(validate_revision("main:evil").is_err());
    }

    #[test]
    fn affected_paths_include_both_sides_of_rename() {
        let delta = GitDelta {
            base: "main".into(),
            head: "HEAD".into(),
            changes: vec![PathChange {
                kind: ChangeKind::Renamed,
                path: "src/new.rs".into(),
                previous_path: Some("src/old.rs".into()),
            }],
        };
        assert_eq!(delta.affected_paths(), vec!["src/new.rs", "src/old.rs"]);
    }
}
