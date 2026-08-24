use criterion::{black_box, criterion_group, criterion_main, Criterion};

use klaar::wiki::git::{ChangeKind, GitDelta, PathChange};
use klaar::wiki::{
    ensure_no_secrets, validate_markdown, RelativeWikiPath, WikiConfig, WikiManifestV1,
};

fn bench_wiki_contracts(c: &mut Criterion) {
    let config_toml = r#"
[wiki]
enabled = true
lenses = ["technical", "executive"]

[wiki.documents]
trusted = ["README.md", "docs/**/*.md"]
exclude = ["**/.env*"]
"#;
    c.bench_function("wiki_config_parse_and_validate", |b| {
        b.iter(|| {
            let config = WikiConfig::from_toml_str(black_box(config_toml)).unwrap();
            black_box(config)
        });
    });

    let paths = [
        "technical/index.md",
        "technical/architecture.md",
        "technical/./dependencies.md",
        "technical/components/api.md",
    ];
    c.bench_function("wiki_relative_path_normalization", |b| {
        b.iter(|| {
            let normalized: Vec<_> = paths
                .iter()
                .map(|path| RelativeWikiPath::new(black_box(path)).unwrap())
                .collect();
            black_box(normalized)
        });
    });

    let markdown =
        "# Architecture\n\nThe parser reads source files.\n\n[Source](https://example.com/docs).\n";
    c.bench_function("wiki_markdown_safety_validation", |b| {
        b.iter(|| {
            validate_markdown(black_box(markdown), 32 * 1024).unwrap();
        });
    });

    let clean_evidence = "source excerpt: parser module\nrelationship: parser -> facts\n";
    c.bench_function("wiki_secret_marker_scan_clean", |b| {
        b.iter(|| {
            ensure_no_secrets(black_box(clean_evidence)).unwrap();
        });
    });

    c.bench_function("wiki_manifest_serialization", |b| {
        b.iter(|| {
            let manifest = WikiManifestV1::empty("fixture-repository", "1.0.0");
            let encoded = serde_json::to_vec(black_box(&manifest)).unwrap();
            black_box(encoded)
        });
    });

    let delta = GitDelta {
        base: "base-revision".to_string(),
        head: "head-revision".to_string(),
        changes: vec![
            PathChange {
                kind: ChangeKind::Modified,
                path: "src/zeta.rs".to_string(),
                previous_path: None,
            },
            PathChange {
                kind: ChangeKind::Renamed,
                path: "src/new.rs".to_string(),
                previous_path: Some("src/old.rs".to_string()),
            },
            PathChange {
                kind: ChangeKind::Added,
                path: "README.md".to_string(),
                previous_path: None,
            },
        ],
    };
    c.bench_function("wiki_git_affected_paths_sorted", |b| {
        b.iter(|| {
            let paths = delta.affected_paths();
            black_box(paths)
        });
    });

    let repository = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repository.path().join("src")).unwrap();
    std::fs::write(
        repository.path().join("src/lib.rs"),
        "pub mod service;\npub fn run() { service::execute(); }\n",
    )
    .unwrap();
    std::fs::write(
        repository.path().join("src/service.rs"),
        "pub fn execute() {}\n",
    )
    .unwrap();
    std::fs::write(repository.path().join("README.md"), "# Fixture\n").unwrap();
    klaar::wiki::build::init(repository.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(klaar::wiki::build::build(repository.path(), None))
        .unwrap();

    c.bench_function("wiki_repository_fact_scan_warm", |b| {
        b.iter(|| {
            black_box(klaar::repo::RepositoryFacts::scan(black_box(repository.path())).unwrap())
        });
    });

    c.bench_function("wiki_noop_build_warm", |b| {
        b.iter(|| {
            let report = runtime
                .block_on(klaar::wiki::build::build(
                    black_box(repository.path()),
                    None,
                ))
                .unwrap();
            assert_eq!(report.generator_calls, 0);
            black_box(report)
        });
    });
}

criterion_group!(benches, bench_wiki_contracts);
criterion_main!(benches);
