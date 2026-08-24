use crate::wiki::model::{Diagnostic, DiagnosticLevel, EvidenceId, PageDraft, RelativeWikiPath};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// A category is safe to expose; the matched value is intentionally absent.
    pub category: &'static str,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyError {
    PathOutsideProject,
    HardExcludedPath,
    SymlinkEscape,
    InputTooLarge,
    SecretDetected(Vec<&'static str>),
    UnsafeUrl,
    ActiveHtml,
    InvalidMarkdown(String),
    InvalidDraft(String),
}

impl std::fmt::Display for SafetyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PathOutsideProject => formatter.write_str("path is outside the project"),
            Self::HardExcludedPath => formatter.write_str("path is hard-excluded"),
            Self::SymlinkEscape => formatter.write_str("path resolves outside the project"),
            Self::InputTooLarge => formatter.write_str("input exceeds the configured limit"),
            Self::SecretDetected(categories) => {
                write!(
                    formatter,
                    "secret-like material detected ({})",
                    categories.join(", ")
                )
            }
            Self::UnsafeUrl => formatter.write_str("URL scheme is not allowed"),
            Self::ActiveHtml => formatter.write_str("active HTML is not allowed"),
            Self::InvalidMarkdown(message) => write!(formatter, "invalid Markdown: {message}"),
            Self::InvalidDraft(message) => write!(formatter, "invalid page draft: {message}"),
        }
    }
}

impl std::error::Error for SafetyError {}

/// Hard exclusions cannot be overridden by repository configuration.
pub fn is_hard_excluded(path: &Path) -> bool {
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let Some(name) = component.as_os_str().to_str() else {
            return true;
        };
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            ".git" | "node_modules" | "target" | "vendor" | "dist" | "build"
        ) {
            return true;
        }
        if lower == ".klaar" {
            let next = components
                .peek()
                .and_then(|value| value.as_os_str().to_str());
            if matches!(next, Some("wiki" | "wiki-state")) {
                return true;
            }
        }
        if lower.starts_with(".env")
            || lower.ends_with(".pem")
            || lower.ends_with(".key")
            || lower.ends_with(".p12")
            || lower.contains("credentials")
            || lower.contains("secret")
            || lower == "id_rsa"
            || lower == "id_ed25519"
        {
            return true;
        }
    }
    false
}

/// Validate an existing repository input before reading its contents.
pub fn validate_input_path(
    project_root: &Path,
    candidate: &Path,
    max_bytes: usize,
) -> Result<PathBuf, SafetyError> {
    let root = project_root
        .canonicalize()
        .map_err(|_| SafetyError::PathOutsideProject)?;
    let resolved = candidate
        .canonicalize()
        .map_err(|_| SafetyError::PathOutsideProject)?;
    if !resolved.starts_with(&root) {
        return Err(SafetyError::SymlinkEscape);
    }
    let relative = resolved
        .strip_prefix(&root)
        .map_err(|_| SafetyError::PathOutsideProject)?;
    if is_hard_excluded(relative) || !resolved.is_file() {
        return Err(SafetyError::HardExcludedPath);
    }
    let metadata = fs::metadata(&resolved).map_err(|_| SafetyError::PathOutsideProject)?;
    if metadata.len() > max_bytes as u64 {
        return Err(SafetyError::InputTooLarge);
    }
    Ok(resolved)
}

pub fn validate_output_path(
    project_root: &Path,
    relative: &RelativeWikiPath,
) -> Result<PathBuf, SafetyError> {
    let root = project_root.join(".klaar");
    let output = root.join(relative.to_path_buf());
    if !output.starts_with(&root) || output == root {
        return Err(SafetyError::PathOutsideProject);
    }
    if is_hard_excluded(relative.to_path_buf().as_path()) {
        return Err(SafetyError::HardExcludedPath);
    }
    let managed = RelativeWikiPath::new(
        relative
            .as_str()
            .split('/')
            .next()
            .map(|first| format!(".klaar/{first}"))
            .ok_or(SafetyError::PathOutsideProject)?,
    )
    .map_err(|_| SafetyError::PathOutsideProject)?;
    validate_managed_directory(project_root, &managed)?;
    let mut ancestor = output.parent();
    while let Some(candidate) = ancestor {
        if candidate.exists() {
            let resolved = candidate
                .canonicalize()
                .map_err(|_| SafetyError::PathOutsideProject)?;
            let project = project_root
                .canonicalize()
                .map_err(|_| SafetyError::PathOutsideProject)?;
            if !resolved.starts_with(project) {
                return Err(SafetyError::SymlinkEscape);
            }
            break;
        }
        ancestor = candidate.parent();
    }
    Ok(output)
}

