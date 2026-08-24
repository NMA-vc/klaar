use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::path::{Component, Path, PathBuf};

pub const WIKI_SCHEMA_VERSION: u32 = 1;
pub type SchemaVersion = u32;
pub type ContentHash = String;

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
        && value.as_bytes()[0].is_ascii_alphanumeric()
}

macro_rules! identifier_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if valid_identifier(&value) {
                    Ok(Self(value))
                } else {
                    Err(format!("invalid {}", stringify!($name)))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = String;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

identifier_type!(PageId);
identifier_type!(LensId);
identifier_type!(EvidenceId);

impl LensId {
    pub fn technical() -> Self {
        Self("technical".to_string())
    }

    pub fn executive() -> Self {
        Self("executive".to_string())
    }
}

/// A normalized path relative to `.klaar`, never an absolute or traversing path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativeWikiPath(String);

impl RelativeWikiPath {
    pub fn new(value: impl AsRef<str>) -> Result<Self, String> {
        let raw = value.as_ref();
        if raw.is_empty() || raw.contains('\0') || raw.contains('\\') {
            return Err("wiki path must be a non-empty slash-separated path".to_string());
        }
        let path = Path::new(raw);
        if path.is_absolute() {
            return Err("wiki path must be relative".to_string());
        }

        let mut components = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => {
                    let part = part
                        .to_str()
                        .ok_or_else(|| "wiki path is not UTF-8".to_string())?;
                    if part.is_empty() {
                        return Err("wiki path contains an empty component".to_string());
                    }
                    components.push(part.to_string());
                }
                Component::CurDir => {}
                Component::ParentDir => return Err("wiki path may not contain '..'".to_string()),
                Component::RootDir | Component::Prefix(_) => {
                    return Err("wiki path must be relative".to_string())
                }
            }
        }
        if components.is_empty() {
            return Err("wiki path must name a file or directory".to_string());
        }
        Ok(Self(components.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn join(&self, suffix: &str) -> Result<Self, String> {
        Self::new(format!("{}/{}", self.0, suffix))
    }

    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(&self.0)
    }
}

impl TryFrom<String> for RelativeWikiPath {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RelativeWikiPath> for String {
    fn from(value: RelativeWikiPath) -> Self {
        value.0
    }
}

impl Serialize for RelativeWikiPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RelativeWikiPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl fmt::Display for RelativeWikiPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Source,
    TrustedDocument,
    Manifest,
    Symbol,
    Dependency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    /// Repository-relative path using `/` separators.
    pub path: String,
    pub kind: SourceKind,
    pub stable_id: Option<String>,
}

impl SourceIdentity {
    pub fn new(path: impl AsRef<str>, kind: SourceKind) -> Result<Self, String> {
        let path = RelativeWikiPath::new(path.as_ref())?;
        Ok(Self {
            path: path.as_str().to_string(),
            kind,
            stable_id: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceKind {
    SourceExcerpt,
    Symbol,
    Dependency,
    Manifest,
    TrustedDocument,
    Diagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub id: EvidenceId,
    pub source: SourceIdentity,
    pub range: Option<SourceRange>,
    pub content_hash: ContentHash,
    pub evidence_kind: EvidenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageKind {
    Index,
    Architecture,
    Dependencies,
    Testing,
    Operations,
    Domain,
    Component,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimKind {
    Fact,
    Relationship,
    Limitation,
    Risk,
    Purpose,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportedClaim {
    pub claim_id: String,
    pub kind: ClaimKind,
    pub text: String,
    pub evidence: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalPagePlan {
    pub schema_version: SchemaVersion,
    pub page_id: PageId,
    pub page_kind: PageKind,
    pub title: String,
    pub evidence: Vec<EvidenceRef>,
    pub related_pages: Vec<PageId>,
    pub output_stem: RelativeWikiPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalPageState {
    pub schema_version: SchemaVersion,
    pub page_id: PageId,
    pub page_kind: PageKind,
    pub title: String,
    pub evidence: Vec<EvidenceRef>,
    pub related_pages: Vec<PageId>,
    pub evidence_hash: ContentHash,
    pub structured_claims: Vec<SupportedClaim>,
    pub content_hash: Option<ContentHash>,
    pub stale_reasons: Vec<StaleReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratorReceipt {
    pub schema_version: SchemaVersion,
    pub adapter: String,
    pub generator_version: String,
    pub input_hash: ContentHash,
    pub output_hash: ContentHash,
    pub deterministic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensRenderState {
    pub schema_version: SchemaVersion,
    pub page_id: PageId,
    pub lens: LensId,
    pub evidence_hash: ContentHash,
    pub content_hash: ContentHash,
    pub generator: GeneratorReceipt,
    pub output_path: RelativeWikiPath,
    pub stale_reasons: Vec<StaleReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageDraft {
    pub schema_version: SchemaVersion,
    pub page_id: PageId,
    pub lens: LensId,
    pub title: String,
    pub body: String,
    pub claims: Vec<SupportedClaim>,
    pub citations: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StaleReason {
    SourceChanged { path: String },
    SourceDeleted { path: String },
    SourceRenamed { from: String, to: String },
    EvidenceChanged { evidence_id: EvidenceId },
    ConfigurationChanged,
    GeneratorChanged,
    StateMissing,
    SchemaIncompatible,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub level: DiagnosticLevel,
    pub message: String,
    pub path: Option<String>,
    pub evidence_id: Option<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MigrationStatus {
    #[default]
    Current,
    RequiresRebuild,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiManifestV1 {
    pub schema_version: SchemaVersion,
    pub klaar_version: String,
    pub repository_id: String,
    pub lenses: Vec<LensId>,
    pub canonical_pages: Vec<CanonicalPageState>,
    pub lens_renders: Vec<LensRenderState>,
    pub provenance: crate::wiki::state::ProvenanceState,
    pub source_hashes: std::collections::BTreeMap<String, ContentHash>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub migration_status: MigrationStatus,
}

impl WikiManifestV1 {
    pub fn empty(repository_id: impl Into<String>, klaar_version: impl Into<String>) -> Self {
        Self {
            schema_version: WIKI_SCHEMA_VERSION,
            klaar_version: klaar_version.into(),
            repository_id: repository_id.into(),
            lenses: Vec::new(),
            canonical_pages: Vec::new(),
            lens_renders: Vec::new(),
            provenance: crate::wiki::state::ProvenanceState::default(),
            source_hashes: std::collections::BTreeMap::new(),
            diagnostics: Vec::new(),
            migration_status: MigrationStatus::Current,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_paths_fail_closed() {
        assert!(PageId::new("architecture").is_ok());
        assert!(PageId::new("../secret").is_err());
        assert!(RelativeWikiPath::new("../outside.md").is_err());
        assert!(RelativeWikiPath::new("/tmp/wiki.md").is_err());
        assert!(RelativeWikiPath::new("./technical/../index.md").is_err());
        assert_eq!(
            RelativeWikiPath::new("technical/index.md")
                .unwrap()
                .as_str(),
            "technical/index.md"
        );
    }

    #[test]
    fn path_serialization_is_normalized() {
        let path = RelativeWikiPath::new("technical\\index.md");
        assert!(path.is_err());
        let path = RelativeWikiPath::new("technical/./index.md").unwrap();
        assert_eq!(path.as_str(), "technical/index.md");
    }
}
