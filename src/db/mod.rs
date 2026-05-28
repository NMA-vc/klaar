use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::Surreal;
use tracing::info;

// ---------------------------------------------------------------------------
// Record types
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
    pub id: surrealdb::sql::Thing,
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
    pub id: surrealdb::sql::Thing,
    pub project: String,
    pub files: Vec<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Database handle (cheap to clone — Arc-wrapped internally by SurrealDB)
// ---------------------------------------------------------------------------

pub type KlaarDb = Surreal<Db>;

/// Initialise the embedded SurrealDB, apply schema, return a ready handle.
pub async fn init(db_path: &Path) -> Result<KlaarDb> {
    std::fs::create_dir_all(db_path)?;

    let db = Surreal::new::<SurrealKv>(db_path.to_string_lossy().as_ref()).await?;
    db.use_ns("klaar").use_db("memory").await?;

    // L1: Check the database schema version to execute versioned migrations
    let mut version = 0;
    if let Ok(mut res) = db.query("SELECT version FROM meta:version").await {
        if let Ok(Some(row)) = res.take::<Option<serde_json::Value>>(0) {
            if let Some(v) = row["version"].as_i64() {
                version = v;
            }
        }
    }

    if version < 2 {
        // Run migration: deduplicate any pre-existing decisions to prevent unique index upgrade crashes
        if let Ok(mut select_res) = db.query("SELECT * FROM decisions").await {
            if let Ok(decisions) = select_res.take::<Vec<DecisionRecord>>(0) {
                use std::collections::HashMap;
                let mut groups: HashMap<(String, String), Vec<DecisionRecord>> = HashMap::new();
                for dec in decisions {
                    groups.entry((dec.project.clone(), dec.decision.clone())).or_default().push(dec);
                }
                for ((proj, dec_name), mut group) in groups {
                    if group.len() > 1 {
                        // Sort descending by last_confirmed, keeping the newest
                        group.sort_by_key(|b| std::cmp::Reverse(b.last_confirmed));
                        // Delete all duplicate records except the first one (newest)
                        for duplicate in group.iter().skip(1) {
                            let _ = db.query("DELETE $id").bind(("id", duplicate.id.clone())).await;
                        }
                        info!("Deduplicated project decision duplicate: '{}' inside '{}'", dec_name, proj);
                    }
                }
            }
        }

        // Record schema version 2 in the meta table
        let _ = db
            .query("INSERT INTO meta (id, version) VALUES ('version', 2) ON DUPLICATE KEY UPDATE version = 2")
            .await;
    }

    // Embed schema at compile time so the binary carries no external files.
    let schema = include_str!("../../schema/memory.surql");
    db.query(schema).await?.check()?;

    info!("SurrealDB ready at {}", db_path.display());
    Ok(db)
}

// ---------------------------------------------------------------------------
// Query helpers
// ---------------------------------------------------------------------------

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
    let now = Utc::now();
    db.query(
        "INSERT INTO memory (key, content, tags, project, created_at, agent_id, token_saved, token_usage, embedding)
         VALUES ($key, $content, $tags, $project, type::datetime($created_at), $agent_id, $token_saved, $token_usage, $embedding)
         ON DUPLICATE KEY UPDATE
             content = $content, tags = $tags, created_at = type::datetime($created_at), agent_id = $agent_id, token_saved = IF $token_saved IS NOT NULL THEN token_saved + $token_saved ELSE token_saved END, token_usage = IF $token_usage IS NOT NULL THEN token_usage + $token_usage ELSE token_usage END, embedding = $embedding",
    )
    .bind(("key", key))
    .bind(("content", content))
    .bind(("tags", tags))
    .bind(("project", project.clone()))
    .bind(("created_at", now))
    .bind(("agent_id", agent_id))
    .bind(("token_saved", token_saved))
    .bind(("token_usage", token_usage))
    .bind(("embedding", embedding))
    .await?
    .check()?;

    // R6-M2: Purge ROI entries older than 30 days to prevent unbounded growth
    if token_saved.is_some() {
        let cutoff = now - Duration::days(30);
        let _ = db.query(
            "DELETE FROM memory WHERE project = $project AND tags CONTAINS 'roi' AND created_at < type::datetime($cutoff)"
        )
        .bind(("project", project))
        .bind(("cutoff", cutoff))
        .await;
    }

    Ok(())
}

