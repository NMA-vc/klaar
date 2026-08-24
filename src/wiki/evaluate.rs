use crate::wiki::model::{ClaimKind, EvidenceKind, PageKind, WikiManifestV1};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityThresholds {
    pub min_citation_validity: f64,
    pub min_evidence_support: f64,
    pub min_architecture_coverage: f64,
    pub max_stale_pages: usize,
    pub max_unsupported_executive_claims: usize,
}

impl Default for QualityThresholds {
    fn default() -> Self {
        Self {
            min_citation_validity: 1.0,
            min_evidence_support: 1.0,
            min_architecture_coverage: 1.0,
            max_stale_pages: 0,
            max_unsupported_executive_claims: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityReport {
    pub schema_version: u32,
    pub total_claims: usize,
    pub supported_claims: usize,
    pub citation_validity: f64,
    pub evidence_support: f64,
    pub architecture_coverage: f64,
    pub stale_pages: usize,
    pub unsupported_executive_claims: usize,
    pub generator_identities: Vec<String>,
    pub thresholds: QualityThresholds,
    pub passed: bool,
    pub diagnostics: Vec<String>,
}

pub fn evaluate_manifest(manifest: &WikiManifestV1) -> QualityReport {
    let mut total_claims = 0;
    let mut supported_claims = 0;
    let mut total_citations = 0;
    let mut valid_citations = 0;
    let mut unsupported_executive_claims = 0;

    for page in &manifest.canonical_pages {
        let page_evidence: BTreeSet<_> = page.evidence.iter().map(|item| &item.id).collect();
        let trusted_evidence: BTreeSet<_> = page
            .evidence
            .iter()
            .filter(|item| item.evidence_kind == EvidenceKind::TrustedDocument)
            .map(|item| &item.id)
            .collect();

        for claim in &page.structured_claims {
            total_claims += 1;
            total_citations += claim.evidence.len();
            let valid_for_page = claim
                .evidence
                .iter()
                .filter(|evidence| page_evidence.contains(evidence))
                .count();
            valid_citations += valid_for_page;
            if !claim.evidence.is_empty() && valid_for_page == claim.evidence.len() {
                supported_claims += 1;
            }
            if matches!(claim.kind, ClaimKind::Purpose | ClaimKind::Risk)
                && claim
                    .evidence
                    .iter()
                    .all(|evidence| !trusted_evidence.contains(evidence))
            {
                unsupported_executive_claims += 1;
            }
        }
    }
    let covered_kinds: BTreeSet<_> = manifest
        .canonical_pages
        .iter()
        .filter_map(|page| match page.page_kind {
            PageKind::Index => Some("index"),
            PageKind::Architecture => Some("architecture"),
            PageKind::Dependencies => Some("dependencies"),
            PageKind::Testing => Some("testing"),
            PageKind::Operations => Some("operations"),
            _ => None,
        })
        .collect();
    let stale_pages = manifest
        .canonical_pages
        .iter()
        .filter(|page| !page.stale_reasons.is_empty())
        .count();
    let ratio = |part: usize, total: usize| {
        if total == 0 {
            1.0
        } else {
            part as f64 / total as f64
        }
    };
    let mut diagnostics = Vec::new();
    if supported_claims != total_claims {
        diagnostics.push("one or more claims lack valid evidence".to_string());
    }
    if unsupported_executive_claims > 0 {
        diagnostics
            .push("executive purpose/risk claims lack trusted-document evidence".to_string());
    }
    if stale_pages > 0 {
        diagnostics.push("manifest contains stale canonical pages".to_string());
    }
    let thresholds = QualityThresholds::default();
    let mut generator_identities: Vec<_> = manifest
        .lens_renders
        .iter()
        .map(|render| {
            format!(
                "{}:{}:{}",
                render.generator.adapter,
                render.generator.generator_version,
                render.generator.deterministic
            )
        })
        .collect();
    generator_identities.sort();
    generator_identities.dedup();
    let citation_validity = ratio(valid_citations, total_citations);
    let evidence_support = ratio(supported_claims, total_claims);
    let architecture_coverage = ratio(covered_kinds.len(), 5);
    let passed = citation_validity >= thresholds.min_citation_validity
        && evidence_support >= thresholds.min_evidence_support
        && architecture_coverage >= thresholds.min_architecture_coverage
        && stale_pages <= thresholds.max_stale_pages
        && unsupported_executive_claims <= thresholds.max_unsupported_executive_claims;
    QualityReport {
        schema_version: crate::wiki::WIKI_SCHEMA_VERSION,
        total_claims,
        supported_claims,
        citation_validity,
        evidence_support,
        architecture_coverage,
        stale_pages,
        unsupported_executive_claims,
        generator_identities,
        thresholds,
        passed,
        diagnostics,
    }
}

pub fn evaluate_project(project_root: &Path) -> Result<(QualityReport, PathBuf)> {
    let root = project_root.canonicalize()?;
    let config = crate::wiki::config::WikiConfig::load(&root)?;
    let state_dir = crate::wiki::safety::validate_managed_directory(&root, &config.state_dir)?;
    let manifest = crate::wiki::WikiManifestV1::load(&state_dir.join("manifest.json"))
        .context("build the wiki before evaluating it")?;
    let report = evaluate_manifest(&manifest);
    let path = state_dir.join("quality-report.json");
    crate::wiki::state::atomic_write(&path, &crate::wiki::state::stable_json(&report)?)?;
    Ok((report, path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::content_hash;
    use crate::wiki::model::{
        CanonicalPageState, EvidenceId, EvidenceRef, PageId, SourceIdentity, SourceKind,
        SupportedClaim, WIKI_SCHEMA_VERSION,
    };

    #[test]
    fn empty_manifest_scores_deterministically() {
        let report = evaluate_manifest(&WikiManifestV1::empty("repo", "test"));
        assert_eq!(report.evidence_support, 1.0);
        assert_eq!(report.citation_validity, 1.0);
        assert_eq!(report.architecture_coverage, 0.0);
        assert!(!report.passed);
    }

    #[test]
    fn invalid_cross_page_citation_is_not_supported() {
        let valid_id = EvidenceId::new("evidence-valid").unwrap();
        let invalid_id = EvidenceId::new("evidence-other-page").unwrap();
        let mut manifest = WikiManifestV1::empty("repo", "test");
        manifest.canonical_pages.push(CanonicalPageState {
            schema_version: WIKI_SCHEMA_VERSION,
            page_id: PageId::new("architecture").unwrap(),
            page_kind: PageKind::Architecture,
            title: "Architecture".into(),
            evidence: vec![EvidenceRef {
                id: valid_id,
                source: SourceIdentity::new("src/lib.rs", SourceKind::Source).unwrap(),
                range: None,
                content_hash: content_hash(b"source"),
                evidence_kind: EvidenceKind::SourceExcerpt,
            }],
            related_pages: vec![],
            evidence_hash: content_hash(b"evidence"),
            structured_claims: vec![SupportedClaim {
                claim_id: "claim-1".into(),
                kind: ClaimKind::Fact,
                text: "A fact".into(),
                evidence: vec![invalid_id],
            }],
            content_hash: None,
            stale_reasons: vec![],
        });

        let report = evaluate_manifest(&manifest);
        assert_eq!(report.total_claims, 1);
        assert_eq!(report.supported_claims, 0);
        assert_eq!(report.citation_validity, 0.0);
        assert_eq!(report.evidence_support, 0.0);
        assert!(!report.passed);
    }
}
