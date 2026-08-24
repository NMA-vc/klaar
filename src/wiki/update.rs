use crate::wiki::build::{build_with_generator, BuildReport};
use crate::wiki::check::{affected_pages, StalePage};
use crate::wiki::config::WikiConfig;
use crate::wiki::generator::WikiGenerator;
use crate::wiki::model::WikiManifestV1;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub base: String,
    pub head: String,
    pub affected_pages: Vec<StalePage>,
    pub build: BuildReport,
}

pub async fn update_with_generator(
    project_root: &Path,
    base: &str,
    head: &str,
    generator: &dyn WikiGenerator,
) -> Result<UpdateReport> {
    let delta = crate::wiki::git::compute_delta(project_root, base, head).await?;
    let config = WikiConfig::load(project_root)?;
    let manifest_path =
        crate::wiki::safety::validate_managed_directory(project_root, &config.state_dir)?
            .join("manifest.json");
    let affected = WikiManifestV1::load(&manifest_path)
        .map(|manifest| affected_pages(&manifest, &delta))
        .unwrap_or_default();
    // The shared build engine performs evidence-hash no-op detection, so unaffected
    // pages remain byte-identical and never invoke the generator.
    let build = build_with_generator(project_root, None, generator).await?;
    Ok(UpdateReport {
        base: base.to_string(),
        head: head.to_string(),
        affected_pages: affected,
        build,
    })
}

pub async fn update(project_root: &Path, base: &str, head: &str) -> Result<UpdateReport> {
    update_with_generator(
        project_root,
        base,
        head,
        &crate::wiki::generator::FactRenderer,
    )
    .await
}