/// Search memories for a project.
///
/// Strategy (in priority order):
///
/// 1. **Vector search** — if `query_embedding` is `Some`, rank all records that
///    have an embedding by cosine similarity, return top `limit`.
/// 2. **BM25 fallback** — used when no embedder is available, or as a union to
///    surface old records that pre-date embedding support.
///
/// Results are deduplicated by `key` with vector hits ranked first.
pub async fn search_memories(
    db: &KlaarDb,
    bm25_query: String,
    project: String,
    limit: u32,
    query_embedding: Option<Vec<f32>>,
) -> Result<Vec<MemoryRecord>> {
    match query_embedding {
        Some(embedding) => {
            // Fetch vector search candidates (up to 20) using native HNSW KNN index operator
            let vec_results: Vec<MemoryRecord> = db
                .query(
                    "SELECT *
                     FROM memory
                     WHERE project = $project AND tags CONTAINSNOT 'roi' AND embedding <|20, 100|> $embedding",
                )
                .bind(("project", project.clone()))
                .bind(("embedding", embedding))
                .await?
                .take(0)?;

            // Fetch lexical full-text candidates (up to 20)
            let bm25_results: Vec<MemoryRecord> = db
                .query(
                    "SELECT *, search::score(0) AS score
                     FROM memory
                     WHERE project = $project AND tags CONTAINSNOT 'roi' AND content @@ $query
                     ORDER BY score DESC
                     LIMIT 20",
                )
                .bind(("project", project))
                .bind(("query", bm25_query))
                .await?
                .take(0)?;

            // Perform Reciprocal Rank Fusion (RRF) in Rust
            // K parameter is standard 60
            const K: f64 = 60.0;
            let mut rrf_scores: std::collections::HashMap<String, (f64, MemoryRecord)> = std::collections::HashMap::new();

            // Process vector results
            for (rank, record) in vec_results.into_iter().enumerate() {
                let score = 1.0 / (K + rank as f64);
                rrf_scores
                    .entry(record.key.clone())
                    .and_modify(|(s, _)| *s += score)
                    .or_insert((score, record));
            }

            // Process BM25 results
            for (rank, record) in bm25_results.into_iter().enumerate() {
                let score = 1.0 / (K + rank as f64);
                rrf_scores
                    .entry(record.key.clone())
                    .and_modify(|(s, _)| *s += score)
                    .or_insert((score, record));
            }

            // Sort by RRF score descending
            let mut fused: Vec<(f64, MemoryRecord)> = rrf_scores.into_values().collect();
            fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

            // Return up to limit
            let merged: Vec<MemoryRecord> = fused
                .into_iter()
                .take(limit as usize)
                .map(|(_, record)| record)
                .collect();

            Ok(merged)
        }

        None => {
            // No embedder — pure BM25
            let results: Vec<MemoryRecord> = db
                .query(
                    "SELECT *
                     FROM memory
                     WHERE project = $project AND tags CONTAINSNOT 'roi' AND content @@ $query
                     ORDER BY search::score(0) DESC, created_at DESC
                     LIMIT $limit",
                )
                .bind(("project", project))
                .bind(("query", bm25_query))
                .bind(("limit", limit))
                .await?
                .take(0)?;
            Ok(results)
        }
    }
}

// ---------------------------------------------------------------------------
// Distributed Locking
// ---------------------------------------------------------------------------

/// Attempt to acquire a lock on a file for an agent.
pub async fn lock_file(
    db: &KlaarDb,
    file_path: String,
    agent_id: String,
    virtual_branch: String,
) -> Result<()> {
    let now = Utc::now();
    db.query(
        "INSERT INTO locks (file_path, agent_id, virtual_branch, locked_at)
         VALUES ($file_path, $agent_id, $virtual_branch, type::datetime($locked_at))"
    )
    .bind(("file_path", file_path))
    .bind(("agent_id", agent_id))
    .bind(("virtual_branch", virtual_branch))
    .bind(("locked_at", now))
    .await?
    .check()?;
    Ok(())
}

