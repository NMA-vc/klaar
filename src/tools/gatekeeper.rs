use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tracing::{info, warn};

use crate::utils::safe_truncate;

use crate::config::ProjectConfig;
use crate::config::{check_path_allowed, GlobalConfig};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum wall-clock time per check before it is killed.
const CHECK_TIMEOUT_SECS: u64 = 120;

/// Maximum combined stdout+stderr stored per check (512 KB).
const OUTPUT_CAP_BYTES: usize = 512 * 1024;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub command: String,
    pub passed: bool,
    pub exit_code: Option<i32>,
    pub output: String,
}

#[derive(Debug, Serialize)]
pub struct GatekeeperResult {
    pub passed: bool,
    pub target: String,
    pub checks: Vec<CheckResult>,
}

// ---------------------------------------------------------------------------
// pre_push_check
// ---------------------------------------------------------------------------

pub async fn pre_push_check(
    project_path: &str,
    target: &str,
    config: &Arc<GlobalConfig>,
) -> Result<GatekeeperResult> {
    let root = Path::new(project_path);

    // F2: enforce trusted_projects boundary (fail-closed by default when list is empty)
    check_path_allowed(
        root,
        &config.trusted_projects,
        "pre_push_check",
        "trusted_projects",
        config.deny_commands_when_unconfigured,
    )?;

    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root = root.as_path();

    let project_config = ProjectConfig::load(root)?;

    let t = project_config
        .find_target(target)
        .ok_or_else(|| anyhow::anyhow!("Target '{}' not found in .klaar/targets.toml", target))?;

    let mut check_list: Vec<String> = t.pre_push_checks.clone();

    // Optionally run `but validate` if GitButler CLI is present
    if gitbutler_present().await {
        info!("GitButler `but` CLI detected — adding `but validate`");
        check_list.push("but validate".to_string());
    } else {
        warn!("GitButler `but` CLI not found — skipping `but validate`");
    }

    let mut results: Vec<CheckResult> = Vec::new();
    let mut all_passed = true;

    for cmd_str in &check_list {
        info!("Running check: {}", cmd_str);
        let result = run_check(cmd_str, root).await;
        if !result.passed {
            all_passed = false;
        }
        results.push(result);

        // Stop on first failure — don't waste time running further checks
        if !all_passed {
            break;
        }
    }

    if all_passed && root.join(".klaar/wiki.toml").is_file() {
        match crate::wiki::config::WikiConfig::load(root) {
            Ok(wiki) => match wiki.automation.pre_push {
                crate::wiki::config::PrePushPolicy::Disabled => {}
                crate::wiki::config::PrePushPolicy::Check => {
                    let wiki_root = root.to_path_buf();
                    let status =
                        tokio::task::spawn_blocking(move || crate::wiki::check::status(&wiki_root))
                            .await
                            .map_err(anyhow::Error::from)
                            .and_then(|result| result);
                    let result = match status {
                        Ok(status) => CheckResult {
                            command: "klaar wiki status".to_string(),
                            passed: status.current,
                            exit_code: Some(if status.current { 0 } else { 1 }),
                            output: serde_json::to_string_pretty(&status)
                                .unwrap_or_else(|_| "wiki status unavailable".to_string()),
                        },
                        Err(error) => CheckResult {
                            command: "klaar wiki status".to_string(),
                            passed: false,
                            exit_code: Some(1),
                            output: error.to_string(),
                        },
                    };
                    all_passed = result.passed;
                    results.push(result);
                }
                crate::wiki::config::PrePushPolicy::Update => {
                    let update = crate::wiki::build::build(root, None).await;
                    let result = match update {
                        Ok(report) => CheckResult {
                            command: "klaar wiki build".to_string(),
                            passed: true,
                            exit_code: Some(0),
                            output: serde_json::to_string_pretty(&report)
                                .unwrap_or_else(|_| "wiki updated".to_string()),
                        },
                        Err(error) => CheckResult {
                            command: "klaar wiki build".to_string(),
                            passed: false,
                            exit_code: Some(1),
                            output: error.to_string(),
                        },
                    };
                    all_passed = result.passed;
                    results.push(result);
                }
            },
            Err(error) => {
                all_passed = false;
                results.push(CheckResult {
                    command: "klaar wiki status".to_string(),
                    passed: false,
                    exit_code: Some(1),
                    output: error.to_string(),
                });
            }
        }
    }

    Ok(GatekeeperResult {
        passed: all_passed,
        target: target.to_string(),
        checks: results,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn run_check(cmd_str: &str, cwd: &Path) -> CheckResult {
    // Split naively (shell-free, no glob expansion)
    let mut parts = cmd_str.split_whitespace();
    let program = match parts.next() {
        Some(p) => p,
        None => {
            return CheckResult {
                command: cmd_str.to_string(),
                passed: false,
                exit_code: None,
                output: "Empty command".to_string(),
            }
        }
    };
    let args: Vec<&str> = parts.collect();

    // F3 & P1: strict resource-bounded execution
    let mut child = match Command::new(program)
        .args(&args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true) // Dropping a timed-out future requests termination of the direct child.
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                command: cmd_str.to_string(),
                passed: false,
                exit_code: None,
                output: format!("Failed to spawn process: {}", e),
            }
        }
    };

    let stdout_handle = match child.stdout.take() {
        Some(s) => s,
        None => {
            return CheckResult {
                command: cmd_str.to_string(),
                passed: false,
                exit_code: None,
                output: "Failed to pipe process stdout".to_string(),
            }
        }
    };
    let stderr_handle = match child.stderr.take() {
        Some(s) => s,
        None => {
            return CheckResult {
                command: cmd_str.to_string(),
                passed: false,
                exit_code: None,
                output: "Failed to pipe process stderr".to_string(),
            }
        }
    };

    let exec_future = async move {
        // Continue draining after the retained prefix is full so verbose children cannot
        // block forever on a full pipe. Retained memory remains strictly bounded.
        let out_fut = read_bounded(stdout_handle, OUTPUT_CAP_BYTES / 2);
        let err_fut = read_bounded(stderr_handle, OUTPUT_CAP_BYTES / 2);
        let (out_buf, err_buf, status_res) = tokio::join!(out_fut, err_fut, child.wait());
        (status_res, out_buf, err_buf)
    };

    match tokio::time::timeout(Duration::from_secs(CHECK_TIMEOUT_SECS), exec_future).await {
        Err(_elapsed) => CheckResult {
            command: cmd_str.to_string(),
            passed: false,
            exit_code: None,
            output: format!("Timed out after {}s", CHECK_TIMEOUT_SECS),
        },
        Ok((Err(error), out_buf, err_buf)) => CheckResult {
            command: cmd_str.to_string(),
            passed: false,
            exit_code: None,
            output: format!(
                "Failed to wait for process: {error}; captured {} stdout and {} stderr bytes",
                out_buf.len(),
                err_buf.len()
            ),
        },
        Ok((Ok(status), out_buf, err_buf)) => {
            let exit_code = status.code();
            let passed = status.success();

            let stdout = String::from_utf8_lossy(&out_buf).to_string();
            let stderr = String::from_utf8_lossy(&err_buf).to_string();

            let mut combined = if stderr.is_empty() {
                stdout
            } else if stdout.is_empty() {
                stderr
            } else {
                format!("{}\n---stderr---\n{}", stdout, stderr)
            };

            // Double check bounds ensuring UTF-8 boundary integrity.
            safe_truncate(&mut combined, OUTPUT_CAP_BYTES);

            CheckResult {
                command: cmd_str.to_string(),
                passed,
                exit_code,
                output: combined,
            }
        }
    }
}

async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R, limit: usize) -> Vec<u8> {
    let mut retained = Vec::with_capacity(limit.min(8 * 1024));
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let keep = read.min(limit.saturating_sub(retained.len()));
        retained.extend_from_slice(&chunk[..keep]);
    }
    retained
}

/// Detect if the GitButler `but` CLI is available on $PATH.
async fn gitbutler_present() -> bool {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    Command::new(cmd)
        .arg("but")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn verbose_check_is_drained_without_exceeding_output_cap() -> Result<()> {
        let root = tempfile::tempdir()?;
        let script = root.path().join("verbose-check");
        std::fs::write(
            &script,
            "#!/bin/sh\nhead -c 700000 /dev/zero | tr '\\0' x\n",
        )?;
        let mut permissions = std::fs::metadata(&script)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions)?;

        let result = run_check(script.to_str().unwrap(), root.path()).await;
        assert!(result.passed, "{}", result.output);
        assert!(result.output.len() <= OUTPUT_CAP_BYTES);
        Ok(())
    }
}
