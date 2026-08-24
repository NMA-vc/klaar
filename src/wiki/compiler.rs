use crate::repo::content_hash;
use crate::wiki::config::WikiConfig;
use crate::wiki::model::*;
use crate::wiki::safety::{validate_output_path, validate_page_draft, SafetyError};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPage {
    pub relative_path: RelativeWikiPath,
    pub output_path: std::path::PathBuf,
    pub content: String,
    pub content_hash: ContentHash,
}

pub fn compile_page(
    project_root: &Path,
    config: &WikiConfig,
    plan: &CanonicalPagePlan,
    draft: &PageDraft,
    allowed_evidence: &BTreeSet<EvidenceId>,
    klaar_version: &str,
    repository_id: &str,
) -> Result<CompiledPage, SafetyError> {
    validate_page_draft(
        draft,
        plan,
        allowed_evidence,
        config.budgets.max_output_bytes_per_page,
    )?;
    let filename = format!("{}/{}.md", draft.lens.as_str(), plan.output_stem.as_str());
    let output_relative = config
        .output_dir
        .as_str()
        .strip_prefix(".klaar/")
        .ok_or(SafetyError::PathOutsideProject)?;
    let relative_path = RelativeWikiPath::new(format!("{}/{}", output_relative, filename))
        .map_err(SafetyError::InvalidDraft)?;
    let output_path = validate_output_path(project_root, &relative_path)?;
    let mut output = format!("---\nschema_version: {}\npage_id: {}\nlens: {}\ntitle: {}\nrepository_id: {}\nklaar_version: {}\n---\n\n{}", WIKI_SCHEMA_VERSION, plan.page_id, draft.lens, yaml(draft.title.as_str()), yaml(repository_id), yaml(klaar_version), draft.body);
    output.push_str("\n\n## Evidence\n\n");
    for id in &draft.citations {
        if let Some(reference) = plan.evidence.iter().find(|e| e.id == *id) {
            output.push_str(&format!("- `{}`: `{}`\n", id, reference.source.path));
        }
    }
    if output.len() > config.budgets.max_output_bytes_per_page {
        return Err(SafetyError::InputTooLarge);
    }
    let hash = content_hash(output.as_bytes());
    Ok(CompiledPage {
        relative_path,
        output_path,
        content: output,
        content_hash: hash,
    })
}

pub fn compile_draft(
    project_root: &Path,
    config: &WikiConfig,
    plan: &CanonicalPagePlan,
    draft: &PageDraft,
    allowed_evidence: &BTreeSet<EvidenceId>,
    klaar_version: &str,
    repository_id: &str,
) -> Result<CompiledPage, SafetyError> {
    compile_page(
        project_root,
        config,
        plan,
        draft,
        allowed_evidence,
        klaar_version,
        repository_id,
    )
}

fn yaml(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''").replace('\n', " "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::config::WikiConfig;
    use tempfile::tempdir;
    #[test]
    fn compiler_owns_frontmatter_and_output_path() {
        let root = tempdir().unwrap();
        std::fs::create_dir(root.path().join(".klaar")).unwrap();
        let plan = CanonicalPagePlan {
            schema_version: WIKI_SCHEMA_VERSION,
            page_id: PageId::new("index").unwrap(),
            page_kind: PageKind::Index,
            title: "Index".into(),
            evidence: vec![],
            related_pages: vec![],
            output_stem: RelativeWikiPath::new("index").unwrap(),
        };
        let draft = PageDraft {
            schema_version: WIKI_SCHEMA_VERSION,
            page_id: plan.page_id.clone(),
            lens: LensId::technical(),
            title: "Index".into(),
            body: "Facts.".into(),
            claims: vec![],
            citations: vec![],
        };
        let compiled = compile_page(
            root.path(),
            &WikiConfig::default(),
            &plan,
            &draft,
            &BTreeSet::new(),
            "test",
            "repo",
        )
        .unwrap();
        assert_eq!(compiled.relative_path.as_str(), "wiki/technical/index.md");
        assert!(compiled.content.starts_with("---\nschema_version: 1"));
    }
}
