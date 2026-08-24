use crate::repo::{self, RepositoryFacts};
use crate::wiki::config::WikiConfig;
use crate::wiki::model::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningError(pub String);

impl std::fmt::Display for PlanningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PlanningError {}

/// Builds the complete, sorted information architecture from facts only.
pub fn plan_pages(
    facts: &RepositoryFacts,
    config: &WikiConfig,
) -> Result<Vec<CanonicalPagePlan>, PlanningError> {
    config
        .validate()
        .map_err(|e| PlanningError(e.to_string()))?;
    let mut pages = Vec::new();
    let all = refs(facts.files.iter().map(|f| &f.evidence))
        .chain(refs(facts.manifests.iter().map(|f| &f.evidence)))
        .chain(refs(facts.trusted_docs.iter().map(|f| &f.evidence)))
        .collect::<Vec<_>>();
    pages.push(page("index", PageKind::Index, "Repository", all, "index"));
    if !facts.files.is_empty() || !facts.components.is_empty() {
        pages.push(page(
            "architecture",
            PageKind::Architecture,
            "Architecture",
            refs(facts.files.iter().map(|f| &f.evidence))
                .chain(refs(facts.dependencies.iter().map(|f| &f.evidence)))
                .collect(),
            "architecture",
        ));
    }
    if !facts.dependencies.is_empty() {
        pages.push(page(
            "dependencies",
            PageKind::Dependencies,
            "Dependencies",
            refs(facts.dependencies.iter().map(|f| &f.evidence)).collect(),
            "dependencies",
        ));
    }
    if facts
        .manifests
        .iter()
        .any(|m| m.commands.iter().any(|c| c.contains("test")))
    {
        pages.push(page(
            "testing",
            PageKind::Testing,
            "Testing",
            refs(facts.manifests.iter().map(|f| &f.evidence)).collect(),
            "testing",
        ));
    }
    if !facts.manifests.is_empty() {
        pages.push(page(
            "operations",
            PageKind::Operations,
            "Operations",
            refs(facts.manifests.iter().map(|f| &f.evidence)).collect(),
            "operations",
        ));
    }
    if !facts.trusted_docs.is_empty() {
        pages.push(page(
            "domain",
            PageKind::Domain,
            "Project Context",
            refs(facts.trusted_docs.iter().map(|f| &f.evidence)).collect(),
            "domain",
        ));
    }
    for component in &facts.components {
        let id = PageId::new(format!(
            "component-{}",
            &repo::content_hash(component.name.as_bytes())[..16]
        ))
        .unwrap();
        let evidence = facts
            .files
            .iter()
            .filter(|f| component.file_ids.contains(&f.id))
            .map(|f| &f.evidence);
        pages.push(CanonicalPagePlan {
            schema_version: WIKI_SCHEMA_VERSION,
            page_id: id,
            page_kind: PageKind::Component,
            title: component.name.clone(),
            evidence: refs(evidence).collect(),
            related_pages: Vec::new(),
            output_stem: RelativeWikiPath::new(format!(
                "components/{}",
                component_slug(&component.name)
            ))
            .map_err(PlanningError)?,
        });
    }
    pages.sort_by(|a, b| a.page_id.cmp(&b.page_id));
    if pages.len() > config.budgets.max_pages {
        return Err(PlanningError(
            "page plan exceeds configured page budget".to_string(),
        ));
    }
    let ids: Vec<_> = pages.iter().map(|p| p.page_id.clone()).collect();
    for page in &mut pages {
        page.related_pages = ids
            .iter()
            .filter(|id| **id != page.page_id)
            .cloned()
            .collect();
    }
    Ok(pages)
}

fn component_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut separator = false;
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.') {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push((byte as char).to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    let slug = slug.trim_matches('-');
    let hash = repo::content_hash(name.as_bytes());
    if slug.is_empty() {
        format!("component-{}", &hash[..16])
    } else {
        format!("{}-{}", slug, &hash[..8])
    }
}

pub fn plan_canonical_pages(
    facts: &RepositoryFacts,
    config: &WikiConfig,
) -> Result<Vec<CanonicalPagePlan>, PlanningError> {
    plan_pages(facts, config)
}

fn refs<'a, I>(items: I) -> impl Iterator<Item = EvidenceRef> + 'a
where
    I: Iterator<Item = &'a repo::EvidenceRef> + 'a,
{
    items.map(to_wiki_ref)
}

fn to_wiki_ref(value: &repo::EvidenceRef) -> EvidenceRef {
    let kind = match value.evidence_kind {
        repo::EvidenceKind::File => EvidenceKind::SourceExcerpt,
        repo::EvidenceKind::Symbol => EvidenceKind::Symbol,
        repo::EvidenceKind::Dependency => EvidenceKind::Dependency,
        repo::EvidenceKind::Manifest => EvidenceKind::Manifest,
        repo::EvidenceKind::TrustedDocument => EvidenceKind::TrustedDocument,
    };
    let source_kind = match kind {
        EvidenceKind::Manifest => SourceKind::Manifest,
        EvidenceKind::TrustedDocument => SourceKind::TrustedDocument,
        EvidenceKind::Symbol => SourceKind::Symbol,
        EvidenceKind::Dependency => SourceKind::Dependency,
        _ => SourceKind::Source,
    };
    EvidenceRef {
        id: EvidenceId::new(value.id.clone()).expect("repository evidence IDs are valid"),
        source: SourceIdentity::new(&value.source, source_kind)
            .expect("repository paths are normalized"),
        range: value.range.as_ref().map(|r| SourceRange {
            start_line: r.start_line as u32,
            start_column: r.start_column as u32,
            end_line: r.end_line as u32,
            end_column: r.end_column as u32,
        }),
        content_hash: value.content_hash.clone(),
        evidence_kind: kind,
    }
}

fn page(
    id: &str,
    kind: PageKind,
    title: &str,
    evidence: Vec<EvidenceRef>,
    stem: &str,
) -> CanonicalPagePlan {
    let mut evidence = evidence;
    evidence.sort_by(|a, b| a.id.cmp(&b.id));
    evidence.dedup_by(|a, b| a.id == b.id);
    CanonicalPagePlan {
        schema_version: WIKI_SCHEMA_VERSION,
        page_id: PageId::new(id).unwrap(),
        page_kind: kind,
        title: title.to_string(),
        evidence,
        related_pages: Vec::new(),
        output_stem: RelativeWikiPath::new(stem).unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_facts_have_one_canonical_page() {
        let pages = plan_pages(&RepositoryFacts::default(), &WikiConfig::default()).unwrap();
        assert_eq!(
            pages.iter().map(|p| p.page_id.as_str()).collect::<Vec<_>>(),
            vec!["index"]
        );
    }

    #[test]
    fn component_paths_are_portable() {
        assert!(component_slug("packages/@scope").starts_with("packages-scope-"));
        assert!(component_slug("component-name").starts_with("component-name-"));
        assert_ne!(component_slug("a@b"), component_slug("a-b"));
        assert!(component_slug("\u{65e5}\u{672c}\u{8a9e}").starts_with("component-"));
    }
}
