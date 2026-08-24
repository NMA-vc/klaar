use anyhow::{Context, Result};
use serde_json::Value;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_INPUT_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: usize = 512 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratorCommand {
    executable: PathBuf,
    arguments: Vec<String>,
    environment_names: Vec<String>,
    timeout: Duration,
}

impl GeneratorCommand {
    pub fn new(executable: PathBuf, arguments: Vec<String>) -> Result<Self> {
        validate_executable(&executable)?;
        validate_arguments(&arguments)?;
        Ok(Self {
            executable,
            arguments,
            environment_names: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn with_environment(mut self, mut names: Vec<String>) -> Result<Self> {
        validate_environment_names(&names)?;
        names.sort();
        names.dedup();
        self.environment_names = names;
        Ok(self)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout < Duration::from_secs(1) || timeout > Duration::from_secs(600) {
            anyhow::bail!("generator timeout must be between 1 and 600 seconds");
        }
        self.timeout = timeout;
        Ok(self)
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    pub fn identity(&self) -> String {
        let material = format!(
            "{}\0{}\0{}\0{}",
            self.executable.to_string_lossy(),
            self.arguments.join("\0"),
            self.environment_names.join("\0"),
            self.timeout.as_secs()
        );
        crate::repo::content_hash(material.as_bytes())
    }

    pub async fn run_json(&self, request: &Value) -> Result<Value> {
        let input = serde_json::to_vec(request).context("failed to encode generator request")?;
        if input.len() > MAX_INPUT_BYTES {
            anyhow::bail!(
                "generator request exceeded the {} byte limit",
                MAX_INPUT_BYTES
            );
        }

        let mut command = Command::new(&self.executable);
        command
            .args(&self.arguments)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in &self.environment_names {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to start configured wiki generator {}",
                executable_label(&self.executable)
            )
        })?;

        let mut stdin = child
            .stdin
            .take()
            .context("failed to capture wiki generator stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to capture wiki generator stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture wiki generator stderr")?;

        let input_task = tokio::spawn(async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await
        });
        let stdout_task = tokio::spawn(read_capped(stdout, MAX_OUTPUT_BYTES));
        let stderr_task = tokio::spawn(read_capped(stderr, MAX_STDERR_BYTES));

        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(status) => status.context("failed to wait for wiki generator")?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                input_task.abort();
                stdout_task.abort();
                stderr_task.abort();
                anyhow::bail!(
                    "wiki generator timed out after {} seconds",
                    self.timeout.as_secs()
                );
            }
        };

        input_task
            .await
            .context("wiki generator stdin task failed")??;
        let stdout = stdout_task
            .await
            .context("wiki generator stdout task failed")??;
        let stderr = stderr_task
            .await
            .context("wiki generator stderr task failed")??;

        if stdout.truncated {
            anyhow::bail!(
                "wiki generator output exceeded the {} byte limit",
                MAX_OUTPUT_BYTES
            );
        }
        if !status.success() {
            anyhow::bail!(
                "wiki generator exited unsuccessfully: {}",
                sanitize_stderr(&stderr.bytes, stderr.truncated)
            );
        }
        if stdout.bytes.is_empty() {
            anyhow::bail!("wiki generator returned an empty response");
        }

        serde_json::from_slice(&stdout.bytes).context("wiki generator returned invalid JSON")
    }
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

fn validate_executable(executable: &Path) -> Result<()> {
    if executable.as_os_str().is_empty() {
        anyhow::bail!("wiki generator executable cannot be empty");
    }
    let text = executable.to_string_lossy();
    if text
        .bytes()
        .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        anyhow::bail!("wiki generator executable contains invalid characters");
    }
    if !executable.is_absolute() {
        anyhow::bail!("wiki generator executable must be an absolute path");
    }
    Ok(())
}

