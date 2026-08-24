//! Durable, deterministic state for the repository wiki.

use crate::wiki::model::{
    CanonicalPageState, EvidenceId, PageId, StaleReason, WikiManifestV1, WIKI_SCHEMA_VERSION,
};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum StateError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Corrupt {
        path: PathBuf,
        message: String,
    },
    Incompatible {
        path: PathBuf,
        found: u32,
        expected: u32,
    },
    Locked {
        path: PathBuf,
    },
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "cannot {operation} {}: {source}", path.display()),
            Self::Corrupt { path, message } => {
                write!(f, "corrupt wiki state {}: {message}", path.display())
            }
            Self::Incompatible {
                path,
                found,
                expected,
            } => write!(
                f,
                "incompatible wiki state {}: schema {found}, expected {expected}",
                path.display()
            ),
            Self::Locked { path } => write!(f, "wiki state is locked: {}", path.display()),
        }
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Forward and reverse page/evidence edges. Vectors are used on disk for a compact,
/// portable contract; all mutating operations keep them sorted and unique.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProvenanceState {
    pub forward: BTreeMap<PageId, Vec<EvidenceId>>,
    pub reverse: BTreeMap<EvidenceId, Vec<PageId>>,
}

impl ProvenanceState {
    pub fn record(&mut self, page: PageId, evidence: EvidenceId) {
        insert_sorted(
            self.forward.entry(page.clone()).or_default(),
            evidence.clone(),
        );
        insert_sorted(self.reverse.entry(evidence).or_default(), page);
    }

    pub fn evidence_for(&self, page: &PageId) -> &[EvidenceId] {
        self.forward.get(page).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn pages_for(&self, evidence: &EvidenceId) -> &[PageId] {
        self.reverse.get(evidence).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn remove_page(&mut self, page: &PageId) {
        if let Some(edges) = self.forward.remove(page) {
            for evidence in edges {
                remove_sorted(self.reverse.get_mut(&evidence), page);
                if self.reverse.get(&evidence).is_some_and(Vec::is_empty) {
                    self.reverse.remove(&evidence);
                }
            }
        }
    }

    pub fn remove_evidence(&mut self, evidence: &EvidenceId) {
        if let Some(pages) = self.reverse.remove(evidence) {
            for page in pages {
                remove_sorted(self.forward.get_mut(&page), evidence);
                if self.forward.get(&page).is_some_and(Vec::is_empty) {
                    self.forward.remove(&page);
                }
            }
        }
    }

    pub fn from_pages(pages: &[CanonicalPageState]) -> Self {
        let mut state = Self::default();
        for page in pages {
            for evidence in &page.evidence {
                state.record(page.page_id.clone(), evidence.id.clone());
            }
        }
        state
    }

    pub fn normalize(&mut self) {
        for values in self.forward.values_mut() {
            normalize(values);
        }
        for values in self.reverse.values_mut() {
            normalize(values);
        }
        self.forward.retain(|_, values| !values.is_empty());
        self.reverse.retain(|_, values| !values.is_empty());
    }
}

fn normalize<T: Ord>(values: &mut Vec<T>) {
    values.sort();
    values.dedup();
}
fn insert_sorted<T: Ord>(values: &mut Vec<T>, value: T) {
    values.push(value);
    normalize(values);
}
fn remove_sorted<T: Ord + PartialEq>(values: Option<&mut Vec<T>>, value: &T) {
    if let Some(values) = values {
        values.retain(|item| item != value);
    }
}

pub fn stale_reason_source_changed(path: impl Into<String>) -> StaleReason {
    StaleReason::SourceChanged { path: path.into() }
}
pub fn stale_reason_source_deleted(path: impl Into<String>) -> StaleReason {
    StaleReason::SourceDeleted { path: path.into() }
}
pub fn stale_reason_source_renamed(from: impl Into<String>, to: impl Into<String>) -> StaleReason {
    StaleReason::SourceRenamed {
        from: from.into(),
        to: to.into(),
    }
}
pub fn stale_reason_evidence_changed(id: EvidenceId) -> StaleReason {
    StaleReason::EvidenceChanged { evidence_id: id }
}
pub fn stale_reasons_for_change(paths: impl IntoIterator<Item = String>) -> Vec<StaleReason> {
    let reasons: BTreeSet<_> = paths.into_iter().map(stale_reason_source_changed).collect();
    reasons.into_iter().collect()
}

pub fn stable_json<T: Serialize>(value: &T) -> Result<Vec<u8>, StateError> {
    serde_json::to_vec_pretty(value)
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })
        .map_err(|error| StateError::Corrupt {
            path: PathBuf::from("<serialization>"),
            message: error.to_string(),
        })
}

pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T, StateError> {
    let bytes = fs::read(path).map_err(|source| StateError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|error| StateError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

pub fn load_manifest(path: &Path) -> Result<WikiManifestV1, StateError> {
    let manifest: WikiManifestV1 = load_json(path)?;
    if manifest.schema_version != WIKI_SCHEMA_VERSION {
        return Err(StateError::Incompatible {
            path: path.to_path_buf(),
            found: manifest.schema_version,
            expected: WIKI_SCHEMA_VERSION,
        });
    }
    Ok(manifest)
}

pub fn save_manifest(path: &Path, manifest: &WikiManifestV1) -> Result<(), StateError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let _lock = RepositoryLock::acquire(parent)?;
    recover_staging(parent)?;
    save_manifest_locked(path, manifest)
}

pub(crate) fn save_manifest_locked(
    path: &Path,
    manifest: &WikiManifestV1,
) -> Result<(), StateError> {
    let mut manifest = manifest.clone();
    canonicalize_manifest(&mut manifest);
    AtomicStaging::new(path)?.tap_write(&stable_json(&manifest)?)
}

impl WikiManifestV1 {
    pub fn load(path: &Path) -> Result<Self, StateError> {
        load_manifest(path)
    }
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        save_manifest(path, self)
    }
}

fn canonicalize_manifest(manifest: &mut WikiManifestV1) {
    manifest.lenses.sort();
    manifest.lenses.dedup();
    manifest
        .canonical_pages
        .sort_by(|a, b| a.page_id.cmp(&b.page_id));
    manifest
        .lens_renders
        .sort_by(|a, b| a.page_id.cmp(&b.page_id).then(a.lens.cmp(&b.lens)));
    manifest
        .diagnostics
        .sort_by(|a, b| a.code.cmp(&b.code).then(a.message.cmp(&b.message)));
    manifest.provenance.normalize();
}

pub struct RepositoryLock {
    _file: File,
}
impl RepositoryLock {
    pub fn acquire(state_dir: &Path) -> Result<Self, StateError> {
        fs::create_dir_all(state_dir).map_err(|source| StateError::Io {
            operation: "create state directory",
            path: state_dir.to_path_buf(),
            source,
        })?;
        let path = state_dir.join(".lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| StateError::Io {
                operation: "open lock",
                path: path.clone(),
                source,
            })?;
        file.try_lock_exclusive()
            .map_err(|_| StateError::Locked { path: path.clone() })?;
        Ok(Self { _file: file })
    }
}
impl Drop for RepositoryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._file);
    }
}

