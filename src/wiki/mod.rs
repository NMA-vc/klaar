//! Versioned, local-first contracts for the compiled repository wiki.
//!
//! This module deliberately contains no provider, network, CLI, or filesystem
//! scanning code.  It is the deterministic control-plane boundary used by
//! later wiki builders and adapters.

pub mod build;
pub mod check;
pub mod compiler;
pub mod config;
pub mod evaluate;
pub mod evidence;
pub mod generator;
pub mod git;
pub mod model;
pub mod planner;
pub mod process;
pub mod realm;
pub mod safety;
pub mod state;
pub mod story;
pub mod update;
pub mod viewer;

pub use config::{AutomationPolicy, WikiConfig, WikiConfigFile};
pub use model::{
    CanonicalPagePlan, CanonicalPageState, ContentHash, Diagnostic, DiagnosticLevel, EvidenceId,
    EvidenceKind, EvidenceRef, GeneratorReceipt, LensId, LensRenderState, MigrationStatus,
    PageDraft, PageId, PageKind, RelativeWikiPath, SchemaVersion, SourceIdentity, SourceRange,
    StaleReason, SupportedClaim, WikiManifestV1, WIKI_SCHEMA_VERSION,
};
pub use safety::{
    ensure_no_secrets, validate_managed_directory, validate_markdown, validate_mermaid,
    validate_page_draft, validate_url,
};
pub use state::{
    atomic_write, recover_staging, AtomicStaging, CleanupReport, ProvenanceState, StateError,
};
