use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{info, warn};

use crate::utils::safe_truncate;

use crate::config::{check_path_allowed, GlobalConfig};
use crate::config::ProjectConfig;

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
    check_path_allowed(root, &config.trusted_projects, "pre_push_check", "trusted_projects", config.deny_commands_when_unconfigured)?;

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
        .kill_on_drop(true) // Orphan children die if we timeout
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
        // Read streams up to OUTPUT_CAP_BYTES to prevent memory exhaustion
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();

        // Read up to half of OUTPUT_CAP_BYTES per stream so combined peak allocation is strictly <= OUTPUT_CAP_BYTES
        let mut out_take = stdout_handle.take((OUTPUT_CAP_BYTES / 2) as u64);
        let mut err_take = stderr_handle.take((OUTPUT_CAP_BYTES / 2) as u64);

        let out_fut = out_take.read_to_end(&mut out_buf);
        let err_fut = err_take.read_to_end(&mut err_buf);
        
        let (_out_res, _err_res, status_res) = tokio::join!(out_fut, err_fut, child.wait());

        let status = status_res.unwrap_or_else(|_| std::os::unix::process::ExitStatusExt::from_raw(1));
        
        // We ignore IO read errors for out/err and just accept what we buffered so far.
        (status, out_buf, err_buf)
    };

    match tokio::time::timeout(Duration::from_secs(CHECK_TIMEOUT_SECS), exec_future).await {
        Err(_elapsed) => CheckResult {
            command: cmd_str.to_string(),
            passed: false,
            exit_code: None,
            output: format!("Timed out after {}s", CHECK_TIMEOUT_SECS),
        },
        Ok((status, out_buf, err_buf)) => {
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