/// Release a lock gracefully.
pub async fn unlock_file(
    db: &KlaarDb,
    file_path: String,
    agent_id: String,
) -> Result<()> {
    db.query("DELETE locks WHERE file_path = $file_path AND agent_id = $agent_id")
        .bind(("file_path", file_path))
        .bind(("agent_id", agent_id))
        .await?;
    Ok(())
}

/// Check if a lock exists and apply automatic expiration purges (>30 min ttl).
pub async fn check_lock(
    db: &KlaarDb,
    file_path: String,
) -> Result<Option<FileLock>> {
    let mut response = db.query("SELECT * FROM locks WHERE file_path = $file_path LIMIT 1")
        .bind(("file_path", file_path.clone()))
        .await?;
    
    let lock: Option<FileLock> = response.take(0)?;
    
    if let Some(l) = lock {
        let expiration_threshold = Utc::now() - chrono::Duration::minutes(30);
        if l.locked_at < expiration_threshold {
            tracing::warn!("Auto-expiring stale lock for {} held by agent {} (older than 30m)", l.file_path, l.agent_id);
            db.query("DELETE locks WHERE file_path = $file_path")
                .bind(("file_path", file_path))
                .await?;
            return Ok(None);
        }
        return Ok(Some(l));
    }
    
    Ok(None)
}

/// Retrieve aggregated ROI/Token statistics.
pub async fn get_stats(
    db: &KlaarDb,
    time_limit: Option<DateTime<Utc>>,
    target_project: Option<String>
) -> Result<Vec<StatsRow>> {
    let mut q_str = "SELECT agent_id, project, math::sum(token_saved) AS total_saved, math::sum(token_usage) AS total_usage FROM memory".to_string();
    let mut filters = Vec::new();
    
    if time_limit.is_some() {
        filters.push("created_at >= $time_limit");
    }
    if target_project.is_some() {
        filters.push("project = $proj");
    }
    
    if !filters.is_empty() {
        q_str.push_str(" WHERE ");
        q_str.push_str(&filters.join(" AND "));
    }
    
    q_str.push_str(" GROUP BY agent_id, project");

    let mut q = db.query(q_str);
    if let Some(t) = time_limit {
        q = q.bind(("time_limit", t));
    }
    if let Some(p) = target_project {
        q = q.bind(("proj", p));
    }

    let mut response = q.await?;
    let stats: Vec<StatsRow> = response.take(0)?;
    Ok(stats)
}

// ---------------------------------------------------------------------------
// Decision Intelligence
// ---------------------------------------------------------------------------

pub async fn insert_decision(
    db: &KlaarDb,
    project: &str,
    decision: &str,
    context: &str,
    files: Vec<String>,
) -> Result<()> {
    let now = Utc::now();
    // R4-M2: Upsert on (project, decision) to prevent unbounded duplicate rows
    db.query(
        "INSERT INTO decisions (project, decision, context, governed_files, confidence, edit_count, created_at, last_confirmed)
         VALUES ($project, $decision, $context, $files, 1.0f, 0, type::datetime($now), type::datetime($now))
         ON DUPLICATE KEY UPDATE
             context = $context, governed_files = $files, last_confirmed = type::datetime($now)"
    )
    .bind(("project", project.to_string()))
    .bind(("decision", decision.to_string()))
    .bind(("context", context.to_string()))
    .bind(("files", files))
    .bind(("now", now))
    .await?
    .check()?;
    Ok(())
}

pub async fn get_decisions_for_files(
    db: &KlaarDb,
    project: &str,
    files: &[String],
) -> Result<Vec<DecisionRecord>> {
    let mut response = db.query(
        "SELECT * FROM decisions WHERE project = $project AND governed_files CONTAINSANY $files ORDER BY confidence DESC LIMIT 20"
    )
    .bind(("project", project.to_string()))
    .bind(("files", files.to_vec()))
    .await?;

    let decisions: Vec<DecisionRecord> = response.take(0)?;
    Ok(decisions)
}

pub async fn confirm_decision(
    db: &KlaarDb,
    decision_id: &str,
    project: &str,
) -> Result<()> {
    let now = Utc::now();
    // R4-M1: Enforce project ownership — only confirm decisions belonging to the specified project
    db.query(
        "UPDATE type::thing($id) SET confidence = 1.0f, edit_count = 0, last_confirmed = type::datetime($now) WHERE project = $project"
    )
    .bind(("id", decision_id.to_string()))
    .bind(("project", project.to_string()))
    .bind(("now", now))
    .await?
    .check()?;
    Ok(())
}

