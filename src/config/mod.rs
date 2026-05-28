use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};


// ---------------------------------------------------------------------------
// Global config (~/.config/klaar/config.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct GlobalConfig {
    /// Override the SurrealDB storage path. Default: ~/.local/share/klaar/db
    pub db_path: Option<PathBuf>,
    /// Log level filter string (e.g. "info", "debug"). Default: "info"
    pub log_level: Option<String>,
    /// Optional file-path allowlist for surgical_read and fast_apply.
    /// If empty (default), all paths are allowed with a warning.
    /// Set this to restrict which directories agents can read/write.
    /// Example: allowed_roots = ["/Users/me/Projects"]
    #[serde(default)]
    pub allowed_roots: Vec<PathBuf>,
    /// Optional project allowlist for pre_push_check command execution.
    /// If empty (default), all project paths are allowed with a warning.
    /// Set this to restrict which repos klaar will run commands in.
    /// Example: trusted_projects = ["/Users/me/Projects/my-project"]
    #[serde(default)]
    pub trusted_projects: Vec<PathBuf>,
    /// Whether to fail closed when allowed_roots is empty.
    /// Default: true (fail closed and deny access)
    #[serde(default)]
    pub deny_when_unconfigured: Option<bool>,
    /// Whether to fail closed when trusted_projects is empty for command execution.
    /// Default: true (fail closed and deny execution)
    #[serde(default)]
    pub deny_commands_when_unconfigured: Option<bool>,
}

impl GlobalConfig {
    pub fn load() -> Result<Self> {
        let path = global_config_path();
        let mut cfg = if !path.exists() {
            GlobalConfig::default()
        } else {
            let raw = std::fs::read_to_string(&path)?;
            let loaded: GlobalConfig = toml::from_str(&raw)?;
            loaded
        };

        // Pre-canonicalize allowed_roots
        cfg.allowed_roots = cfg.allowed_roots.into_iter()
            .map(|r| match r.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Failed to canonicalize allowed_root '{}': {}", r.display(), e);
                    r
                }
            })
            .collect();

        // Pre-canonicalize trusted_projects
        cfg.trusted_projects = cfg.trusted_projects.into_iter()
            .map(|r| match r.canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Failed to canonicalize trusted_project '{}': {}", r.display(), e);
                    r
                }
            })
            .collect();

        Ok(cfg)
    }

    /// Resolved DB path: explicit override or default location.
    pub fn resolved_db_path(&self) -> PathBuf {
        self.db_path.clone().unwrap_or_else(default_db_path)
    }
}

fn global_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("klaar")
        .join("config.toml")
}

fn default_db_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("klaar")
        .join("db")
}

// ---------------------------------------------------------------------------
// Path boundary guard
// ---------------------------------------------------------------------------

