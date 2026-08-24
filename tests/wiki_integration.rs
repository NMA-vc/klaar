use anyhow::Result;
use klaar::repo::RepositoryFacts;
use klaar::wiki;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/wiki")
        .join(name)
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = if relative
            .file_name()
            .is_some_and(|name| name == "Cargo.toml.fixture")
        {
            target.join(relative.with_file_name("Cargo.toml"))
        } else {
            target.join(relative)
        };
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&destination)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

fn read_managed_files(root: &Path) -> Result<String> {
    let mut content = String::new();
    for entry in walkdir::WalkDir::new(root.join(".klaar")).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            content.push_str(&String::from_utf8_lossy(&std::fs::read(entry.path())?));
        }
    }
    Ok(content)
}

#[tokio::test]
async fn supported_language_fixtures_build_and_noop() -> Result<()> {
    for name in [
        "rust-basic",
        "typescript-basic",
        "python-basic",
        "go-basic",
        "mixed-monorepo",
    ] {
        let root = tempfile::tempdir()?;
        copy_tree(&fixture(name), root.path())?;
        wiki::build::init(root.path())?;

        let first = wiki::build::build(root.path(), None).await?;
        assert!(first.planned_pages > 0, "fixture {name} planned no pages");
        assert!(first.generator_calls > 0, "fixture {name} did not render");

        let manifest = std::fs::read(&first.manifest_path)?;
        let second = wiki::build::build(root.path(), None).await?;
        assert_eq!(second.generator_calls, 0, "fixture {name} was not a no-op");
        assert_eq!(manifest, std::fs::read(&second.manifest_path)?);
        assert!(wiki::check::status(root.path())?.current);
    }
    Ok(())
}

#[tokio::test]
async fn no_trusted_docs_still_builds_without_business_claims() -> Result<()> {
    let root = tempfile::tempdir()?;
    copy_tree(&fixture("no-trusted-docs"), root.path())?;
    let report = wiki::build::build(root.path(), None).await?;
    let manifest = wiki::WikiManifestV1::load(&report.manifest_path)?;
    let quality = wiki::evaluate::evaluate_manifest(&manifest);
    assert_eq!(quality.unsupported_executive_claims, 0);
    assert_eq!(quality.evidence_support, 1.0);
    Ok(())
}

#[tokio::test]
async fn shared_dependency_change_has_bounded_explainable_fanout() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = fixture("shared-dependency");
    copy_tree(&source.join("baseline"), root.path())?;
    wiki::build::init(root.path())?;
    let first = wiki::build::build(root.path(), None).await?;
    std::fs::remove_dir_all(root.path().join("src"))?;
    copy_tree(&source.join("updated"), root.path())?;
    let second = wiki::build::build(root.path(), None).await?;
    assert!(second.rendered_pages > 0);
    assert!(second.rendered_pages < first.rendered_pages);
    assert!(second.reused_pages > 0);
    Ok(())
}

#[tokio::test]
async fn adversarial_secrets_never_persist_in_managed_artifacts() -> Result<()> {
    let root = tempfile::tempdir()?;
    copy_tree(&fixture("adversarial-secrets"), root.path())?;
    wiki::build::init(root.path())?;
    wiki::build::build(root.path(), None).await?;

    let artifacts = read_managed_files(root.path())?.to_ascii_lowercase();
    assert!(!artifacts.contains("fake_test_token_do_not_use_12345"));
    assert!(!artifacts.contains("fake_fixture_token_000000"));
    assert!(!artifacts.contains("fake private key marker only"));
    Ok(())
}

#[test]
fn generated_and_vendor_directories_are_excluded_from_facts() -> Result<()> {
    let facts = RepositoryFacts::scan(&fixture("generated-vendor-heavy"))?;
    assert!(facts
        .files
        .iter()
        .all(|file| !file.path.starts_with("generated/") && !file.path.starts_with("vendor/")));
    assert!(facts.files.iter().any(|file| file.path == "src/real.ts"));
    Ok(())
}

#[tokio::test]
async fn rename_delete_fixture_rebuilds_without_orphans() -> Result<()> {
    let root = tempfile::tempdir()?;
    let fixture_root = fixture("rename-delete");
    copy_tree(&fixture_root.join("baseline"), root.path())?;
    wiki::build::init(root.path())?;
    wiki::build::build(root.path(), None).await?;

    std::fs::remove_dir_all(root.path().join("src"))?;
    std::fs::remove_file(root.path().join("Cargo.toml"))?;
    copy_tree(&fixture_root.join("updated"), root.path())?;
    assert!(!wiki::check::status(root.path())?.current);

    wiki::build::build(root.path(), None).await?;
    assert!(wiki::check::status(root.path())?.current);
    let config = wiki::WikiConfig::load(root.path())?;
    let manifest = wiki::WikiManifestV1::load(
        &config
            .resolve_dir(root.path(), &config.state_dir)?
            .join("manifest.json"),
    )?;
    let output = config.resolve_dir(root.path(), &config.output_dir)?;
    let markdown_files = walkdir::WalkDir::new(output)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
        .count();
    assert_eq!(markdown_files, manifest.lens_renders.len());
    Ok(())
}