pub fn validate_managed_directory(
    project_root: &Path,
    relative: &RelativeWikiPath,
) -> Result<PathBuf, SafetyError> {
    if !relative.as_str().starts_with(".klaar/") {
        return Err(SafetyError::PathOutsideProject);
    }
    let project = project_root
        .canonicalize()
        .map_err(|_| SafetyError::PathOutsideProject)?;
    let directory = project.join(relative.to_path_buf());
    let mut ancestor = Some(directory.as_path());
    while let Some(candidate) = ancestor {
        if candidate.exists() {
            let resolved = candidate
                .canonicalize()
                .map_err(|_| SafetyError::PathOutsideProject)?;
            if !resolved.starts_with(&project) {
                return Err(SafetyError::SymlinkEscape);
            }
            break;
        }
        ancestor = candidate.parent();
    }
    Ok(directory)
}

pub fn scan_secrets(input: &str) -> Vec<SecretFinding> {
    let mut findings = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        let categories = [
            ("private-key", "-----begin "),
            ("aws-access-key", "akia"),
            ("github-token", "ghp_"),
            ("oauth-token", "ya29."),
            ("slack-token", "xox"),
            ("secret-assignment", "secret="),
            ("token-assignment", "token="),
            ("password-assignment", "password="),
            ("api-key-assignment", "api_key="),
            ("authorization-token", "bearer "),
        ];
        for (category, marker) in categories {
            if lower.contains(marker) {
                findings.push(SecretFinding {
                    category,
                    line: index + 1,
                });
            }
        }
        if has_credential_literal_assignment(&lower) {
            findings.push(SecretFinding {
                category: "credential-literal-assignment",
                line: index + 1,
            });
        }
    }
    findings.sort_by_key(|finding| (finding.line, finding.category));
    findings.dedup_by_key(|finding| (finding.line, finding.category));
    findings
}

fn has_credential_literal_assignment(line: &str) -> bool {
    const KEYS: [&str; 6] = [
        "token",
        "password",
        "secret",
        "api_key",
        "private_key",
        "access_key",
    ];
    let assignment = line.split_once('=').or_else(|| line.split_once(':'));
    let Some((name, value)) = assignment else {
        return false;
    };
    KEYS.iter().any(|key| name.contains(key))
        && (value.contains('"') || value.contains('\''))
        && value
            .chars()
            .any(|character| character.is_ascii_alphanumeric())
}

pub fn ensure_no_secrets(input: &str) -> Result<(), SafetyError> {
    let categories: BTreeSet<_> = scan_secrets(input)
        .into_iter()
        .map(|finding| finding.category)
        .collect();
    if categories.is_empty() {
        Ok(())
    } else {
        Err(SafetyError::SecretDetected(
            categories.into_iter().collect(),
        ))
    }
}

pub fn validate_url(url: &str) -> Result<(), SafetyError> {
    if url.is_empty()
        || url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'"' || byte == b'\'')
    {
        return Err(SafetyError::UnsafeUrl);
    }
    let trimmed = url.trim();
    if trimmed.starts_with('#')
        || trimmed.starts_with('/')
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
    {
        return if trimmed.starts_with("../") {
            Err(SafetyError::UnsafeUrl)
        } else {
            Ok(())
        };
    }
    let Some(colon) = trimmed.find(':') else {
        return Ok(());
    };
    let scheme = &trimmed[..colon];
    if scheme.is_empty()
        || !scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && (byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.')))
        })
    {
        return Err(SafetyError::UnsafeUrl);
    }
    if matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    ) {
        Ok(())
    } else {
        Err(SafetyError::UnsafeUrl)
    }
}

pub fn reject_active_html(markdown: &str) -> Result<(), SafetyError> {
    let lower = markdown.to_ascii_lowercase();
    let active_markers = [
        "<script",
        "</script",
        "<iframe",
        "<object",
        "<embed",
        "<svg",
        "<form",
        "<meta",
        "onload=",
        "onclick=",
        "onerror=",
        "javascript:",
    ];
    if active_markers.iter().any(|marker| lower.contains(marker)) {
        Err(SafetyError::ActiveHtml)
    } else {
        Ok(())
    }
}