fn validate_environment_names(names: &[String]) -> Result<()> {
    if names.len() > 32
        || names.iter().any(|name| {
            name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    {
        anyhow::bail!("wiki generator environment names are invalid");
    }
    Ok(())
}

fn validate_arguments(arguments: &[String]) -> Result<()> {
    if arguments.len() > MAX_ARGUMENTS {
        anyhow::bail!("wiki generator accepts at most {} arguments", MAX_ARGUMENTS);
    }
    let total_bytes: usize = arguments.iter().map(String::len).sum();
    if total_bytes > MAX_ARGUMENT_BYTES {
        anyhow::bail!(
            "wiki generator arguments exceeded the {} byte limit",
            MAX_ARGUMENT_BYTES
        );
    }
    if arguments
        .iter()
        .any(|argument| argument.bytes().any(|byte| byte == 0))
    {
        anyhow::bail!("wiki generator argument contains a NUL byte");
    }
    Ok(())
}

fn executable_label(executable: &Path) -> String {
    executable
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("configured executable")
        .to_string()
}

fn sanitize_stderr(stderr: &[u8], truncated: bool) -> String {
    let text = String::from_utf8_lossy(stderr);
    if !crate::wiki::safety::scan_secrets(&text).is_empty() {
        return "sensitive generator diagnostic omitted".to_string();
    }
    let mut sanitized = String::with_capacity(text.len().min(1024));
    for character in text.chars().take(1024) {
        if character == '\n' || character == '\t' || !character.is_control() {
            sanitized.push(character);
        }
    }
    let sanitized = sanitized.trim();
    if sanitized.is_empty() {
        return "no diagnostic output".to_string();
    }
    if truncated || text.chars().count() > 1024 {
        format!("{} [truncated]", sanitized)
    } else {
        sanitized.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_command_limits_without_shell_parsing() {
        let executable = std::env::current_exe().unwrap();
        let command = GeneratorCommand::new(
            executable.clone(),
            vec!["--model".to_string(), "local".to_string()],
        )
        .unwrap();
        assert_eq!(command.executable(), executable);
        assert_eq!(command.arguments(), &["--model", "local"]);
        assert!(GeneratorCommand::new(PathBuf::new(), Vec::new()).is_err());
        assert!(
            GeneratorCommand::new(std::env::current_exe().unwrap(), vec!["x".repeat(20_000)])
                .is_err()
        );
        assert!(GeneratorCommand::new(PathBuf::from("relative"), Vec::new()).is_err());
        assert!(command
            .with_environment(vec!["PROVIDER_API_KEY".to_string()])
            .is_ok());
    }

    #[test]
    fn validates_timeout_range() {
        let command = GeneratorCommand::new(std::env::current_exe().unwrap(), Vec::new()).unwrap();
        assert!(command
            .clone()
            .with_timeout(Duration::from_secs(0))
            .is_err());
        assert!(command
            .clone()
            .with_timeout(Duration::from_secs(601))
            .is_err());
        assert!(command.with_timeout(Duration::from_secs(60)).is_ok());
    }

    #[test]
    fn sanitizes_process_diagnostics() {
        assert_eq!(sanitize_stderr(b"bad\0message\n", false), "badmessage");
        assert!(sanitize_stderr(&vec![b'x'; 2048], true).ends_with("[truncated]"));
        assert_eq!(sanitize_stderr(b"\0\0", false), "no diagnostic output");
        assert_eq!(
            sanitize_stderr(b"token=do-not-print", false),
            "sensitive generator diagnostic omitted"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn executes_json_without_inheriting_the_environment() {
        let script = r#"cat >/dev/null
        if [ -n "${HOME+x}" ]; then
            printf '{"home":"visible"}'
        else
            printf '{"home":"cleared"}'
        fi"#;
        let shell = PathBuf::from("/bin/sh");

        let cleared =
            GeneratorCommand::new(shell.clone(), vec!["-c".to_string(), script.to_string()])
                .unwrap()
                .run_json(&serde_json::json!({"request": true}))
                .await
                .unwrap();
        assert_eq!(cleared, serde_json::json!({"home": "cleared"}));

        let forwarded = GeneratorCommand::new(shell, vec!["-c".to_string(), script.to_string()])
            .unwrap()
            .with_environment(vec!["HOME".to_string()])
            .unwrap()
            .run_json(&serde_json::json!({"request": true}))
            .await
            .unwrap();
        assert_eq!(forwarded, serde_json::json!({"home": "visible"}));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_malformed_generator_json() {
        let command = GeneratorCommand::new(
            PathBuf::from("/bin/sh"),
            vec![
                "-c".to_string(),
                "cat >/dev/null; printf 'not-json'".to_string(),
            ],
        )
        .unwrap();

        let error = command.run_json(&serde_json::json!({})).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("wiki generator returned invalid JSON"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kills_a_generator_that_exceeds_its_timeout() {
        let command = GeneratorCommand::new(PathBuf::from("/bin/sleep"), vec!["2".to_string()])
            .unwrap()
            .with_timeout(Duration::from_secs(1))
            .unwrap();

        let started = std::time::Instant::now();
        let error = command.run_json(&serde_json::json!({})).await.unwrap_err();
        assert!(error.to_string().contains("timed out after 1 seconds"));
        assert!(started.elapsed() < Duration::from_millis(1_800));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_output_over_the_process_cap() {
        let command = GeneratorCommand::new(
            PathBuf::from("/bin/sh"),
            vec![
                "-c".to_string(),
                "cat >/dev/null; head -c 524289 /dev/zero | tr '\\0' x".to_string(),
            ],
        )
        .unwrap();
        let error = command.run_json(&serde_json::json!({})).await.unwrap_err();
        assert!(error.to_string().contains("output exceeded"));
    }
}
