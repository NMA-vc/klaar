use crate::wiki::model::{LensId, RelativeWikiPath};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WatcherPolicy {
    Disabled,
    MarkStale,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrePushPolicy {
    Disabled,
    Check,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CiPolicy {
    Disabled,
    Check,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationPolicy {
    #[serde(default = "default_watcher")]
    pub watcher: WatcherPolicy,
    #[serde(default = "default_pre_push")]
    pub pre_push: PrePushPolicy,
    #[serde(default = "default_ci")]
    pub ci: CiPolicy,
}

fn default_watcher() -> WatcherPolicy {
    WatcherPolicy::MarkStale
}

fn default_pre_push() -> PrePushPolicy {
    PrePushPolicy::Check
}

fn default_ci() -> CiPolicy {
    CiPolicy::Check
}

impl Default for AutomationPolicy {
    fn default() -> Self {
        Self {
            watcher: default_watcher(),
            pre_push: default_pre_push(),
            ci: default_ci(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiDocuments {
    #[serde(default)]
    pub trusted: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Default for WikiDocuments {
    fn default() -> Self {
        Self {
            trusted: vec!["README.md".to_string(), "docs/**/*.md".to_string()],
            exclude: vec!["**/.env*".to_string(), "**/secrets/**".to_string()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiBudgets {
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
    #[serde(default = "default_evidence_bytes")]
    pub max_evidence_bytes_per_page: usize,
    #[serde(default = "default_output_bytes")]
    pub max_output_bytes_per_page: usize,
    #[serde(default = "default_max_files")]
    pub max_input_files: usize,
}

fn default_max_pages() -> usize {
    100
}
fn default_evidence_bytes() -> usize {
    128 * 1024
}
fn default_output_bytes() -> usize {
    32 * 1024
}
fn default_max_files() -> usize {
    20_000
}

impl Default for WikiBudgets {
    fn default() -> Self {
        Self {
            max_pages: default_max_pages(),
            max_evidence_bytes_per_page: default_evidence_bytes(),
            max_output_bytes_per_page: default_output_bytes(),
            max_input_files: default_max_files(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_lenses")]
    pub lenses: Vec<LensId>,
    #[serde(default = "default_output_dir")]
    pub output_dir: RelativeWikiPath,
    #[serde(default = "default_state_dir")]
    pub state_dir: RelativeWikiPath,
    #[serde(default)]
    pub documents: WikiDocuments,
    #[serde(default)]
    pub budgets: WikiBudgets,
    #[serde(default)]
    pub automation: AutomationPolicy,
}

fn default_lenses() -> Vec<LensId> {
    vec![LensId::technical(), LensId::executive()]
}
fn default_output_dir() -> RelativeWikiPath {
    RelativeWikiPath::new(".klaar/wiki").expect("default wiki output path")
}
fn default_state_dir() -> RelativeWikiPath {
    RelativeWikiPath::new(".klaar/wiki-state").expect("default wiki state path")
}

impl Default for WikiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lenses: default_lenses(),
            output_dir: default_output_dir(),
            state_dir: default_state_dir(),
            documents: WikiDocuments::default(),
            budgets: WikiBudgets::default(),
            automation: AutomationPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WikiConfigFile {
    pub wiki: WikiConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl WikiConfig {
    pub fn from_toml_str(input: &str) -> Result<Self, ConfigError> {
        let file: WikiConfigFile = toml::from_str(input)
            .map_err(|error| ConfigError(format!("invalid .klaar/wiki.toml: {error}")))?;
        file.wiki.validate()?;
        Ok(file.wiki)
    }

    pub fn load(project_root: &Path) -> Result<Self, ConfigError> {
        let path = project_root.join(".klaar").join("wiki.toml");
        let input = std::fs::read_to_string(&path)
            .map_err(|error| ConfigError(format!("cannot read {}: {error}", path.display())))?;
        Self::from_toml_str(&input)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_wiki_dir(&self.output_dir, "output_dir")?;
        validate_wiki_dir(&self.state_dir, "state_dir")?;
        if self.output_dir == self.state_dir {
            return Err(ConfigError(
                "output_dir and state_dir must differ".to_string(),
            ));
        }
        if self.enabled && self.lenses.is_empty() {
            return Err(ConfigError(
                "enabled wiki must configure at least one lens".to_string(),
            ));
        }
        if self.lenses.iter().any(|lens| lens.as_str() == "") {
            return Err(ConfigError(
                "lens identifiers must not be empty".to_string(),
            ));
        }
        validate_globs(&self.documents.trusted, "documents.trusted")?;
        validate_globs(&self.documents.exclude, "documents.exclude")?;
        validate_budgets(&self.budgets)
    }

    /// Resolve a configured directory while keeping it below the repository's `.klaar`.
    /// Existing symlinks are checked by the caller's filesystem boundary.
    pub fn resolve_dir(
        &self,
        project_root: &Path,
        path: &RelativeWikiPath,
    ) -> Result<PathBuf, ConfigError> {
        self.validate()?;
        let klaar = project_root.join(".klaar");
        let resolved = project_root.join(path.to_path_buf());
        if !resolved.starts_with(&klaar) || resolved == klaar {
            return Err(ConfigError(format!(
                "configured path {path} is not below .klaar"
            )));
        }
        Ok(resolved)
    }
}

fn validate_wiki_dir(path: &RelativeWikiPath, field: &str) -> Result<(), ConfigError> {
    let components: Vec<_> = Path::new(path.as_str()).components().collect();
    if components.len() < 2
        || components[0] != Component::Normal(".klaar".as_ref())
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ConfigError(format!(
            "{field} must be a relative directory below .klaar"
        )));
    }
    Ok(())
}

fn validate_globs(globs: &[String], field: &str) -> Result<(), ConfigError> {
    if globs.len() > 128 {
        return Err(ConfigError(format!("{field} accepts at most 128 patterns")));
    }
    for glob in globs {
        if glob.is_empty() || glob.len() > 512 || glob.contains('\0') || glob.contains('\\') {
            return Err(ConfigError(format!("{field} contains an invalid glob")));
        }
        let path = Path::new(glob);
        if path.is_absolute()
            || path
                .components()
                .any(|component| component == Component::ParentDir)
        {
            return Err(ConfigError(format!(
                "{field} may not escape the repository"
            )));
        }
    }
    Ok(())
}

fn validate_budgets(budgets: &WikiBudgets) -> Result<(), ConfigError> {
    const MAX_PAGES: usize = 10_000;
    const MAX_EVIDENCE: usize = 16 * 1024 * 1024;
    const MAX_OUTPUT: usize = 4 * 1024 * 1024;
    const MAX_FILES: usize = 1_000_000;
    if budgets.max_pages == 0 || budgets.max_pages > MAX_PAGES {
        return Err(ConfigError(
            "max_pages is outside the safe bound".to_string(),
        ));
    }
    if budgets.max_evidence_bytes_per_page == 0
        || budgets.max_evidence_bytes_per_page > MAX_EVIDENCE
    {
        return Err(ConfigError(
            "max_evidence_bytes_per_page is outside the safe bound".to_string(),
        ));
    }
    if budgets.max_output_bytes_per_page == 0 || budgets.max_output_bytes_per_page > MAX_OUTPUT {
        return Err(ConfigError(
            "max_output_bytes_per_page is outside the safe bound".to_string(),
        ));
    }
    if budgets.max_input_files == 0 || budgets.max_input_files > MAX_FILES {
        return Err(ConfigError(
            "max_input_files is outside the safe bound".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe_and_parse() {
        let config = WikiConfig::from_toml_str("[wiki]\nenabled = true\n").unwrap();
        assert_eq!(config.output_dir.as_str(), ".klaar/wiki");
        assert!(config
            .resolve_dir(Path::new("/repo"), &config.output_dir)
            .is_ok());
    }

    #[test]
    fn config_rejects_commands_credentials_endpoints_and_escape_paths() {
        let bad = "[wiki]\nenabled = true\noutput_dir = \"/tmp/out\"\n";
        assert!(WikiConfig::from_toml_str(bad).is_err());
        let bad = "[wiki]\n[wiki.automation]\ncommand = \"rm -rf /\"\n";
        assert!(WikiConfig::from_toml_str(bad).is_err());
        let bad = "[wiki]\n[wiki.documents]\ntrusted = [\"../secret.md\"]\n";
        assert!(WikiConfig::from_toml_str(bad).is_err());
        let bad = "[wiki]\noutput_dir = \".klaar/wiki\"\nstate_dir = \".klaar/wiki\"\n";
        assert!(WikiConfig::from_toml_str(bad).is_err());
    }
}