pub async fn increment_file_edits(
    db: &KlaarDb,
    file_path: &str,
) -> Result<()> {
    db.query(
        "UPDATE decisions SET 
            confidence = math::max([0.1f, 1.0f - ((edit_count + 1) * 0.1f)]),
            edit_count += 1
         WHERE governed_files CONTAINS $file_path"
    )
    .bind(("file_path", file_path.to_string()))
    .await?
    .check()?;
    Ok(())
}

pub async fn record_co_changes(
    db: &KlaarDb,
    project: &str,
    files: Vec<String>,
) -> Result<()> {
    let now = Utc::now();
    db.query(
        "INSERT INTO co_changes (project, files, created_at)
         VALUES ($project, $files, type::datetime($now))"
    )
    .bind(("project", project.to_string()))
    .bind(("files", files))
    .bind(("now", now))
    .await?
    .check()?;

    // R4-L2: Purge co_changes older than 30 days to prevent unbounded table growth
    let cutoff = now - Duration::days(30);
    let _ = db.query(
        "DELETE FROM co_changes WHERE project = $project AND created_at < type::datetime($cutoff)"
    )
    .bind(("project", project.to_string()))
    .bind(("cutoff", cutoff))
    .await;

    Ok(())
}

pub async fn get_co_changes(
    db: &KlaarDb,
    project: &str,
) -> Result<Vec<CoChangeRecord>> {
    let mut response = db.query(
        "SELECT * FROM co_changes WHERE project = $project ORDER BY created_at DESC LIMIT 50"
    )
    .bind(("project", project.to_string()))
    .await?;
    let records: Vec<CoChangeRecord> = response.take(0)?;
    Ok(records)
}