/// Check whether `path` is inside one of the configured `roots`.
///
/// Behaviour:
/// - If `roots` is empty:
///   - Denied by default if `deny_when_unconfigured` is true or unset (fail-closed posture).
///   - Allowed if `deny_when_unconfigured` is explicitly set to false, but a warning is logged.
/// - If `roots` is non-empty → path must canonicalize and start with one root.
///   Returns `Err` if denied.
pub fn check_path_allowed(
    path: &Path,
    roots: &[PathBuf],
    context: &str,
    config_key: &str,
    deny_when_unconfigured: Option<bool>,
) -> Result<()> {
    if roots.is_empty() {
        let deny = deny_when_unconfigured.unwrap_or(true);
        if deny {
            return Err(anyhow::anyhow!(
                "Access denied: no boundary roots configured for {} ({}). Configure {} in ~/.config/klaar/config.toml.",
                context,
                config_key,
                config_key
            ));
        }
        static WARN_ALLOWED_ROOTS: std::sync::Once = std::sync::Once::new();
        static WARN_TRUSTED_PROJECTS: std::sync::Once = std::sync::Once::new();
        
        let once_gate = match config_key {
            "trusted_projects" => &WARN_TRUSTED_PROJECTS,
            _ => &WARN_ALLOWED_ROOTS,
        };
        
        once_gate.call_once(|| {
            tracing::warn!(
                "{}: no {} configured — allowing access to {} (set {} in \
                 ~/.config/klaar/config.toml to restrict)",
                context,
                config_key,
                path.display(),
                config_key
            );
        });
        return Ok(());
    }

    // Resolve symlinks and relative components
    let canonical = path.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "Cannot resolve path {}: {} (does it exist?)",
            path.display(),
            e
        )
    })?;

    let allowed = roots.iter().any(|root| {
        if canonical.starts_with(root) {
            true // Fast match: pre-canonicalized roots match instantly without syscalls
        } else {
            // Deferred Fallback: only canonicalize dynamically if the fast match failed
            // (e.g. because root was symlinked and failed to canonicalize at startup).
            root.canonicalize().map(|r| canonical.starts_with(&r)).unwrap_or(false)
        }
    });

    if allowed {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Access denied: {} is outside {}.\n\
             Add it to ~/.config/klaar/config.toml:\n\
             \n\
             {} = [\"{}\"]",
            canonical.display(),
            config_key,
            config_key,
            canonical
                .parent()
                .unwrap_or(&canonical)
                .display()
        ))
    }
}

// ---------------------------------------------------------------------------
// Per-project config (.klaar/targets.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProjectConfig {
    pub project: ProjectMeta,
    #[serde(default)]
    pub targets: Vec<Target>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProjectMeta {
    pub name: String,
    /// "rust" | "typescript" | "mixed"
    pub language: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Target {
    pub name: String,
    #[serde(default)]
    pub pre_push_checks: Vec<String>,
    pub deploy_method: Option<String>,
    pub deploy_command: Option<String>,
}

impl ProjectConfig {
    pub fn load(project_root: &Path) -> Result<Self> {
        let path = project_root.join(".klaar").join("targets.toml");
        let raw = std::fs::read_to_string(&path)
            .map_err(|_| anyhow::anyhow!("No .klaar/targets.toml found in {}", project_root.display()))?;
        let cfg: ProjectConfig = toml::from_str(&raw)?;
        Ok(cfg)
    }

    pub fn find_target(&self, name: &str) -> Option<&Target> {
        self.targets.iter().find(|t| t.name == name)
    }
}

// ---------------------------------------------------------------------------
// Template generation for `klaar install --project .`
// ---------------------------------------------------------------------------

pub fn rust_template(project_name: &str) -> String {
    format!(
        r#"[project]
name = "{name}"
language = "rust"

[[targets]]
name = "production"
pre_push_checks = [
  "cargo check --workspace",
  "cargo clippy -- -D warnings",
  "cargo test --workspace"
]
deploy_method = "custom"
deploy_command = "echo 'configure deploy_command'"

[[targets]]
name = "staging"
pre_push_checks = [
  "cargo check --workspace"
]
deploy_method = "custom"
deploy_command = "echo 'configure deploy_command'"
"#,
        name = project_name
    )
}

pub fn typescript_template(project_name: &str) -> String {
    format!(
        r#"[project]
name = "{name}"
language = "typescript"

[[targets]]
name = "production"
pre_push_checks = [
  "npm run build",
  "npm run lint",
  "npm run test"
]
deploy_method = "custom"
deploy_command = "npm run deploy"

[[targets]]
name = "staging"
pre_push_checks = [
  "npm run build"
]
deploy_method = "custom"
deploy_command = "echo 'configure deploy_command'"
"#,
        name = project_name
    )
}

pub fn mixed_template(project_name: &str) -> String {
    format!(
        r#"[project]
name = "{name}"
language = "mixed"

[[targets]]
name = "production"
pre_push_checks = [
  "cargo check --workspace",
  "npm run build",
  "cargo test --workspace"
]
deploy_method = "custom"
deploy_command = "echo 'configure deploy_command'"
"#,
        name = project_name
    )
}
