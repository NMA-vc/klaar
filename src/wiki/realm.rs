use crate::wiki::model::{ContentHash, EvidenceRef, LensId, PageId, WikiManifestV1};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const REALM_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryWikiExportV1 {
    pub schema_version: u32,
    pub repository_id: String,
    pub manifest_hash: ContentHash,
    pub manifest: WikiManifestV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealmMemberStatus {
    pub repository_id: String,
    pub manifest_hash: ContentHash,
    pub lenses: Vec<LensId>,
    pub stale_pages: Vec<PageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealmPageRef {
    pub repository_id: String,
    pub page_id: PageId,
    pub title: String,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedSourceRef {
    pub source_path: String,
    pub repositories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealmManifestV1 {
    pub schema_version: u32,
    pub members: Vec<RealmMemberStatus>,
    pub pages: Vec<RealmPageRef>,
    pub shared_source_names: Vec<SharedSourceRef>,
    pub diagnostics: Vec<String>,
}

impl RepositoryWikiExportV1 {
    pub fn from_manifest(manifest: WikiManifestV1) -> Result<Self> {
        if manifest.schema_version != crate::wiki::WIKI_SCHEMA_VERSION {
            anyhow::bail!("repository wiki schema is incompatible");
        }
        let manifest_hash = stable_json_hash(&manifest)?;
        Ok(Self {
            schema_version: REALM_SCHEMA_VERSION,
            repository_id: manifest.repository_id.clone(),
            manifest_hash,
            manifest,
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("cannot read Realm export {}", path.display()))?;
        let export: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid Realm export {}", path.display()))?;
        if export.schema_version != REALM_SCHEMA_VERSION {
            anyhow::bail!("Realm export schema is incompatible");
        }
        if export.repository_id != export.manifest.repository_id {
            anyhow::bail!("Realm export repository identity mismatch");
        }
        if stable_json_hash(&export.manifest)? != export.manifest_hash {
            anyhow::bail!("Realm export manifest hash mismatch");
        }
        Ok(export)
    }

    pub fn to_stable_json(&self) -> Result<Vec<u8>> {
        stable_json(self)
    }
}

/// Federate only versioned repository exports. No repository source is read here.
pub fn federate(mut exports: Vec<RepositoryWikiExportV1>) -> Result<RealmManifestV1> {
    exports.sort_by(|a, b| a.repository_id.cmp(&b.repository_id));
    let mut seen = BTreeSet::new();
    let mut members = Vec::new();
    let mut pages = Vec::new();
    let mut source_owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut diagnostics = Vec::new();

    for export in exports {
        if !seen.insert(export.repository_id.clone()) {
            anyhow::bail!("duplicate Realm repository identity");
        }
        let mut stale_pages = Vec::new();
        for page in &export.manifest.canonical_pages {
            let render_stale =
                export.manifest.lens_renders.iter().any(|render| {
                    render.page_id == page.page_id && !render.stale_reasons.is_empty()
                });
            if !page.stale_reasons.is_empty()
                || render_stale
                || export.manifest.migration_status
                    == crate::wiki::model::MigrationStatus::RequiresRebuild
            {
                stale_pages.push(page.page_id.clone());
            }
            for evidence in &page.evidence {
                source_owners
                    .entry(evidence.source.path.clone())
                    .or_default()
                    .insert(export.repository_id.clone());
            }
            pages.push(RealmPageRef {
                repository_id: export.repository_id.clone(),
                page_id: page.page_id.clone(),
                title: page.title.clone(),
                evidence: page.evidence.clone(),
            });
        }
        stale_pages.sort();
        if !stale_pages.is_empty() {
            diagnostics.push(format!(
                "repository {} contains {} stale canonical pages",
                export.repository_id,
                stale_pages.len()
            ));
        }
        members.push(RealmMemberStatus {
            repository_id: export.repository_id,
            manifest_hash: export.manifest_hash,
            lenses: export.manifest.lenses,
            stale_pages,
        });
    }
    pages.sort_by(|a, b| (&a.repository_id, &a.page_id).cmp(&(&b.repository_id, &b.page_id)));
    let shared_source_names = source_owners
        .into_iter()
        .filter(|(_, repositories)| repositories.len() > 1)
        .map(|(source_path, repositories)| SharedSourceRef {
            source_path,
            repositories: repositories.into_iter().collect(),
        })
        .collect();
    diagnostics.sort();
    Ok(RealmManifestV1 {
        schema_version: REALM_SCHEMA_VERSION,
        members,
        pages,
        shared_source_names,
        diagnostics,
    })
}

fn stable_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn stable_json_hash<T: Serialize>(value: &T) -> Result<String> {
    Ok(crate::repo::content_hash(&stable_json(value)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn federation_is_deterministic_and_rejects_duplicates() -> Result<()> {
        let first = RepositoryWikiExportV1::from_manifest(WikiManifestV1::empty("a", "test"))?;
        let second = RepositoryWikiExportV1::from_manifest(WikiManifestV1::empty("b", "test"))?;
        assert_eq!(
            federate(vec![second.clone(), first.clone()])?,
            federate(vec![first.clone(), second])?
        );
        assert!(federate(vec![first.clone(), first]).is_err());
        Ok(())
    }
}
