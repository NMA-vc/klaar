//! Agent memory storage with two interchangeable backends.
//!
//! * `sqlite` (default): one `klaar.sqlite3` file, FTS5 for BM25, brute-force
//!   cosine for vectors. Small, stable on-disk format, no audit churn.
//! * `surrealdb`: embedded SurrealKV with an HNSW vector index.
//!
//! Both are compiled when both features are on; `KLAAR_DB_BACKEND=sqlite|surrealdb`
//! picks one at runtime (default: sqlite when built, else surrealdb). With neither
//! feature, [`init`] returns an error and nothing else can be reached. The two
//! backends do not share on-disk data.

#![cfg_attr(
    not(any(feature = "sqlite", feature = "surrealdb")),
    allow(unused_variables, unused_imports, dead_code)
)]

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[cfg(feature = "sqlite")]
mod sqlite;
#[cfg(feature = "surrealdb")]
mod surreal;

// ---------------------------------------------------------------------------
// Record types (backend independent)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryRecord {
    pub key: String,
    pub content: String,
    pub tags: Vec<String>,
    pub project: String,
    pub created_at: DateTime<Utc>,
    pub agent_id: Option<String>,
    pub token_saved: Option<usize>,
    pub token_usage: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StatsRow {
    pub agent_id: Option<String>,
    pub project: String,
    pub total_saved: usize,
    pub total_usage: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileLock {
    pub file_path: String,
    pub agent_id: String,
    pub virtual_branch: String,
    pub locked_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DecisionRecord {
    /// Opaque backend id; pass it back to [`confirm_decision`] unchanged.
    pub id: String,
    pub project: String,
    pub decision: String,
    pub context: String,
    pub governed_files: Vec<String>,
    pub confidence: f32,
    pub edit_count: usize,
    pub created_at: DateTime<Utc>,
    pub last_confirmed: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(dead_code)]
pub struct CoChangeRecord {
    pub id: String,
    pub project: String,
    pub files: Vec<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Handle and dispatch
// ---------------------------------------------------------------------------

/// A ready memory store. Cheap to clone.
#[derive(Clone)]
pub enum KlaarDb {
    #[cfg(feature = "sqlite")]
    Sqlite(sqlite::Store),
    #[cfg(feature = "surrealdb")]
    Surreal(surreal::Store),
}

/// Run `$e` against whichever backend `$db` holds. Matches `*$db` so that a build
/// with no backend (an uninhabited enum) still compiles.
macro_rules! on {
    ($db:expr, $s:ident => $e:expr) => {
        match *$db {
            #[cfg(feature = "sqlite")]
            KlaarDb::Sqlite(ref $s) => $e,
            #[cfg(feature = "surrealdb")]
            KlaarDb::Surreal(ref $s) => $e,
        }
    };
}

/// Open (creating if needed) the store under `db_path`.
pub async fn init(db_path: &Path) -> Result<KlaarDb> {
    let wanted = std::env::var("KLAAR_DB_BACKEND").unwrap_or_default();
    open(db_path, wanted.trim().to_ascii_lowercase().as_str()).await
}

async fn open(db_path: &Path, wanted: &str) -> Result<KlaarDb> {
    match wanted {
        #[cfg(feature = "sqlite")]
        "" | "sqlite" => Ok(KlaarDb::Sqlite(sqlite::Store::open(db_path)?)),
        #[cfg(all(feature = "surrealdb", not(feature = "sqlite")))]
        "" | "surrealdb" => Ok(KlaarDb::Surreal(surreal::Store::open(db_path).await?)),
        #[cfg(all(feature = "surrealdb", feature = "sqlite"))]
        "surrealdb" => Ok(KlaarDb::Surreal(surreal::Store::open(db_path).await?)),
        other => bail!(
            "memory backend {:?} is not available in this build (built with: {})",
            other,
            built_backends()
        ),
    }
}

fn built_backends() -> &'static str {
    match (cfg!(feature = "sqlite"), cfg!(feature = "surrealdb")) {
        (true, true) => "sqlite, surrealdb",
        (true, false) => "sqlite",
        (false, true) => "surrealdb",
        (false, false) => "none",
    }
}

/// Insert or update a memory record.
/// `embedding` is `Some(vec)` when the embedder is available, `None` otherwise.
#[allow(clippy::too_many_arguments)]
pub async fn insert_memory(
    db: &KlaarDb,
    key: String,
    content: String,
    tags: Vec<String>,
    project: String,
    agent_id: Option<String>,
    token_saved: Option<usize>,
    token_usage: Option<usize>,
    embedding: Option<Vec<f32>>,
) -> Result<()> {
    on!(db, s => s.insert_memory(key, content, tags, project, agent_id, token_saved, token_usage, embedding).await)
}

/// Search memories for a project: vector and BM25 candidates fused with
/// reciprocal rank fusion when `query_embedding` is given, pure BM25 otherwise.
pub async fn search_memories(
    db: &KlaarDb,
    bm25_query: String,
    project: String,
    limit: u32,
    query_embedding: Option<Vec<f32>>,
) -> Result<Vec<MemoryRecord>> {
    on!(db, s => s.search_memories(bm25_query, project, limit, query_embedding).await)
}

/// Up to `limit` memory records, optionally for one project (for `klaar dump`).
pub async fn list_memories(
    db: &KlaarDb,
    project: Option<String>,
    limit: u32,
) -> Result<Vec<MemoryRecord>> {
    on!(db, s => s.list_memories(project, limit).await)
}

/// Attempt to acquire a lock on a file for an agent. Fails if the file is already locked.
pub async fn lock_file(
    db: &KlaarDb,
    file_path: String,
    agent_id: String,
    virtual_branch: String,
) -> Result<()> {
    on!(db, s => s.lock_file(file_path, agent_id, virtual_branch).await)
}

/// Release a lock gracefully.
pub async fn unlock_file(db: &KlaarDb, file_path: String, agent_id: String) -> Result<()> {
    on!(db, s => s.unlock_file(file_path, agent_id).await)
}

/// Check if a lock exists and apply automatic expiration purges (>30 min ttl).
pub async fn check_lock(db: &KlaarDb, file_path: String) -> Result<Option<FileLock>> {
    on!(db, s => s.check_lock(file_path).await)
}

/// Retrieve aggregated ROI/Token statistics.
pub async fn get_stats(
    db: &KlaarDb,
    time_limit: Option<DateTime<Utc>>,
    target_project: Option<String>,
) -> Result<Vec<StatsRow>> {
    on!(db, s => s.get_stats(time_limit, target_project).await)
}

pub async fn insert_decision(
    db: &KlaarDb,
    project: &str,
    decision: &str,
    context: &str,
    files: Vec<String>,
) -> Result<()> {
    on!(db, s => s.insert_decision(project, decision, context, files).await)
}

pub async fn get_decisions_for_files(
    db: &KlaarDb,
    project: &str,
    files: &[String],
) -> Result<Vec<DecisionRecord>> {
    on!(db, s => s.get_decisions_for_files(project, files).await)
}

pub async fn confirm_decision(db: &KlaarDb, decision_id: &str, project: &str) -> Result<()> {
    on!(db, s => s.confirm_decision(decision_id, project).await)
}

pub async fn increment_file_edits(db: &KlaarDb, file_path: &str) -> Result<()> {
    on!(db, s => s.increment_file_edits(file_path).await)
}

pub async fn record_co_changes(db: &KlaarDb, project: &str, files: Vec<String>) -> Result<()> {
    on!(db, s => s.record_co_changes(project, files).await)
}

pub async fn get_co_changes(db: &KlaarDb, project: &str) -> Result<Vec<CoChangeRecord>> {
    on!(db, s => s.get_co_changes(project).await)
}

/// R6-L2: Purge stale co_changes and ROI memory entries across all projects.
pub async fn purge_stale_entries(db: &KlaarDb) -> Result<()> {
    on!(db, s => s.purge_stale_entries().await)
}

// ---------------------------------------------------------------------------
// Shared logic
// ---------------------------------------------------------------------------

/// Reciprocal Rank Fusion of two ranked lists, K = 60, deduplicated by `key`.
#[allow(dead_code)]
pub(crate) fn fuse(
    vector: Vec<MemoryRecord>,
    lexical: Vec<MemoryRecord>,
    limit: usize,
) -> Vec<MemoryRecord> {
    const K: f64 = 60.0;
    let mut scores: HashMap<String, (f64, MemoryRecord)> = HashMap::new();
    for list in [vector, lexical] {
        for (rank, record) in list.into_iter().enumerate() {
            let score = 1.0 / (K + rank as f64);
            scores
                .entry(record.key.clone())
                .and_modify(|(s, _)| *s += score)
                .or_insert((score, record));
        }
    }
    let mut fused: Vec<(f64, MemoryRecord)> = scores.into_values().collect();
    // Tie-break on key so equal scores give a stable order.
    fused.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.key.cmp(&b.1.key))
    });
    fused.into_iter().take(limit).map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Every backend this build contains, opened on its own fresh directory.
    async fn stores() -> Vec<(&'static str, KlaarDb, tempfile::TempDir)> {
        let mut out = Vec::new();
        for name in ["sqlite", "surrealdb"] {
            let dir = tempdir().unwrap();
            match open(dir.path(), name).await {
                Ok(db) => out.push((name, db, dir)),
                Err(e) => eprintln!("backend {name} did not open: {e:#}"),
            }
        }
        out
    }

    #[tokio::test]
    async fn every_built_backend_opens_and_is_tested() {
        let built =
            usize::from(cfg!(feature = "sqlite")) + usize::from(cfg!(feature = "surrealdb"));
        assert_eq!(stores().await.len(), built);
    }

    #[tokio::test]
    async fn unknown_backend_is_refused() {
        let dir = tempdir().unwrap();
        assert!(open(dir.path(), "postgres").await.is_err());
    }

    #[tokio::test]
    async fn locking_and_roi_memory() -> Result<()> {
        for (name, db, _dir) in stores().await {
            insert_memory(
                &db,
                "test_roi_memory".into(),
                "Optimized some functions".into(),
                vec!["test".into()],
                "test_project".into(),
                Some("agent_omega".into()),
                Some(250),
                None,
                None,
            )
            .await?;
            let stats = get_stats(&db, None, Some("test_project".into())).await?;
            assert_eq!(stats.len(), 1, "{name}");
            assert_eq!(stats[0].agent_id.as_deref(), Some("agent_omega"), "{name}");
            assert_eq!(stats[0].total_saved, 250, "{name}");

            let f = "src/main.rs".to_string();
            lock_file(&db, f.clone(), "agent_A".into(), "b1".into()).await?;
            assert_eq!(
                check_lock(&db, f.clone()).await?.unwrap().agent_id,
                "agent_A",
                "{name}"
            );
            assert!(
                lock_file(&db, f.clone(), "agent_B".into(), "b1".into())
                    .await
                    .is_err(),
                "{name}: a second lock on the same file must fail"
            );
            // Only the owner can release it.
            unlock_file(&db, f.clone(), "agent_B".into()).await?;
            assert!(check_lock(&db, f.clone()).await?.is_some(), "{name}");
            unlock_file(&db, f.clone(), "agent_A".into()).await?;
            assert!(check_lock(&db, f).await?.is_none(), "{name}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn decision_intelligence() -> Result<()> {
        for (name, db, _dir) in stores().await {
            let project = "p";
            let files = vec!["src/db/mod.rs".to_string(), "src/main.rs".to_string()];
            insert_decision(
                &db,
                project,
                "Use SQLite memory layer",
                "small, stable",
                files,
            )
            .await?;
            // Upsert on (project, decision) does not duplicate.
            insert_decision(
                &db,
                project,
                "Use SQLite memory layer",
                "small, stable",
                vec!["src/main.rs".into()],
            )
            .await?;
            let main = ["src/main.rs".to_string()];
            let d = get_decisions_for_files(&db, project, &main).await?;
            assert_eq!(d.len(), 1, "{name}");
            assert_eq!((d[0].confidence, d[0].edit_count), (1.0, 0), "{name}");
            assert!(
                get_decisions_for_files(&db, "other", &main)
                    .await?
                    .is_empty(),
                "{name}"
            );

            increment_file_edits(&db, "src/main.rs").await?;
            let d = get_decisions_for_files(&db, project, &main).await?;
            assert_eq!(d[0].edit_count, 1, "{name}");
            assert!((d[0].confidence - 0.9).abs() < 0.001, "{name}");
            for _ in 0..15 {
                increment_file_edits(&db, "src/main.rs").await?;
            }
            let d = get_decisions_for_files(&db, project, &main).await?;
            assert!(
                (d[0].confidence - 0.1).abs() < 0.001,
                "{name}: floor is 0.1"
            );

            // Another project cannot confirm it.
            confirm_decision(&db, &d[0].id, "other").await?;
            assert!(
                get_decisions_for_files(&db, project, &main).await?[0].confidence < 0.2,
                "{name}"
            );
            confirm_decision(&db, &d[0].id, project).await?;
            let d = get_decisions_for_files(&db, project, &main).await?;
            assert_eq!((d[0].confidence, d[0].edit_count), (1.0, 0), "{name}");

            record_co_changes(&db, project, vec!["a.rs".into(), "b.rs".into()]).await?;
            assert_eq!(get_co_changes(&db, project).await?.len(), 1, "{name}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn hybrid_search_fuses_lexical_and_vector_hits() -> Result<()> {
        for (name, db, _dir) in stores().await {
            let mut e1 = vec![0.0; 384];
            e1[0] = 1.0;
            let mut e2 = vec![0.0; 384];
            e2[1] = 1.0;
            for (key, text, emb) in [
                ("rec1", "Reciprocal Rank Fusion is awesome", e1),
                ("rec2", "Some random content here", e2.clone()),
            ] {
                insert_memory(
                    &db,
                    key.into(),
                    text.into(),
                    vec![],
                    "h".into(),
                    None,
                    None,
                    None,
                    Some(emb),
                )
                .await?;
            }
            let hits = search_memories(&db, "Reciprocal".into(), "h".into(), 2, Some(e2)).await?;
            let keys: Vec<_> = hits.iter().map(|r| r.key.as_str()).collect();
            assert!(
                keys.contains(&"rec1") && keys.contains(&"rec2"),
                "{name}: {keys:?}"
            );

            // Lexical only: finds the match, and a query of punctuation does not error.
            let hits = search_memories(&db, "reciprocal".into(), "h".into(), 5, None).await?;
            assert_eq!(hits.len(), 1, "{name}");
            search_memories(&db, "\"(*)\" OR AND".into(), "h".into(), 5, None).await?;

            // ROI entries are never recalled.
            insert_memory(
                &db,
                "roi1".into(),
                "Reciprocal roi".into(),
                vec!["roi".into()],
                "h".into(),
                None,
                Some(1),
                None,
                None,
            )
            .await?;
            let hits = search_memories(&db, "reciprocal".into(), "h".into(), 5, None).await?;
            assert!(hits.iter().all(|r| r.key != "roi1"), "{name}");
        }
        Ok(())
    }
}