pub struct AtomicStaging {
    target: PathBuf,
    staging: PathBuf,
    file: Option<File>,
}
impl AtomicStaging {
    pub fn new(target: &Path) -> Result<Self, StateError> {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| StateError::Io {
                operation: "create staging directory",
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let staging = target.with_file_name(format!(
            ".{}.staging",
            target
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("state")
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|source| StateError::Io {
                operation: "create staging file",
                path: staging.clone(),
                source,
            })?;
        Ok(Self {
            target: target.to_path_buf(),
            staging,
            file: Some(file),
        })
    }
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.file
            .as_mut()
            .expect("staging file open")
            .write_all(bytes)
            .map_err(|source| StateError::Io {
                operation: "write staging file",
                path: self.staging.clone(),
                source,
            })
    }
    pub fn commit(mut self) -> Result<(), StateError> {
        if let Some(file) = self.file.take() {
            file.sync_all().map_err(|source| StateError::Io {
                operation: "sync staging file",
                path: self.staging.clone(),
                source,
            })?;
        }
        replace_staged(&self.staging, &self.target)
    }
}
impl Drop for AtomicStaging {
    fn drop(&mut self) {
        if self.file.is_some() {
            let _ = fs::remove_file(&self.staging);
        }
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StateError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let _lock = RepositoryLock::acquire(parent)?;
    recover_staging(parent)?;
    AtomicStaging::new(path)?.tap_write(bytes)
}
trait TapWrite {
    fn tap_write(self, bytes: &[u8]) -> Result<(), StateError>;
}
impl TapWrite for AtomicStaging {
    fn tap_write(mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.write_all(bytes)?;
        self.commit()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed: Vec<PathBuf>,
    pub recovered: Vec<PathBuf>,
}
pub fn recover_staging(directory: &Path) -> Result<CleanupReport, StateError> {
    let mut report = CleanupReport {
        removed: Vec::new(),
        recovered: Vec::new(),
    };
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(report),
        Err(source) => {
            return Err(StateError::Io {
                operation: "read staging directory",
                path: directory.to_path_buf(),
                source,
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| StateError::Io {
            operation: "read staging entry",
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') && name.ends_with(".backup") {
            let target_name = &name[1..name.len() - ".backup".len()];
            let target = directory.join(target_name);
            if target.exists() {
                fs::remove_file(&path).map_err(|source| StateError::Io {
                    operation: "remove committed backup",
                    path: path.clone(),
                    source,
                })?;
                report.removed.push(path);
            } else {
                fs::rename(&path, &target).map_err(|source| StateError::Io {
                    operation: "restore staged backup",
                    path: target.clone(),
                    source,
                })?;
                report.recovered.push(target);
            }
            continue;
        }
        if !name.starts_with('.') || !name.ends_with(".staging") {
            continue;
        }
        let target_name = &name[1..name.len() - ".staging".len()];
        let target = directory.join(target_name);
        if !target.exists() {
            fs::rename(&path, &target).map_err(|source| StateError::Io {
                operation: "recover staged file",
                path: target.clone(),
                source,
            })?;
            report.recovered.push(target);
        } else {
            fs::remove_file(&path).map_err(|source| StateError::Io {
                operation: "remove stale staging file",
                path: path.clone(),
                source,
            })?;
            report.removed.push(path);
        }
    }
    report.recovered.sort();
    report.removed.sort();
    Ok(report)
}

#[cfg(not(windows))]
fn replace_staged(staging: &Path, target: &Path) -> Result<(), StateError> {
    fs::rename(staging, target).map_err(|source| StateError::Io {
        operation: "commit staged file",
        path: target.to_path_buf(),
        source,
    })
}

#[cfg(windows)]
fn replace_staged(staging: &Path, target: &Path) -> Result<(), StateError> {
    let backup = target.with_file_name(format!(
        ".{}.backup",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state")
    ));
    if target.exists() {
        fs::rename(target, &backup).map_err(|source| StateError::Io {
            operation: "stage previous file",
            path: target.to_path_buf(),
            source,
        })?;
    }
    if let Err(source) = fs::rename(staging, target) {
        if backup.exists() {
            let _ = fs::rename(&backup, target);
        }
        return Err(StateError::Io {
            operation: "commit staged file",
            path: target.to_path_buf(),
            source,
        });
    }
    if backup.exists() {
        fs::remove_file(&backup).map_err(|source| StateError::Io {
            operation: "remove staged backup",
            path: backup,
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::model::{EvidenceKind, EvidenceRef, PageKind, SourceIdentity};
    use tempfile::tempdir;

    fn ids() -> (PageId, EvidenceId) {
        (
            PageId::new("index").unwrap(),
            EvidenceId::new("source-1").unwrap(),
        )
    }
    #[test]
    fn provenance_is_bidirectional_and_sorted() {
        let (page, evidence) = ids();
        let mut p = ProvenanceState::default();
        p.record(page.clone(), evidence.clone());
        p.record(page.clone(), evidence.clone());
        assert_eq!(p.evidence_for(&page), std::slice::from_ref(&evidence));
        assert_eq!(p.pages_for(&evidence), &[page]);
        p.remove_evidence(&evidence);
        assert!(p.forward.is_empty() && p.reverse.is_empty());
    }
    #[test]
    fn manifest_json_is_stable_and_load_checks_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        let manifest = WikiManifestV1::empty("repo", "test");
        manifest.save(&path).unwrap();
        let first = fs::read(&path).unwrap();
        manifest.save(&path).unwrap();
        assert_eq!(first, fs::read(&path).unwrap());
        assert_eq!(WikiManifestV1::load(&path).unwrap().repository_id, "repo");
        fs::write(&path, b"{").unwrap();
        assert!(matches!(
            load_manifest(&path),
            Err(StateError::Corrupt { .. })
        ));
    }
    #[test]
    fn atomic_recovery_promotes_or_cleans() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("manifest.json");
        let mut staged = AtomicStaging::new(&target).unwrap();
        staged.write_all(b"{}").unwrap();
        std::mem::forget(staged);
        let report = recover_staging(dir.path()).unwrap();
        assert_eq!(report.recovered, vec![target.clone()]);
        let mut staged = AtomicStaging::new(&target).unwrap();
        staged.write_all(b"old").unwrap();
        std::mem::forget(staged);
        let report = recover_staging(dir.path()).unwrap();
        assert_eq!(report.removed.len(), 1);
    }
    #[test]
    fn helper_reasons_are_deterministic() {
        assert_eq!(
            stale_reasons_for_change(vec!["b".into(), "a".into(), "b".into()]),
            vec![
                stale_reason_source_changed("a"),
                stale_reason_source_changed("b")
            ]
        );
    }
    #[test]
    fn repository_lock_is_released_by_the_os_handle() {
        let dir = tempdir().unwrap();
        let first = RepositoryLock::acquire(dir.path()).unwrap();
        assert!(matches!(
            RepositoryLock::acquire(dir.path()),
            Err(StateError::Locked { .. })
        ));
        drop(first);
        RepositoryLock::acquire(dir.path()).unwrap();
    }
    #[allow(dead_code)]
    fn _contract_types_compile(_: EvidenceRef, _: EvidenceKind, _: PageKind, _: SourceIdentity) {}
}
