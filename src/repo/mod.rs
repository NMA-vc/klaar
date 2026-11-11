//! Bounded, deterministic repository facts for indexing, diagrams, and wiki provenance.
//!
//! This module intentionally contains no model or filesystem-write behavior. It turns a
//! repository into stable, source-backed facts that downstream features can render.

use crate::parser::{extract_symbols, Lang};
use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

pub const FACTS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_FILES: usize = 2_000;
pub const DEFAULT_MAX_EDGES: usize = 10_000;
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_048_576;

const MAX_AUX_FILES: usize = 512;
const MAX_DIAGNOSTICS: usize = 512;
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".klaar",
    ".next",
    ".turbo",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "generated",
    "gen",
    "node_modules",
    "out",
    "target",
    "vendor",
];

/// Skipped directories worth a diagnostic: they usually hold *tracked* source.
/// The rest of [`SKIP_DIRS`] is VCS or tool state (`.git`, `target`, `node_modules`,
/// ...) that is present or absent depending on the checkout, so reporting it would
/// make a facts snapshot differ between a clean clone and a working directory.
const DIAGNOSED_SKIP_DIRS: &[&str] = &["gen", "generated", "vendor"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FileLanguage {
    Rust,
    TypeScript,
    TypeScriptReact,
    JavaScript,
    JavaScriptReact,
    Python,
    Go,
}

impl FileLanguage {
    fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("rs") => Some(Self::Rust),
            Some("ts") => Some(Self::TypeScript),
            Some("tsx") => Some(Self::TypeScriptReact),
            Some("js") => Some(Self::JavaScript),
            Some("jsx") => Some(Self::JavaScriptReact),
            Some("py") => Some(Self::Python),
            Some("go") => Some(Self::Go),
            _ => None,
        }
    }

    fn parser_lang(self) -> Option<Lang> {
        match self {
            Self::Rust => Some(Lang::Rust),
            Self::TypeScript | Self::TypeScriptReact => Some(Lang::TypeScript),
            Self::Python => Some(Lang::Python),
            Self::Go => Some(Lang::Go),
            Self::JavaScript | Self::JavaScriptReact => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FileKind {
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvidenceKind {
    File,
    Symbol,
    Dependency,
    Manifest,
    TrustedDocument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceRange {
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub id: String,
    pub source: String,
    pub range: Option<SourceRange>,
    pub content_hash: String,
    pub evidence_kind: EvidenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFact {
    pub id: String,
    pub path: String,
    pub language: FileLanguage,
    pub kind: FileKind,
    pub bytes: u64,
    pub content_hash: String,
    pub evidence: EvidenceRef,
    pub symbol_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolFact {
    pub id: String,
    pub file_id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub range: SourceRange,
    pub evidence: EvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub id: String,
    pub from_file_id: String,
    pub to_file_id: String,
    pub kind: String,
    pub evidence: EvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFact {
    pub id: String,
    pub path: String,
    pub kind: String,
    pub content_hash: String,
    pub commands: Vec<String>,
    pub evidence: EvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedDocFact {
    pub id: String,
    pub path: String,
    pub title: Option<String>,
    pub content_hash: String,
    pub evidence: EvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentCandidate {
    pub id: String,
    pub name: String,
    pub file_ids: Vec<String>,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanDiagnostic {
    pub code: String,
    pub path: Option<String>,
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryFacts {
    pub schema_version: u32,
    pub repository_id: String,
    pub files: Vec<FileFact>,
    pub symbols: Vec<SymbolFact>,
    pub dependencies: Vec<DependencyEdge>,
    pub manifests: Vec<ManifestFact>,
    pub trusted_docs: Vec<TrustedDocFact>,
    pub components: Vec<ComponentCandidate>,
    pub diagnostics: Vec<ScanDiagnostic>,
}

impl Default for RepositoryFacts {
    fn default() -> Self {
        Self {
            schema_version: FACTS_SCHEMA_VERSION,
            repository_id: String::new(),
            files: Vec::new(),
            symbols: Vec::new(),
            dependencies: Vec::new(),
            manifests: Vec::new(),
            trusted_docs: Vec::new(),
            components: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    pub max_files: usize,
    pub max_edges: usize,
    pub max_file_bytes: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_MAX_FILES,
            max_edges: DEFAULT_MAX_EDGES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }
}

impl RepositoryFacts {
    pub fn scan(root: &Path) -> Result<Self> {
        Self::scan_with_options(root, ScanOptions::default())
    }

    pub fn scan_with_options(root: &Path, options: ScanOptions) -> Result<Self> {
        scan_repository(root, options)
    }

    pub fn scan_with_document_policy(
        root: &Path,
        options: ScanOptions,
        trusted_documents: &[String],
        excluded_paths: &[String],
    ) -> Result<Self> {
        scan_repository_inner(root, options, Some(trusted_documents), excluded_paths)
    }

    /// Serialize a fully sorted facts snapshot for durable wiki state.
    pub fn to_stable_json(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

#[derive(Debug)]
struct Candidate {
    absolute: PathBuf,
    relative: String,
    bytes: u64,
}

#[derive(Debug)]
struct SourceRecord {
    absolute: PathBuf,
    relative: String,
    language: FileLanguage,
    content: String,
    bytes: u64,
    content_hash: String,
    file_id: String,
}

#[derive(Debug)]
struct InferredDependency {
    target: usize,
    line: usize,
    kind: &'static str,
}

pub fn scan_repository(root: &Path, options: ScanOptions) -> Result<RepositoryFacts> {
    scan_repository_inner(root, options, None, &[])
}

fn scan_repository_inner(
    root: &Path,
    options: ScanOptions,
    trusted_documents: Option<&[String]>,
    excluded_paths: &[String],
) -> Result<RepositoryFacts> {
    let root = root
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize repository path: {}", root.display()))?;
    if !root.is_dir() {
        anyhow::bail!("Repository path is not a directory: {}", root.display());
    }

    let trusted_patterns = trusted_documents
        .map(compile_globs)
        .transpose()
        .context("invalid trusted-document glob")?;
    let excluded_patterns = compile_globs(excluded_paths).context("invalid exclude glob")?;
    let mut diagnostics = Vec::new();
    let mut source_candidates = Vec::with_capacity(options.max_files.saturating_add(1));
    let mut aux_candidates = Vec::with_capacity(MAX_AUX_FILES + 1);
    // Entries the walk pruned before the loop sees them. Recorded so a skipped
    // directory or symlink is a diagnostic, not silence a reader mistakes for "no code here".
    let pruned: std::cell::RefCell<Vec<(&'static str, String)>> = Default::default();

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.file_type().is_symlink() {
                pruned
                    .borrow_mut()
                    .push(("skipped_symlink", normalized_relative(&root, entry.path())));
                return false;
            }
            let keep = !entry.file_type().is_dir()
                || entry
                    .file_name()
                    .to_str()
                    .is_none_or(|name| !SKIP_DIRS.contains(&name));
            if !keep
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| DIAGNOSED_SKIP_DIRS.contains(&name))
            {
                pruned.borrow_mut().push((
                    "skipped_directory",
                    normalized_relative(&root, entry.path()),
                ));
            }
            keep
        })
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                push_diagnostic(
                    &mut diagnostics,
                    ScanDiagnostic {
                        code: "walk_error".to_string(),
                        path: None,
                        message: error.to_string(),
                        severity: DiagnosticSeverity::Warning,
                        truncated: false,
                    },
                );
                continue;
            }
        };
        if entry.file_type().is_symlink() || !entry.file_type().is_file() {
            continue;
        }

        let relative = normalized_relative(&root, entry.path());
        if hard_excluded_path(&relative) {
            // Silent on purpose: even a count of secret-looking files is local state.
            continue;
        }
        if excluded_patterns
            .iter()
            .any(|pattern| pattern.is_match(&relative))
        {
            push_diagnostic(
                &mut diagnostics,
                diagnostic_info(
                    "excluded_path",
                    Some(relative),
                    "Excluded by an exclude pattern",
                ),
            );
            continue;
        }
        let Some(bytes) = entry.metadata().ok().map(|metadata| metadata.len()) else {
            push_diagnostic(
                &mut diagnostics,
                diagnostic(
                    "metadata_error",
                    Some(relative),
                    "Could not read file metadata",
                ),
            );
            continue;
        };

        if let Some(language) = FileLanguage::from_path(entry.path()) {
            if source_candidates.len() < options.max_files.saturating_add(1) {
                source_candidates.push(Candidate {
                    absolute: entry.path().to_path_buf(),
                    relative,
                    bytes,
                });
            }
            let _ = language;
        } else if is_manifest(entry.path())
            || trusted_patterns.as_ref().map_or_else(
                || is_trusted_doc(&relative),
                |patterns| patterns.iter().any(|pattern| pattern.is_match(&relative)),
            )
        {
            if aux_candidates.len() < MAX_AUX_FILES + 1 {
                aux_candidates.push(Candidate {
                    absolute: entry.path().to_path_buf(),
                    relative,
                    bytes,
                });
            }
        } else if is_known_unsupported_source(entry.path()) {
            push_diagnostic(
                &mut diagnostics,
                ScanDiagnostic {
                    code: "unsupported_language".to_string(),
                    path: Some(relative),
                    message: "Source file was not parsed because this language is unsupported"
                        .to_string(),
                    severity: DiagnosticSeverity::Info,
                    truncated: false,
                },
            );
        }
    }

    for (code, path) in pruned.into_inner() {
        let message = if code == "skipped_symlink" {
            "Symlink not followed"
        } else {
            "Directory not scanned (built-in skip list)"
        };
        push_diagnostic(&mut diagnostics, diagnostic_info(code, Some(path), message));
    }
    source_candidates.sort_by(|a, b| a.relative.cmp(&b.relative));
    let source_truncated = source_candidates.len() > options.max_files;
    if source_truncated {
        push_diagnostic(
            &mut diagnostics,
            ScanDiagnostic {
                code: "max_files".to_string(),
                path: None,
                message: format!("Source scan truncated at {} files", options.max_files),
                severity: DiagnosticSeverity::Warning,
                truncated: true,
            },
        );
        source_candidates.truncate(options.max_files);
    }

    let mut sources = Vec::with_capacity(source_candidates.len());
    for candidate in source_candidates {
        let Some(language) = FileLanguage::from_path(&candidate.absolute) else {
            continue;
        };
        if candidate.bytes > options.max_file_bytes {
            push_diagnostic(
                &mut diagnostics,
                ScanDiagnostic {
                    code: "max_file_bytes".to_string(),
                    path: Some(candidate.relative),
                    message: format!(
                        "File exceeds the {} byte scan limit",
                        options.max_file_bytes
                    ),
                    severity: DiagnosticSeverity::Warning,
                    truncated: true,
                },
            );
            continue;
        }
        let bytes = match std::fs::read(&candidate.absolute) {
            Ok(bytes) => bytes,
            Err(error) => {
                push_diagnostic(
                    &mut diagnostics,
                    diagnostic("read_error", Some(candidate.relative), error.to_string()),
                );
                continue;
            }
        };
        let content = match String::from_utf8(bytes.clone()) {
            Ok(content) => content,
            Err(_) => {
                push_diagnostic(
                    &mut diagnostics,
                    diagnostic(
                        "invalid_utf8",
                        Some(candidate.relative),
                        "Source file is not valid UTF-8",
                    ),
                );
                continue;
            }
        };
        let file_id = stable_id("file", &candidate.relative);
        sources.push(SourceRecord {
            absolute: candidate.absolute,
            relative: candidate.relative,
            language,
            content,
            bytes: bytes.len() as u64,
            content_hash: content_hash(&bytes),
            file_id,
        });
    }
    sources.sort_by(|a, b| a.relative.cmp(&b.relative));

    let mut facts = RepositoryFacts {
        schema_version: FACTS_SCHEMA_VERSION,
        repository_id: repository_id(&root),
        ..RepositoryFacts::default()
    };

    let mut source_index = HashMap::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        source_index.insert(source.file_id.clone(), index);
        facts.files.push(FileFact {
            id: source.file_id.clone(),
            path: source.relative.clone(),
            language: source.language,
            kind: FileKind::Source,
            bytes: source.bytes,
            content_hash: source.content_hash.clone(),
            evidence: file_evidence(source),
            symbol_ids: Vec::new(),
        });
    }

    for (source_index_value, source) in sources.iter().enumerate() {
        if let Some(lang) = source.language.parser_lang() {
            for (ordinal, symbol) in extract_symbols(&source.content, lang)
                .into_iter()
                .enumerate()
            {
                let range = SourceRange {
                    start_line: symbol.line,
                    start_column: 1,
                    end_line: symbol.line,
                    end_column: 1,
                };
                let qualified_name = format!("{}::{}", source.relative, symbol.name);
                let id_seed = format!(
                    "{}\0{}\0{}\0{}",
                    source.relative, symbol.name, symbol.kind, ordinal
                );
                let id = stable_id("symbol", &id_seed);
                facts.files[source_index_value].symbol_ids.push(id.clone());
                facts.symbols.push(SymbolFact {
                    id,
                    file_id: source.file_id.clone(),
                    name: symbol.name,
                    qualified_name,
                    kind: symbol.kind,
                    range: range.clone(),
                    evidence: source_evidence(source, Some(range), EvidenceKind::Symbol, "symbol"),
                });
            }
        }
    }

    let rust_modules = build_rust_module_map(&sources);
    let python_modules = build_python_module_map(&sources);
    let canonical_to_index: HashMap<PathBuf, usize> = sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| source.absolute.canonicalize().ok().map(|p| (p, index)))
        .collect();
    let mut edge_count = 0usize;
    let mut edge_truncated = false;
    let mut accepted_edges = HashSet::new();
    for (from, source) in sources.iter().enumerate() {
        let mut inferred =
            infer_dependencies(source, &canonical_to_index, &rust_modules, &python_modules);
        inferred.sort_by_key(|edge| (edge.target, edge.line, edge.kind));
        for edge in inferred {
            if from == edge.target {
                continue;
            }
            let key = (from, edge.target);
            if accepted_edges.contains(&key) {
                continue;
            }
            if edge_count >= options.max_edges {
                edge_truncated = true;
                break;
            }
            accepted_edges.insert(key);
            edge_count += 1;
            let target = &sources[edge.target];
            let seed = format!("{}\0{}\0{}", source.file_id, target.file_id, edge.kind);
            facts.dependencies.push(DependencyEdge {
                id: stable_id("dependency", &seed),
                from_file_id: source.file_id.clone(),
                to_file_id: target.file_id.clone(),
                kind: edge.kind.to_string(),
                evidence: source_evidence(
                    source,
                    Some(line_range(edge.line)),
                    EvidenceKind::Dependency,
                    "dependency",
                ),
            });
        }
        if edge_truncated {
            break;
        }
    }
    if edge_truncated {
        push_diagnostic(
            &mut diagnostics,
            ScanDiagnostic {
                code: "max_edges".to_string(),
                path: None,
                message: format!("Dependency scan truncated at {} edges", options.max_edges),
                severity: DiagnosticSeverity::Warning,
                truncated: true,
            },
        );
    }

    add_auxiliary_facts(
        &mut facts,
        &mut diagnostics,
        aux_candidates,
        options.max_file_bytes,
    );
    facts.components = build_components(&facts.files);
    facts.symbols.sort_by(|a, b| {
        (&a.file_id, a.range.start_line, &a.name, &a.kind, &a.id).cmp(&(
            &b.file_id,
            b.range.start_line,
            &b.name,
            &b.kind,
            &b.id,
        ))
    });
    facts.dependencies.sort_by(|a, b| {
        (&a.from_file_id, &a.to_file_id, &a.kind, &a.id).cmp(&(
            &b.from_file_id,
            &b.to_file_id,
            &b.kind,
            &b.id,
        ))
    });
    facts.diagnostics = diagnostics;
    facts
        .diagnostics
        .sort_by(|a, b| (&a.code, &a.path, &a.message).cmp(&(&b.code, &b.path, &b.message)));
    facts.files.sort_by(|a, b| a.path.cmp(&b.path));
    facts.manifests.sort_by(|a, b| a.path.cmp(&b.path));
    facts.trusted_docs.sort_by(|a, b| a.path.cmp(&b.path));
    let _ = source_index;
    Ok(facts)
}

fn add_auxiliary_facts(
    facts: &mut RepositoryFacts,
    diagnostics: &mut Vec<ScanDiagnostic>,
    mut candidates: Vec<Candidate>,
    max_file_bytes: u64,
) {
    candidates.sort_by(|a, b| a.relative.cmp(&b.relative));
    if candidates.len() > MAX_AUX_FILES {
        push_diagnostic(
            diagnostics,
            ScanDiagnostic {
                code: "max_aux_files".to_string(),
                path: None,
                message: format!(
                    "Manifest/document scan truncated at {} files",
                    MAX_AUX_FILES
                ),
                severity: DiagnosticSeverity::Warning,
                truncated: true,
            },
        );
        candidates.truncate(MAX_AUX_FILES);
    }
    for candidate in candidates {
        if candidate.bytes > max_file_bytes {
            push_diagnostic(
                diagnostics,
                ScanDiagnostic {
                    code: "max_file_bytes".to_string(),
                    path: Some(candidate.relative),
                    message: format!("File exceeds the {} byte scan limit", max_file_bytes),
                    severity: DiagnosticSeverity::Warning,
                    truncated: true,
                },
            );
            continue;
        }
        let bytes = match std::fs::read(&candidate.absolute) {
            Ok(bytes) => bytes,
            Err(error) => {
                push_diagnostic(
                    diagnostics,
                    diagnostic("read_error", Some(candidate.relative), error.to_string()),
                );
                continue;
            }
        };
        let Ok(content) = String::from_utf8(bytes.clone()) else {
            push_diagnostic(
                diagnostics,
                diagnostic(
                    "invalid_utf8",
                    Some(candidate.relative),
                    "Document or manifest is not valid UTF-8",
                ),
            );
            continue;
        };
        let hash = content_hash(&bytes);
        let evidence_kind = if is_manifest(&candidate.absolute) {
            EvidenceKind::Manifest
        } else {
            EvidenceKind::TrustedDocument
        };
        if is_manifest(&candidate.absolute) {
            let path = candidate.relative;
            facts.manifests.push(ManifestFact {
                id: stable_id("manifest", &path),
                path: path.clone(),
                kind: manifest_kind(&path).to_string(),
                content_hash: hash.clone(),
                commands: manifest_commands(&path, &content),
                evidence: source_evidence_from_values(
                    &path,
                    &hash,
                    None,
                    evidence_kind,
                    "manifest",
                ),
            });
        } else {
            let path = candidate.relative;
            facts.trusted_docs.push(TrustedDocFact {
                id: stable_id("trusted-doc", &path),
                path: path.clone(),
                title: markdown_title(&content),
                content_hash: hash.clone(),
                evidence: source_evidence_from_values(
                    &path,
                    &hash,
                    None,
                    evidence_kind,
                    "trusted-doc",
                ),
            });
        }
    }
}

fn build_components(files: &[FileFact]) -> Vec<ComponentCandidate> {
    let mut groups: BTreeMap<String, Vec<&FileFact>> = BTreeMap::new();
    for file in files {
        groups
            .entry(architecture_group(&file.path))
            .or_default()
            .push(file);
    }
    groups
        .into_iter()
        .map(|(name, group)| {
            let mut file_ids: Vec<String> = group.iter().map(|file| file.id.clone()).collect();
            let mut paths: Vec<String> = group.iter().map(|file| file.path.clone()).collect();
            file_ids.sort();
            paths.sort();
            ComponentCandidate {
                id: stable_id("component", &name),
                name,
                file_ids,
                paths,
            }
        })
        .collect()
}

fn architecture_group(relative: &str) -> String {
    let parts: Vec<&str> = relative.split('/').collect();
    match parts.as_slice() {
        ["src" | "app" | "lib" | "packages" | "pkg", second, ..] => {
            format!("{}/{}", parts[0], second)
        }
        [first, ..] => (*first).to_string(),
        [] => "project".to_string(),
    }
}

fn file_evidence(source: &SourceRecord) -> EvidenceRef {
    source_evidence_from_values(
        &source.relative,
        &source.content_hash,
        None,
        EvidenceKind::File,
        "file",
    )
}

fn source_evidence(
    source: &SourceRecord,
    range: Option<SourceRange>,
    kind: EvidenceKind,
    id_prefix: &str,
) -> EvidenceRef {
    source_evidence_from_values(
        &source.relative,
        &source.content_hash,
        range,
        kind,
        id_prefix,
    )
}

fn source_evidence_from_values(
    source: &str,
    hash: &str,
    range: Option<SourceRange>,
    evidence_kind: EvidenceKind,
    id_prefix: &str,
) -> EvidenceRef {
    let range_key = range
        .as_ref()
        .map(|value| {
            format!(
                "{}:{}-{}:{}",
                value.start_line, value.start_column, value.end_line, value.end_column
            )
        })
        .unwrap_or_default();
    EvidenceRef {
        id: stable_id(
            "evidence",
            &format!("{}\0{}\0{}\0{}", id_prefix, source, range_key, hash),
        ),
        source: source.to_string(),
        range,
        content_hash: hash.to_string(),
        evidence_kind,
    }
}

fn line_range(line: usize) -> SourceRange {
    SourceRange {
        start_line: line,
        start_column: 1,
        end_line: line,
        end_column: 1,
    }
}

fn repository_id(root: &Path) -> String {
    let name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repository");
    stable_id("repository", name)
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{}-{}", prefix, content_hash(value.as_bytes()))
}

/// Deterministic SHA-256 implementation used for portable provenance identities.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = bytes.to_vec();
    let bit_len = (message.len() as u64) * 8;
    message.push(0x80);
    while !(message.len() + 8).is_multiple_of(64) {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 64];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let mut working = state;
        for index in 0..64 {
            let choice = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let sigma1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let sigma0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let temp1 = working[7]
                .wrapping_add(sigma1)
                .wrapping_add(choice)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(words[index]);
            let temp2 = sigma0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for index in 0..8 {
            state[index] = state[index].wrapping_add(working[index]);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn infer_dependencies(
    file: &SourceRecord,
    canonical_to_index: &HashMap<PathBuf, usize>,
    rust_modules: &HashMap<String, usize>,
    python_modules: &HashMap<String, usize>,
) -> Vec<InferredDependency> {
    match file.language {
        FileLanguage::Rust => infer_rust_dependencies(file, rust_modules),
        FileLanguage::TypeScript
        | FileLanguage::TypeScriptReact
        | FileLanguage::JavaScript
        | FileLanguage::JavaScriptReact => infer_relative_dependencies(file, canonical_to_index),
        FileLanguage::Python => infer_python_dependencies(file, python_modules),
        FileLanguage::Go => infer_go_dependencies(file, canonical_to_index),
    }
}

/// Split a repository-relative Rust path at its first `src` component:
/// `crates/x/src/a/b.rs` is crate root `crates/x`, module path `a/b.rs`; a top-level
/// `src/a.rs` has the empty root. `None` when there is no `src` component.
fn rust_crate_split(relative: &str) -> Option<(&str, &str)> {
    if let Some(rest) = relative.strip_prefix("src/") {
        return Some(("", rest));
    }
    let at = relative.find("/src/")?;
    Some((&relative[..at], &relative[at + "/src/".len()..]))
}

/// Modules are keyed by crate root, so two crates of a workspace never see each
/// other's `crate::` paths, and a workspace scanned from its root still resolves.
fn rust_module_key(root: &str, module: &str) -> String {
    format!("{root}\0{module}")
}

fn build_rust_module_map(files: &[SourceRecord]) -> HashMap<String, usize> {
    files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| {
            if file.language != FileLanguage::Rust {
                return None;
            }
            let (root, rest) = rust_crate_split(&file.relative)?;
            let mut module = rest.trim_end_matches(".rs");
            module = module.strip_suffix("/mod").unwrap_or(module);
            if matches!(module, "lib" | "main") {
                module = "";
            }
            Some((rust_module_key(root, &module.replace('/', "::")), index))
        })
        .collect()
}

fn build_python_module_map(files: &[SourceRecord]) -> HashMap<String, usize> {
    files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.language == FileLanguage::Python)
        .map(|(index, file)| {
            let mut module = file.relative.trim_end_matches(".py").replace('/', ".");
            module = module
                .strip_suffix(".__init__")
                .unwrap_or(&module)
                .to_string();
            (module, index)
        })
        .collect()
}

fn infer_rust_dependencies(
    file: &SourceRecord,
    modules: &HashMap<String, usize>,
) -> Vec<InferredDependency> {
    let mut dependencies = BTreeMap::new();
    let (root, rest) = rust_crate_split(&file.relative).unwrap_or(("", &file.relative));
    let current_module = rest
        .trim_end_matches(".rs")
        .trim_end_matches("/mod")
        .replace('/', "::");
    let module_parent = if file.relative.ends_with("/mod.rs") {
        current_module.as_str()
    } else if matches!(
        file.absolute.file_name().and_then(|name| name.to_str()),
        Some("lib.rs" | "main.rs")
    ) {
        ""
    } else {
        current_module
            .rsplit_once("::")
            .map_or("", |(parent, _)| parent)
    };
    for (line_number, line) in file.content.lines().enumerate() {
        let line_number = line_number + 1;
        let line = line.trim();
        let use_path = line
            .strip_prefix("use ")
            .or_else(|| line.strip_prefix("pub use "));
        if let Some(use_path) = use_path {
            let raw = use_path
                .split(';')
                .next()
                .unwrap_or(use_path)
                .split('{')
                .next()
                .unwrap_or(use_path)
                .trim_end_matches(':');
            let candidate = if let Some(rest) = raw.strip_prefix("crate::") {
                rest.to_string()
            } else if let Some(rest) = raw.strip_prefix("self::") {
                format!("{}::{}", current_module, rest)
            } else if let Some(rest) = raw.strip_prefix("super::") {
                let parent = current_module
                    .rsplit_once("::")
                    .map_or("", |(parent, _)| parent);
                format!("{}::{}", parent, rest)
                    .trim_start_matches("::")
                    .to_string()
            } else {
                String::new()
            };
            if !candidate.is_empty() {
                if let Some(index) = longest_module_match(root, &candidate, modules) {
                    dependencies
                        .entry((index, line_number, "rust_use"))
                        .or_insert(());
                }
            }
        }
        let module = line
            .strip_prefix("mod ")
            .or_else(|| line.strip_prefix("pub mod "))
            .and_then(|rest| rest.split(';').next())
            .map(str::trim)
            .filter(|name| !name.is_empty());
        if let Some(module) = module {
            let candidate = format!("{}::{}", module_parent, module)
                .trim_start_matches("::")
                .to_string();
            if let Some(index) = modules.get(&rust_module_key(root, &candidate)) {
                dependencies
                    .entry((*index, line_number, "rust_module"))
                    .or_insert(());
            }
        }
    }
    dedupe_inferred(dependencies)
}

fn longest_module_match(
    root: &str,
    candidate: &str,
    modules: &HashMap<String, usize>,
) -> Option<usize> {
    let mut candidate = candidate.trim_matches(':');
    loop {
        if let Some(index) = modules.get(&rust_module_key(root, candidate)) {
            return Some(*index);
        }
        candidate = candidate.rsplit_once("::")?.0;
    }
}

fn infer_relative_dependencies(
    file: &SourceRecord,
    canonical_to_index: &HashMap<PathBuf, usize>,
) -> Vec<InferredDependency> {
    let mut dependencies = BTreeMap::new();
    for (line_number, line) in file.content.lines().enumerate() {
        let line_number = line_number + 1;
        let line = line.trim();
        if !(line.starts_with("import ")
            || line.starts_with("export ")
            || line.contains("require("))
        {
            continue;
        }
        let Some(specifier) = last_quoted_value(line) else {
            continue;
        };
        if !specifier.starts_with('.') {
            continue;
        }
        if let Some(index) = resolve_relative_import(file, specifier, canonical_to_index) {
            dependencies
                .entry((index, line_number, "static_import"))
                .or_insert(());
        }
    }
    dedupe_inferred(dependencies)
}

fn resolve_relative_import(
    file: &SourceRecord,
    specifier: &str,
    canonical_to_index: &HashMap<PathBuf, usize>,
) -> Option<usize> {
    let base = file.absolute.parent()?.join(specifier);
    let mut candidates = vec![base.clone()];
    for extension in ["ts", "tsx", "js", "jsx"] {
        candidates.push(base.with_extension(extension));
        candidates.push(base.join(format!("index.{}", extension)));
    }
    candidates.into_iter().find_map(|candidate| {
        candidate
            .canonicalize()
            .ok()
            .and_then(|path| canonical_to_index.get(&path).copied())
    })
}

fn infer_python_dependencies(
    file: &SourceRecord,
    modules: &HashMap<String, usize>,
) -> Vec<InferredDependency> {
    let mut dependencies = BTreeMap::new();
    for (line_number, line) in file.content.lines().enumerate() {
        let line_number = line_number + 1;
        let line = line.trim();
        let module = if let Some(rest) = line.strip_prefix("from ") {
            rest.split_whitespace().next()
        } else if let Some(rest) = line.strip_prefix("import ") {
            rest.split(|character: char| character.is_whitespace() || character == ',')
                .next()
        } else {
            None
        };
        let Some(module) = module else { continue };
        let mut candidate = module.trim_start_matches('.');
        loop {
            if let Some(index) = modules.get(candidate) {
                dependencies
                    .entry((*index, line_number, "python_import"))
                    .or_insert(());
                break;
            }
            let Some((parent, _)) = candidate.rsplit_once('.') else {
                break;
            };
            candidate = parent;
        }
    }
    dedupe_inferred(dependencies)
}

fn infer_go_dependencies(
    file: &SourceRecord,
    canonical_to_index: &HashMap<PathBuf, usize>,
) -> Vec<InferredDependency> {
    let mut dependencies = BTreeMap::new();
    let current_root = Path::new(&file.relative)
        .components()
        .next()
        .and_then(|part| match part {
            Component::Normal(value) => value.to_str(),
            _ => None,
        });
    for (line_number, line) in file.content.lines().enumerate() {
        let line_number = line_number + 1;
        let Some(import) = last_quoted_value(line.trim()) else {
            continue;
        };
        let Some(root_component) = current_root else {
            continue;
        };
        let Some(position) = import.find(root_component) else {
            continue;
        };
        let directory = Path::new(&import[position..]);
        if let Some(index) = canonical_to_index.iter().find_map(|(path, index)| {
            path.parent()
                .is_some_and(|parent| parent.ends_with(directory))
                .then_some(*index)
        }) {
            dependencies
                .entry((index, line_number, "go_import"))
                .or_insert(());
        }
    }
    dedupe_inferred(dependencies)
}

fn dedupe_inferred(values: BTreeMap<(usize, usize, &'static str), ()>) -> Vec<InferredDependency> {
    let mut seen_targets = BTreeSet::new();
    values
        .into_iter()
        .filter_map(|((target, line, kind), ())| {
            seen_targets
                .insert(target)
                .then_some(InferredDependency { target, line, kind })
        })
        .collect()
}

fn last_quoted_value(line: &str) -> Option<&str> {
    for quote in ['\'', '"'] {
        if let Some(end) = line.rfind(quote) {
            if let Some(start) = line[..end].rfind(quote) {
                if start < end {
                    return Some(&line[start + quote.len_utf8()..end]);
                }
            }
        }
    }
    None
}

fn normalized_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn is_manifest(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            "Cargo.toml" | "package.json" | "pyproject.toml" | "setup.cfg" | "go.mod" | "Makefile"
        )
    )
}

fn manifest_kind(path: &str) -> &'static str {
    match Path::new(path).file_name().and_then(|name| name.to_str()) {
        Some("Cargo.toml") => "cargo",
        Some("package.json") => "npm",
        Some("pyproject.toml") | Some("setup.cfg") => "python",
        Some("go.mod") => "go",
        Some("Makefile") => "make",
        _ => "unknown",
    }
}

fn manifest_commands(path: &str, content: &str) -> Vec<String> {
    let mut commands = Vec::new();
    match manifest_kind(path) {
        "cargo" => {
            commands.extend(["cargo check", "cargo test", "cargo build"].map(str::to_string));
        }
        "go" => commands.extend(["go test ./...", "go build ./..."].map(str::to_string)),
        "python" if content.contains("pytest") => commands.push("pytest".to_string()),
        "npm" => {
            if content.contains("\"test\"") {
                commands.push("npm test".to_string());
            }
            if content.contains("\"build\"") {
                commands.push("npm run build".to_string());
            }
        }
        "make" => commands.push("make".to_string()),
        _ => {}
    }
    commands.sort();
    commands.dedup();
    commands
}

fn compile_globs(patterns: &[String]) -> Result<Vec<Regex>> {
    if patterns.len() > 128 {
        anyhow::bail!("at most 128 repository glob patterns are supported");
    }
    patterns
        .iter()
        .map(|pattern| {
            if pattern.len() > 512 {
                anyhow::bail!("repository glob exceeds 512 bytes");
            }
            let mut expression = String::from("^");
            let bytes = pattern.as_bytes();
            let mut index = 0usize;
            while index < bytes.len() {
                match bytes[index] {
                    b'*' if bytes.get(index + 1) == Some(&b'*') => {
                        if bytes.get(index + 2) == Some(&b'/') {
                            expression.push_str("(?:.*/)?");
                            index += 3;
                        } else {
                            expression.push_str(".*");
                            index += 2;
                        }
                    }
                    b'*' => {
                        expression.push_str("[^/]*");
                        index += 1;
                    }
                    b'?' => {
                        expression.push_str("[^/]");
                        index += 1;
                    }
                    byte => {
                        expression.push_str(&regex::escape(&(byte as char).to_string()));
                        index += 1;
                    }
                }
            }
            expression.push('$');
            Regex::new(&expression).map_err(anyhow::Error::from)
        })
        .collect()
}

fn hard_excluded_path(relative: &str) -> bool {
    Path::new(relative).components().any(|component| {
        let Component::Normal(value) = component else {
            return true;
        };
        let lower = value.to_string_lossy().to_ascii_lowercase();
        lower.starts_with(".env")
            || lower.contains("secret")
            || lower.contains("credential")
            || lower.ends_with(".pem")
            || lower.ends_with(".key")
            || lower.ends_with(".p12")
            || matches!(lower.as_str(), "id_rsa" | "id_ed25519")
    })
}

fn is_trusted_doc(relative: &str) -> bool {
    let path = Path::new(relative);
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let conventional = matches!(
        name.to_ascii_lowercase().as_str(),
        "readme"
            | "readme.md"
            | "contributing.md"
            | "architecture.md"
            | "security.md"
            | "code_of_conduct.md"
    );
    conventional
        || (path.components().next() == Some(Component::Normal("docs".as_ref()))
            && path.extension().and_then(|ext| ext.to_str()) == Some("md"))
}

fn markdown_title(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("# ").map(str::to_string))
}

fn is_known_unsupported_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(
            "c" | "cc"
                | "cpp"
                | "h"
                | "hpp"
                | "java"
                | "kt"
                | "kts"
                | "cs"
                | "swift"
                | "rb"
                | "php"
                | "scala"
                | "lua"
                | "dart"
                | "ex"
                | "exs"
                | "hs"
                | "clj"
                | "zig"
                | "vue"
                | "svelte"
        )
    )
}

fn diagnostic(code: &str, path: Option<String>, message: impl Into<String>) -> ScanDiagnostic {
    ScanDiagnostic {
        code: code.to_string(),
        path,
        message: message.into(),
        severity: DiagnosticSeverity::Warning,
        truncated: false,
    }
}

fn diagnostic_info(code: &str, path: Option<String>, message: impl Into<String>) -> ScanDiagnostic {
    ScanDiagnostic {
        severity: DiagnosticSeverity::Info,
        ..diagnostic(code, path, message)
    }
}

fn push_diagnostic(diagnostics: &mut Vec<ScanDiagnostic>, diagnostic: ScanDiagnostic) {
    if diagnostics.len() < MAX_DIAGNOSTICS {
        diagnostics.push(diagnostic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_matches_sha256_vector() {
        assert_eq!(
            content_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn rust_modules_resolve_per_crate_in_a_workspace_scanned_from_its_root() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for dir in ["crates/a/src", "crates/b/src"] {
            std::fs::create_dir_all(temp.path().join(dir))?;
        }
        std::fs::write(temp.path().join("crates/a/src/z.rs"), "pub fn f() {}\n")?;
        std::fs::write(
            temp.path().join("crates/a/src/lib.rs"),
            "pub mod z;\npub mod o;\n",
        )?;
        std::fs::write(temp.path().join("crates/a/src/o.rs"), "use crate::z::f;\n")?;
        // Crate b has no `z`: its `crate::z` must not resolve into crate a.
        std::fs::write(
            temp.path().join("crates/b/src/lib.rs"),
            "use crate::z::f;\n",
        )?;

        let facts = RepositoryFacts::scan(temp.path())?;
        let path_of = |id: &str| {
            facts
                .files
                .iter()
                .find(|file| file.id == id)
                .map(|file| file.path.clone())
                .unwrap_or_default()
        };
        let mut edges: Vec<(String, String, String)> = facts
            .dependencies
            .iter()
            .map(|e| {
                (
                    path_of(&e.from_file_id),
                    path_of(&e.to_file_id),
                    e.kind.clone(),
                )
            })
            .collect();
        edges.sort();
        let edge =
            |from: &str, to: &str, kind: &str| (from.to_owned(), to.to_owned(), kind.to_owned());
        assert_eq!(
            edges,
            vec![
                edge("crates/a/src/lib.rs", "crates/a/src/o.rs", "rust_module"),
                edge("crates/a/src/lib.rs", "crates/a/src/z.rs", "rust_module"),
                edge("crates/a/src/o.rs", "crates/a/src/z.rs", "rust_use"),
            ]
        );
        Ok(())
    }

    #[test]
    fn scans_deterministically_and_normalizes_paths() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir_all(temp.path().join("src/z"))?;
        std::fs::create_dir_all(temp.path().join("vendor"))?;
        std::fs::write(temp.path().join("src/z/mod.rs"), "pub fn zed() {}\n")?;
        std::fs::write(temp.path().join("src/lib.rs"), "pub mod z;\n")?;
        std::fs::write(temp.path().join("vendor/ignored.rs"), "pub fn no() {}\n")?;

        let first = RepositoryFacts::scan(temp.path())?;
        let second = RepositoryFacts::scan(&temp.path().join("."))?;
        assert_eq!(first, second);
        assert_eq!(first.files[0].path, "src/lib.rs");
        assert!(first.files.iter().all(|file| !file.path.contains("\\")));
        assert!(first.symbols.iter().any(|symbol| symbol.name == "zed"));
        assert!(first
            .dependencies
            .iter()
            .any(|edge| edge.kind == "rust_module"));
        Ok(())
    }

    #[test]
    fn reports_file_and_edge_caps() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir_all(temp.path().join("src"))?;
        for index in 0..3 {
            std::fs::write(
                temp.path().join(format!("src/{index}.rs")),
                if index == 0 {
                    "mod one;\nmod two;\n"
                } else {
                    "pub struct Value;\n"
                },
            )?;
        }
        let facts = RepositoryFacts::scan_with_options(
            temp.path(),
            ScanOptions {
                max_files: 2,
                max_edges: 1,
                max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            },
        )?;
        assert!(facts
            .diagnostics
            .iter()
            .any(|diag| diag.code == "max_files"));
        assert!(facts.files.len() <= 2);
        Ok(())
    }

    #[test]
    fn stable_json_is_byte_identical() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::write(
            temp.path().join("main.go"),
            "package main\nfunc main() {}\n",
        )?;
        let facts = RepositoryFacts::scan(temp.path())?;
        assert_eq!(facts.to_stable_json()?, facts.to_stable_json()?);
        Ok(())
    }

    #[test]
    fn document_policy_applies_before_fact_creation() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir_all(temp.path().join("docs/private"))?;
        std::fs::write(temp.path().join("README.md"), "# Public\n")?;
        std::fs::write(temp.path().join("docs/guide.md"), "# Guide\n")?;
        std::fs::write(temp.path().join("docs/private/hidden.md"), "# Hidden\n")?;
        std::fs::write(temp.path().join("docs/secret-notes.md"), "# Secret\n")?;
        let facts = RepositoryFacts::scan_with_document_policy(
            temp.path(),
            ScanOptions::default(),
            &["README.md".to_string(), "docs/**/*.md".to_string()],
            &["docs/private/**".to_string()],
        )?;
        let paths: Vec<_> = facts
            .trusted_docs
            .iter()
            .map(|doc| doc.path.as_str())
            .collect();
        assert_eq!(paths, vec!["README.md", "docs/guide.md"]);
        Ok(())
    }

    #[test]
    fn pruned_entries_and_unsupported_files_are_diagnosed_not_silent() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir_all(temp.path().join("src"))?;
        std::fs::create_dir_all(temp.path().join("vendor"))?;
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn a() {}\n")?;
        std::fs::write(temp.path().join("src/App.svelte"), "<script></script>\n")?;
        std::fs::write(temp.path().join(".env"), "K=v\n")?;
        let facts = RepositoryFacts::scan(temp.path())?;
        let codes: Vec<(&str, Option<&str>)> = facts
            .diagnostics
            .iter()
            .map(|d| (d.code.as_str(), d.path.as_deref()))
            .collect();
        assert!(codes.contains(&("skipped_directory", Some("vendor"))));
        assert!(codes.contains(&("unsupported_language", Some("src/App.svelte"))));
        assert!(!codes.iter().any(|(code, _)| *code == "hard_excluded"));
        assert!(
            !format!("{codes:?}").contains(".env"),
            "secret names must not leak"
        );
        Ok(())
    }
}