/// R6-L2: Purge stale co_changes and ROI memory entries across all projects.
pub async fn purge_stale_entries(db: &KlaarDb) -> Result<()> {
    let cutoff = Utc::now() - Duration::days(30);
    let _ = db.query(
        "DELETE FROM co_changes WHERE created_at < type::datetime($cutoff)"
    )
    .bind(("cutoff", cutoff))
    .await;

    let _ = db.query(
        "DELETE FROM memory WHERE tags CONTAINS 'roi' AND created_at < type::datetime($cutoff)"
    )
    .bind(("cutoff", cutoff))
    .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_db_locking_and_roi_memory() -> Result<()> {
        let temp_dir = tempdir()?;
        let db = init(temp_dir.path()).await?;

        // 1. Test memory insertion with token ROI attribute
        let agent_id = "agent_omega";
        let memory_key = "test_roi_memory";
        insert_memory(
            &db,
            memory_key.to_string(),
            "Optimized some functions".to_string(),
            vec!["test".to_string()],
            "test_project".to_string(),
            Some(agent_id.to_string()),
            Some(250), // token savings
            None,      // token usage
            None,      // embedding
        )
        .await?;

        let mem_result: Option<MemoryRecord> = db
            .query("SELECT * FROM memory WHERE key = $k")
            .bind(("k", memory_key))
            .await?
            .take(0)?;

        assert!(mem_result.is_some());
        let record = mem_result.unwrap();
        assert_eq!(record.agent_id.as_deref(), Some("agent_omega"));
        assert_eq!(record.token_saved, Some(250));

        // 2. Test Lock Acquisition
        let file_to_lock = "src/main.rs".to_string();
        
        lock_file(&db, file_to_lock.clone(), "agent_A".to_string(), "branch1".to_string()).await?;

        // Verify Lock Exists matching agent_A
        let check_res = check_lock(&db, file_to_lock.clone()).await?;
        assert!(check_res.is_some());
        assert_eq!(check_res.as_ref().unwrap().agent_id, "agent_A");

        // Attempting to lock the same file with another agent should fail
        let duplicate_lock_res = lock_file(&db, file_to_lock.clone(), "agent_B".to_string(), "branch1".to_string()).await;
        assert!(duplicate_lock_res.is_err(), "Duplicate lock should have failed for a unique constraint if implemented properly or manual check");

        // 3. Test Unlock
        unlock_file(&db, file_to_lock.clone(), "agent_A".to_string()).await?;

        // Verify it was unlocked
        let check_unlocked = check_lock(&db, file_to_lock.clone()).await?;
        assert!(check_unlocked.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_decision_intelligence() -> Result<()> {
        let temp_dir = tempdir()?;
        let db = init(temp_dir.path()).await?;

        let project = "test_project";
        let decision_text = "Use SurrealDB memory layer";
        let context_text = "Zero init cost and persistent memories";
        let files = vec!["src/db/mod.rs".to_string(), "src/main.rs".to_string()];

        // 1. Insert Decision
        insert_decision(&db, project, decision_text, context_text, files.clone()).await?;

        // 2. Get Decisions for Files
        let decisions = get_decisions_for_files(&db, project, &["src/main.rs".to_string()]).await?;
        assert_eq!(decisions.len(), 1);
        let dec = &decisions[0];
        assert_eq!(dec.decision, decision_text);
        assert_eq!(dec.confidence, 1.0);
        assert_eq!(dec.edit_count, 0);

        // 3. Increment file edits to degrade confidence
        increment_file_edits(&db, "src/main.rs").await?;
        let decisions_degraded = get_decisions_for_files(&db, project, &["src/main.rs".to_string()]).await?;
        assert_eq!(decisions_degraded[0].edit_count, 1);
        assert!((decisions_degraded[0].confidence - 0.9).abs() < 0.001);

        // 4. Increment multiple times to verify floor of 0.1
        for _ in 0..15 {
            increment_file_edits(&db, "src/main.rs").await?;
        }
        let decisions_floor = get_decisions_for_files(&db, project, &["src/main.rs".to_string()]).await?;
        assert!(decisions_floor[0].edit_count >= 10);
        assert!((decisions_floor[0].confidence - 0.1).abs() < 0.001);

        // 5. Confirm decision to restore confidence to 1.0
        let dec_id = &decisions_floor[0].id;
        confirm_decision(&db, &dec_id.to_string(), project).await?;
        let decisions_restored = get_decisions_for_files(&db, project, &["src/main.rs".to_string()]).await?;
        assert_eq!(decisions_restored[0].confidence, 1.0);
        assert_eq!(decisions_restored[0].edit_count, 0);

        // 6. Record co-changes
        record_co_changes(&db, project, vec!["src/main.rs".to_string(), "src/db/mod.rs".to_string()]).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_hybrid_search_rrf() -> Result<()> {
        let temp_dir = tempdir()?;
        let db = init(temp_dir.path()).await?;

        let project = "test_hybrid";

        // Insert test records
        // Record 1: Highly specific lexical text, completely different vector
        let key1 = "rec1".to_string();
        let content1 = "SurrealDB native search is awesome with Reciprocal Rank Fusion".to_string();
        let mut embedding1 = vec![0.0; 384];
        embedding1[0] = 1.0; // orthogonal vector 1

        // Record 2: Semantic vector match, but completely different lexical text
        let key2 = "rec2".to_string();
        let content2 = "Some random content here".to_string();
        let mut embedding2 = vec![0.0; 384];
        embedding2[1] = 1.0; // orthogonal vector 2

        insert_memory(
            &db,
            key1.clone(),
            content1.clone(),
            vec!["tag1".to_string()],
            project.to_string(),
            None,
            None,
            None,
            Some(embedding1),
        )
        .await?;

        insert_memory(
            &db,
            key2.clone(),
            content2.clone(),
            vec!["tag2".to_string()],
            project.to_string(),
            None,
            None,
            None,
            Some(embedding2),
        )
        .await?;

        // We will query for BM25 term "Reciprocal" and embedding matching Record 2
        let query_text = "Reciprocal".to_string();
        let mut query_embedding = vec![0.0; 384];
        query_embedding[1] = 1.0;

        // Perform RRF search
        let results = search_memories(
            &db,
            query_text,
            project.to_string(),
            2,
            Some(query_embedding),
        )
        .await?;

        println!("DEBUG SEARCH RESULTS: {:?}", results);
        // Ensure both records were returned (fused)
        assert_eq!(results.len(), 2);
        
        let keys: Vec<String> = results.iter().map(|r| r.key.clone()).collect();
        assert!(keys.contains(&key1));
        assert!(keys.contains(&key2));

        Ok(())
    }
}