pub fn validate_markdown(markdown: &str, max_bytes: usize) -> Result<(), SafetyError> {
    if markdown.len() > max_bytes {
        return Err(SafetyError::InvalidMarkdown(
            "body exceeds output budget".to_string(),
        ));
    }
    if markdown.starts_with("---\n") || markdown.contains("\n---\n") {
        return Err(SafetyError::InvalidMarkdown(
            "model-controlled front matter is forbidden".to_string(),
        ));
    }
    reject_active_html(markdown)?;
    ensure_no_secrets(markdown)?;
    let links = regex::Regex::new(r#"\]\(\s*([^\s\)]+)"#)
        .map_err(|_| SafetyError::InvalidMarkdown("link validator unavailable".to_string()))?;
    for captures in links.captures_iter(markdown) {
        validate_url(&captures[1])?;
    }
    validate_mermaid(markdown)?;
    Ok(())
}

pub fn validate_mermaid(markdown: &str) -> Result<(), SafetyError> {
    let mut in_mermaid = false;
    let mut lines = 0usize;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if !in_mermaid && trimmed == "```mermaid" {
            in_mermaid = true;
            lines = 0;
            continue;
        }
        if in_mermaid && trimmed == "```" {
            in_mermaid = false;
            continue;
        }
        if !in_mermaid {
            continue;
        }
        lines += 1;
        let lower = trimmed.to_ascii_lowercase();
        if lines > 500
            || lower.contains("%%{")
            || lower.starts_with("click ")
            || lower.contains("javascript:")
            || lower.contains("data:")
            || lower.contains("<script")
            || lower.contains("<iframe")
            || lower.contains("href=")
        {
            return Err(SafetyError::InvalidMarkdown(
                "Mermaid content is outside the supported safe subset".to_string(),
            ));
        }
    }
    if in_mermaid {
        return Err(SafetyError::InvalidMarkdown(
            "unterminated Mermaid fence".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_page_draft(
    draft: &PageDraft,
    expected_page: &crate::wiki::model::CanonicalPagePlan,
    allowed_evidence: &BTreeSet<EvidenceId>,
    max_output_bytes: usize,
) -> Result<(), SafetyError> {
    if draft.schema_version != crate::wiki::model::WIKI_SCHEMA_VERSION {
        return Err(SafetyError::InvalidDraft(
            "unsupported schema version".to_string(),
        ));
    }
    if draft.page_id != expected_page.page_id || draft.lens.as_str().is_empty() {
        return Err(SafetyError::InvalidDraft(
            "page identity does not match the plan".to_string(),
        ));
    }
    if draft.title != expected_page.title || draft.title.len() > 512 {
        return Err(SafetyError::InvalidDraft(
            "title does not match the deterministic page plan".to_string(),
        ));
    }
    if draft.claims.len() > 500 || draft.citations.len() > 1_000 {
        return Err(SafetyError::InvalidDraft(
            "draft exceeds claim or citation budget".to_string(),
        ));
    }
    let planned: BTreeSet<_> = expected_page
        .evidence
        .iter()
        .map(|evidence| evidence.id.clone())
        .collect();
    if !draft
        .citations
        .iter()
        .all(|id| planned.contains(id) && allowed_evidence.contains(id))
    {
        return Err(SafetyError::InvalidDraft(
            "draft cites evidence outside its pack".to_string(),
        ));
    }
    for claim in &draft.claims {
        if claim.claim_id.is_empty()
            || claim.claim_id.len() > 128
            || !claim
                .claim_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            || claim.text.trim().is_empty()
            || claim.text.len() > 4_096
        {
            return Err(SafetyError::InvalidDraft(
                "claim identity or text is invalid".to_string(),
            ));
        }
        ensure_no_secrets(&claim.text)
            .map_err(|error| SafetyError::InvalidDraft(error.to_string()))?;
        reject_active_html(&claim.text)
            .map_err(|error| SafetyError::InvalidDraft(error.to_string()))?;
        if claim.evidence.is_empty()
            || !claim
                .evidence
                .iter()
                .all(|id| allowed_evidence.contains(id))
        {
            return Err(SafetyError::InvalidDraft(
                "claim is not fully supported by its evidence pack".to_string(),
            ));
        }
    }
    validate_markdown(&draft.body, max_output_bytes)
        .map_err(|error| SafetyError::InvalidDraft(error.to_string()))
}

/// Diagnostics only accept already-safe identifiers and a bounded, non-secret message.
pub fn safe_diagnostic(
    code: &str,
    level: DiagnosticLevel,
    message: &str,
    path: Option<&str>,
    evidence_id: Option<EvidenceId>,
) -> Diagnostic {
    let message = if scan_secrets(message).is_empty() {
        message
            .chars()
            .take(512)
            .filter(|character| !character.is_control() || *character == '\n')
            .collect()
    } else {
        "sensitive material omitted".to_string()
    };
    Diagnostic {
        code: code
            .chars()
            .take(64)
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
            .collect(),
        level,
        message,
        path: path
            .and_then(|value| RelativeWikiPath::new(value).ok())
            .map(|value| value.as_str().to_string()),
        evidence_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::model::*;
    use std::collections::BTreeSet;
    use tempfile::tempdir;

    #[test]
    fn hard_exclusions_and_symlink_escape_fail_closed() {
        assert!(is_hard_excluded(Path::new(".git/config")));
        assert!(is_hard_excluded(Path::new("src/.env.local")));
        assert!(is_hard_excluded(Path::new(".klaar/wiki/index.md")));
        assert!(!is_hard_excluded(Path::new("src/main.rs")));

        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("src/link.txt"),
        )
        .unwrap();
        #[cfg(unix)]
        assert_eq!(
            validate_input_path(dir.path(), &dir.path().join("src/link.txt"), 100),
            Err(SafetyError::SymlinkEscape)
        );

        #[cfg(unix)]
        {
            std::fs::create_dir(dir.path().join(".klaar")).unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join(".klaar/wiki")).unwrap();
            assert_eq!(
                validate_managed_directory(
                    dir.path(),
                    &RelativeWikiPath::new(".klaar/wiki").unwrap()
                ),
                Err(SafetyError::SymlinkEscape)
            );
        }
    }

    #[test]
    fn secret_errors_and_diagnostics_never_echo_values() {
        let secret = "token=super-secret-value";
        let error = ensure_no_secrets(secret).unwrap_err().to_string();
        assert!(!error.contains("super-secret-value"));
        let diagnostic = safe_diagnostic("secret", DiagnosticLevel::Error, secret, None, None);
        assert!(!diagnostic.message.contains("super-secret-value"));
        assert!(!scan_secrets("const TEST_TOKEN: &str = \"do-not-print\";").is_empty());
        assert!(!scan_secrets("{\"password\": \"do-not-print\"}").is_empty());
        assert!(scan_secrets("let token = load_from_environment();").is_empty());
    }

    #[test]
    fn active_content_and_urls_are_rejected() {
        assert!(validate_markdown("<script>alert(1)</script>", 100).is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
        assert!(validate_url("data:text/html,evil").is_err());
        assert!(validate_url("https://example.com/docs").is_ok());
        assert!(validate_url("docs/page.md").is_ok());
        assert!(validate_url("../outside.md").is_err());
        assert!(validate_markdown("[bad](javascript:alert(1))", 100).is_err());
        assert!(validate_markdown("```mermaid\nclick A javascript:evil\n```", 100).is_err());
    }

    #[test]
    fn draft_may_only_cite_planned_evidence() {
        let page_id = PageId::new("architecture").unwrap();
        let evidence_id = EvidenceId::new("src-main").unwrap();
        let plan = CanonicalPagePlan {
            schema_version: WIKI_SCHEMA_VERSION,
            page_id: page_id.clone(),
            page_kind: PageKind::Architecture,
            title: "Architecture".to_string(),
            evidence: vec![EvidenceRef {
                id: evidence_id.clone(),
                source: SourceIdentity::new("src/main.rs", SourceKind::Source).unwrap(),
                range: None,
                content_hash: "hash".to_string(),
                evidence_kind: EvidenceKind::SourceExcerpt,
            }],
            related_pages: Vec::new(),
            output_stem: RelativeWikiPath::new("architecture").unwrap(),
        };
        let draft = PageDraft {
            schema_version: WIKI_SCHEMA_VERSION,
            page_id,
            lens: LensId::technical(),
            title: "Architecture".to_string(),
            body: "A documented component.".to_string(),
            claims: vec![SupportedClaim {
                claim_id: "claim-1".to_string(),
                kind: ClaimKind::Fact,
                text: "A documented component.".to_string(),
                evidence: vec![evidence_id.clone()],
            }],
            citations: vec![evidence_id.clone()],
        };
        let mut allowed = BTreeSet::new();
        allowed.insert(evidence_id);
        assert!(validate_page_draft(&draft, &plan, &allowed, 100).is_ok());
    }
}
