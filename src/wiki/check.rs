use crate::repo::content_hash;
use crate::wiki::config::WikiConfig;
use crate::wiki::git::{ChangeKind, GitDelta};
use crate::wiki::model::{MigrationStatus, PageId, StaleReason, WikiManifestV1};
use crate::wiki::state::{recover_staging, save_manifest_locked, RepositoryLock};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalePage {
    pub page_id: PageId,
    pub reasons: Vec<StaleReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiStatus {
    pub schema_version: u32,
    pub current: bool,
    pub state_missing: bool,
    pub stale_pages: Vec<StalePage>,
    pub diagnostics: Vec<String>,
}

pub fn status(project_root: &Path) -> Result<WikiStatus> {
    let root = project_root.canonicalize()?;
    let config = WikiConfig::load(&root)?;
    let state_dir = crate::wiki::safety::validate_managed_directory(&root, &config.state_dir)?;
    let manifest_path = state_dir.join("manifest.json");
    let manifest = match WikiManifestV1::load(&manifest_path) {
        Ok(manifest) => manifest,
        Err(crate::wiki::state::StateError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(WikiStatus {
                schema_version: crate::wiki::WIKI_SCHEMA_VERSION,
                current: false,
                state_missing: true,
                stale_pages: Vec::new(),
                diagnostics: vec!["wiki state is missing; run `klaar wiki build`".to_string()],
            });
        }
        Err(error) => return Err(error.into()),
    };
    let facts = crate::repo::RepositoryFacts::scan_with_document_policy(
        &root,
        crate::repo::ScanOptions {
            max_files: config.budgets.max_input_files,
            ..crate::repo::ScanOptions::default()
        },
        &config.documents.trusted,
        &config.documents.exclude,
    )?;
    let mut current: BTreeMap<String, String> = facts
        .files
        .iter()
        .map(|item| (item.path.clone(), item.content_hash.clone()))
        .chain(
            facts
                .manifests
                .iter()
                .map(|item| (item.path.clone(), item.content_hash.clone())),
        )
        .chain(
            facts
                .trusted_docs
                .iter()
                .map(|item| (item.path.clone(), item.content_hash.clone())),
        )
        .collect();
    if let Ok(bytes) = std::fs::read(root.join(".klaar/wiki.toml")) {
        current.insert(".klaar/wiki.toml".to_string(), content_hash(&bytes));
    }
    let mut result = status_with_hashes(&manifest, &current);
    verify_artifacts(&root, &config, &manifest, &mut result)?;
    Ok(result)
}

pub fn status_from_manifest(root: &Path, manifest: &WikiManifestV1) -> WikiStatus {
    let current = manifest
        .source_hashes
        .keys()
        .filter_map(|path| {
            std::fs::read(root.join(path))
                .ok()
                .map(|bytes| (path.clone(), content_hash(&bytes)))
        })
        .collect();
    status_with_hashes(manifest, &current)
}

fn status_with_hashes(manifest: &WikiManifestV1, current: &BTreeMap<String, String>) -> WikiStatus {
    let changed = changed_source_paths(&manifest.source_hashes, current);
    let mut stale_pages = Vec::new();
    for page in &manifest.canonical_pages {
        let mut reasons: BTreeSet<_> = page.stale_reasons.iter().cloned().collect();
        if manifest.migration_status == MigrationStatus::RequiresRebuild {
            reasons.insert(StaleReason::SchemaIncompatible);
        }
        if changed.contains_key(".klaar/wiki.toml") {
            reasons.insert(StaleReason::ConfigurationChanged);
        }
        for evidence in &page.evidence {
            if let Some(reason) = changed.get(&evidence.source.path) {
                reasons.insert(reason.clone());
            }
        }
        for render in manifest
            .lens_renders
            .iter()
            .filter(|render| render.page_id == page.page_id)
        {
            reasons.extend(render.stale_reasons.iter().cloned());
        }
        if !reasons.is_empty() {
            stale_pages.push(StalePage {
                page_id: page.page_id.clone(),
                reasons: reasons.into_iter().collect(),
            });
        }
    }
    for path in current
        .keys()
        .filter(|path| !manifest.source_hashes.contains_key(*path))
    {
        for page in manifest.canonical_pages.iter().filter(|page| {
            matches!(
                page.page_id.as_str(),
                "index" | "architecture" | "dependencies"
            )
        }) {
            if let Some(existing) = stale_pages
                .iter_mut()
                .find(|item| item.page_id == page.page_id)
            {
                existing
                    .reasons
                    .push(StaleReason::SourceChanged { path: path.clone() });
                existing.reasons.sort();
                existing.reasons.dedup();
            } else {
                stale_pages.push(StalePage {
                    page_id: page.page_id.clone(),
                    reasons: vec![StaleReason::SourceChanged { path: path.clone() }],
                });
            }
        }
    }
    stale_pages.sort_by(|a, b| a.page_id.cmp(&b.page_id));
    let mut diagnostics = Vec::new();
    if !changed.is_empty() && stale_pages.is_empty() {
        diagnostics
            .push("unmapped source changes require a wider deterministic rebuild".to_string());
    }
    if manifest.migration_status == MigrationStatus::RequiresRebuild {
        diagnostics.push("wiki state requires a schema rebuild".to_string());
    }
    WikiStatus {
        schema_version: crate::wiki::WIKI_SCHEMA_VERSION,
        current: changed.is_empty()
            && stale_pages.is_empty()
            && manifest.migration_status == MigrationStatus::Current,
        state_missing: false,
        stale_pages,
        diagnostics,
    }
}

fn verify_artifacts(
    root: &Path,
    config: &WikiConfig,
    manifest: &WikiManifestV1,
    result: &mut WikiStatus,
) -> Result<()> {
    let output_dir = crate::wiki::safety::validate_managed_directory(root, &config.output_dir)?;
    let canonical_output = output_dir.canonicalize().ok();
    for page in &manifest.canonical_pages {
        for lens in &manifest.lenses {
            if !manifest
                .lens_renders
                .iter()
                .any(|render| render.page_id == page.page_id && &render.lens == lens)
            {
                add_stale_reason(result, &page.page_id, StaleReason::Manual);
            }
        }
    }
    for render in &manifest.lens_renders {
        let path = output_dir.join(render.output_path.to_path_buf());
        let valid = std::fs::metadata(&path)
            .ok()
            .filter(|metadata| {
                metadata.is_file()
                    && metadata.len() <= config.budgets.max_output_bytes_per_page as u64
            })
            .and_then(|_| path.canonicalize().ok())
            .filter(|resolved| {
                canonical_output
                    .as_ref()
                    .is_some_and(|output| resolved.starts_with(output))
            })
            .and_then(|resolved| std::fs::read(resolved).ok())
            .is_some_and(|bytes| content_hash(&bytes) == render.content_hash);
        if !valid {
            add_stale_reason(result, &render.page_id, StaleReason::Manual);
        }
    }
    result.current = result.current && result.stale_pages.is_empty();
    Ok(())
}

fn add_stale_reason(result: &mut WikiStatus, page_id: &PageId, reason: StaleReason) {
    if let Some(page) = result
        .stale_pages
        .iter_mut()
        .find(|page| &page.page_id == page_id)
    {
        page.reasons.push(reason);
        page.reasons.sort();
        page.reasons.dedup();
    } else {
        result.stale_pages.push(StalePage {
            page_id: page_id.clone(),
            reasons: vec![reason],
        });
        result.stale_pages.sort_by(|a, b| a.page_id.cmp(&b.page_id));
    }
}

pub fn affected_pages(manifest: &WikiManifestV1, delta: &GitDelta) -> Vec<StalePage> {
    let changed: BTreeSet<_> = delta.affected_paths().into_iter().collect();
    let changes: BTreeMap<_, _> = delta
        .changes
        .iter()
        .flat_map(|change| {
            let reason = match change.kind {
                ChangeKind::Deleted => StaleReason::SourceDeleted {
                    path: change.path.clone(),
                },
                ChangeKind::Renamed => StaleReason::SourceRenamed {
                    from: change.previous_path.clone().unwrap_or_default(),
                    to: change.path.clone(),
                },
                _ => StaleReason::SourceChanged {
                    path: change.path.clone(),
                },
            };
            let mut paths = vec![(change.path.as_str(), reason.clone())];
            if let Some(previous) = change.previous_path.as_deref() {
                paths.push((previous, reason));
            }
            paths
        })
        .collect();
    let mut pages = Vec::new();
    for page in &manifest.canonical_pages {
        let reasons: BTreeSet<_> = page
            .evidence
            .iter()
            .filter(|evidence| changed.contains(evidence.source.path.as_str()))
            .filter_map(|evidence| changes.get(evidence.source.path.as_str()).cloned())
            .collect();
        if !reasons.is_empty() {
            pages.push(StalePage {
                page_id: page.page_id.clone(),
                reasons: reasons.into_iter().collect(),
            });
        }
    }
    // Added files have no reverse edge yet. Core topology pages are the bounded fallback.
    if delta.changes.iter().any(|change| {
        matches!(change.kind, ChangeKind::Added | ChangeKind::Copied)
            && !manifest.source_hashes.contains_key(&change.path)
    }) {
        for id in ["index", "architecture", "dependencies"] {
            if let Some(page) = manifest
                .canonical_pages
                .iter()
                .find(|page| page.page_id.as_str() == id)
            {
                if !pages.iter().any(|item| item.page_id == page.page_id) {
                    pages.push(StalePage {
                        page_id: page.page_id.clone(),
                        reasons: vec![StaleReason::SourceChanged {
                            path: "new repository input".to_string(),
                        }],
                    });
                }
            }
        }
    }
    pages.sort_by(|a, b| a.page_id.cmp(&b.page_id));
    pages
}

pub fn mark_stale(project_root: &Path, paths: &[String]) -> Result<Vec<StalePage>> {
    let root = project_root.canonicalize()?;
    let config = WikiConfig::load(&root)?;
    let state_dir = crate::wiki::safety::validate_managed_directory(&root, &config.state_dir)?;
    let manifest_path = state_dir.join("manifest.json");
    let _lock = RepositoryLock::acquire(&state_dir)?;
    recover_staging(&state_dir)?;
    let mut manifest = WikiManifestV1::load(&manifest_path)?;
    let changed: BTreeSet<_> = paths.iter().map(String::as_str).collect();
    let mut affected = Vec::new();
    for page in &mut manifest.canonical_pages {
        let mut reasons: BTreeSet<_> = page.stale_reasons.iter().cloned().collect();
        for evidence in &page.evidence {
            if changed.contains(evidence.source.path.as_str()) {
                reasons.insert(StaleReason::SourceChanged {
                    path: evidence.source.path.clone(),
                });
            }
        }
        if !reasons.is_empty() {
            page.stale_reasons = reasons.iter().cloned().collect();
            affected.push(StalePage {
                page_id: page.page_id.clone(),
                reasons: reasons.into_iter().collect(),
            });
        }
    }
    if paths
        .iter()
        .any(|path| !manifest.source_hashes.contains_key(path))
    {
        for page in manifest.canonical_pages.iter_mut().filter(|page| {
            matches!(
                page.page_id.as_str(),
                "index" | "architecture" | "dependencies"
            )
        }) {
            let reason = StaleReason::SourceChanged {
                path: "new repository input".to_string(),
            };
            if !page.stale_reasons.contains(&reason) {
                page.stale_reasons.push(reason.clone());
                page.stale_reasons.sort();
            }
            if let Some(existing) = affected
                .iter_mut()
                .find(|item| item.page_id == page.page_id)
            {
                if !existing.reasons.contains(&reason) {
                    existing.reasons.push(reason);
                    existing.reasons.sort();
                }
            } else {
                affected.push(StalePage {
                    page_id: page.page_id.clone(),
                    reasons: vec![reason],
                });
            }
        }
    }
    affected.sort_by(|a, b| a.page_id.cmp(&b.page_id));
    for render in &mut manifest.lens_renders {
        if let Some(page) = affected.iter().find(|page| page.page_id == render.page_id) {
            render.stale_reasons = page.reasons.clone();
        }
    }
    save_manifest_locked(&manifest_path, &manifest)?;
    Ok(affected)
}

pub fn explain_stale(project_root: &Path, page_id: &PageId) -> Result<Option<StalePage>> {
    Ok(status(project_root)?
        .stale_pages
        .into_iter()
        .find(|page| &page.page_id == page_id))
}

fn changed_source_paths(
    expected: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> BTreeMap<String, StaleReason> {
    expected
        .iter()
        .filter_map(|(path, hash)| match current.get(path) {
            Some(current_hash) if current_hash == hash => None,
            Some(_) => Some((
                path.clone(),
                StaleReason::SourceChanged { path: path.clone() },
            )),
            None => Some((
                path.clone(),
                StaleReason::SourceDeleted { path: path.clone() },
            )),
        })
        .collect()
}

pub async fn check_revisions(project_root: &Path, base: &str, head: &str) -> Result<WikiStatus> {
    let delta = crate::wiki::git::compute_delta(project_root, base, head)
        .await
        .context("cannot calculate wiki Git delta")?;
    let mut result = status(project_root)?;
    if !result.state_missing {
        let config = WikiConfig::load(project_root)?;
        let manifest = WikiManifestV1::load(
            &crate::wiki::safety::validate_managed_directory(project_root, &config.state_dir)?
                .join("manifest.json"),
        )?;
        let affected = affected_pages(&manifest, &delta);
        if !affected.is_empty() && !result.current {
            result.diagnostics.push(format!(
                "{} pages are affected by {}..{}",
                affected.len(),
                base,
                head
            ));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::model::WikiManifestV1;

    #[test]
    fn missing_state_is_explicit() -> Result<()> {
        let root = tempfile::tempdir()?;
        crate::wiki::build::init(root.path())?;
        let result = status(root.path())?;
        assert!(result.state_missing);
        assert!(!result.current);
        Ok(())
    }

    #[test]
    fn empty_manifest_is_current() {
        let root = tempfile::tempdir().unwrap();
        assert!(status_from_manifest(root.path(), &WikiManifestV1::empty("repo", "test")).current);
    }

    #[tokio::test]
    async fn newly_added_source_marks_core_pages_stale() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("main.go"), "package main\n")?;
        crate::wiki::build::init(root.path())?;
        crate::wiki::build::build(root.path(), None).await?;
        std::fs::write(root.path().join("added.go"), "package main\n")?;
        let result = status(root.path())?;
        assert!(!result.current);
        assert!(result
            .stale_pages
            .iter()
            .any(|page| page.page_id.as_str() == "index"));
        Ok(())
    }

    #[tokio::test]
    async fn stale_marking_holds_the_repository_transaction_lock() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("main.go"), "package main\n")?;
        crate::wiki::build::init(root.path())?;
        crate::wiki::build::build(root.path(), None).await?;
        let state_dir = root.path().join(".klaar/wiki-state");
        let lock = RepositoryLock::acquire(&state_dir)?;
        let result = mark_stale(root.path(), &["main.go".to_string()]);
        drop(lock);
        assert!(matches!(
            result,
            Err(error) if error.to_string().contains("wiki state is locked")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn tampered_artifact_is_not_current() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("main.go"), "package main\n")?;
        crate::wiki::build::init(root.path())?;
        crate::wiki::build::build(root.path(), None).await?;
        std::fs::write(
            root.path().join(".klaar/wiki/technical/index.md"),
            "tampered",
        )?;
        let result = status(root.path())?;
        assert!(!result.current);
        assert!(result
            .stale_pages
            .iter()
            .any(|page| page.page_id.as_str() == "index"));
        Ok(())
    }

    #[tokio::test]
    async fn initial_partial_lens_build_is_explicitly_incomplete() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(root.path().join("main.go"), "package main\n")?;
        crate::wiki::build::init(root.path())?;
        crate::wiki::build::build(root.path(), Some("technical")).await?;
        assert!(!status(root.path())?.current);
        Ok(())
    }
}
