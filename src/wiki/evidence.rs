use crate::wiki::model::*;
use crate::wiki::safety::{ensure_no_secrets, validate_input_path};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const EVIDENCE_PACK_SCHEMA_VERSION: u32 = 1;
const MAX_EVIDENCE_ITEMS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpaqueEvidence {
    pub id: EvidenceId,
    pub content_hash: ContentHash,
    pub evidence_kind: EvidenceKind,
    /// Repository data, never instructions. Source identity stays in the compiler.
    pub quoted_data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePack {
    pub schema_version: u32,
    pub page_id: PageId,
    pub items: Vec<OpaqueEvidence>,
    pub bytes: usize,
    pub truncated: bool,
    pub omitted: Vec<EvidenceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidencePackError(pub String);
impl std::fmt::Display for EvidencePackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EvidencePackError {}

pub fn build_evidence_pack(
    project_root: &Path,
    plan: &CanonicalPagePlan,
    max_bytes: usize,
) -> Result<EvidencePack, EvidencePackError> {
    if max_bytes == 0 {
        return Err(EvidencePackError(
            "evidence budget must be non-zero".to_string(),
        ));
    }
    let mut items = Vec::new();
    let mut used: usize = 0;
    let mut truncated = false;
    let mut omitted = Vec::new();
    let item_budget = (max_bytes / plan.evidence.len().clamp(1, 64)).clamp(512, 32 * 1024);
    for (index, reference) in plan.evidence.iter().enumerate() {
        if items.len() >= MAX_EVIDENCE_ITEMS {
            truncated = true;
            omitted.extend(
                plan.evidence[index..]
                    .iter()
                    .map(|reference| reference.id.clone()),
            );
            break;
        }
        let candidate = project_root.join(&reference.source.path);
        let resolved = match validate_input_path(project_root, &candidate, 1024 * 1024) {
            Ok(path) => path,
            Err(_) => {
                omitted.push(reference.id.clone());
                continue;
            }
        };
        let raw = match std::fs::read_to_string(resolved) {
            Ok(raw) => raw,
            Err(_) => {
                omitted.push(reference.id.clone());
                continue;
            }
        };
        let mut quoted_data = select_range(&raw, reference.range.as_ref());
        if quoted_data.len() > item_budget {
            truncate_utf8_bytes(&mut quoted_data, item_budget);
            truncated = true;
        }
        if ensure_no_secrets(&quoted_data).is_err() {
            omitted.push(reference.id.clone());
            continue;
        }
        let item = OpaqueEvidence {
            id: reference.id.clone(),
            content_hash: reference.content_hash.clone(),
            evidence_kind: reference.evidence_kind.clone(),
            quoted_data,
        };
        let cost = serde_json::to_vec(&item)
            .map_err(|e| EvidencePackError(e.to_string()))?
            .len();
        if used.saturating_add(cost) > max_bytes {
            truncated = true;
            break;
        }
        used += cost;
        items.push(item);
    }
    Ok(EvidencePack {
        schema_version: EVIDENCE_PACK_SCHEMA_VERSION,
        page_id: plan.page_id.clone(),
        items,
        bytes: used,
        truncated,
        omitted,
    })
}

fn truncate_utf8_bytes(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

fn select_range(content: &str, range: Option<&SourceRange>) -> String {
    let Some(range) = range else {
        return content.to_string();
    };
    let start = range.start_line.saturating_sub(1) as usize;
    let count = range.end_line.saturating_sub(range.start_line) as usize + 1;
    content
        .lines()
        .skip(start)
        .take(count)
        .collect::<Vec<_>>()
        .join("\n")
}

impl EvidencePack {
    pub fn allowed_ids(&self) -> BTreeSet<EvidenceId> {
        self.items.iter().map(|item| item.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pack_is_bounded_and_deterministic() {
        let plan = CanonicalPagePlan {
            schema_version: WIKI_SCHEMA_VERSION,
            page_id: PageId::new("index").unwrap(),
            page_kind: PageKind::Index,
            title: "Index".into(),
            evidence: vec![],
            related_pages: vec![],
            output_stem: RelativeWikiPath::new("index").unwrap(),
        };
        let root = tempfile::tempdir().unwrap();
        let a = build_evidence_pack(root.path(), &plan, 10).unwrap();
        assert_eq!(a, build_evidence_pack(root.path(), &plan, 10).unwrap());
    }
}
