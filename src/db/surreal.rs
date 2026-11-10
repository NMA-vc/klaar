//! SurrealDB backend (embedded SurrealKV, HNSW vector index).
//!
//! Rows come back as JSON and are deserialized with serde, which keeps this file
//! independent of SurrealDB's own value types. Record ids are fetched as strings.

use super::{CoChangeRecord, DecisionRecord, FileLock, MemoryRecord, StatsRow};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use surrealdb::engine::local::{Db, SurrealKv};
use surrealdb::Surreal;
use tracing::info;

/// The SurrealDB backend. Cheap to clone (the handle is Arc-wrapped inside SurrealDB).
#[derive(Clone)]
pub struct Store(Surreal<Db>);

#[derive(Deserialize)]
struct DecisionRow {
    rid: String,
    project: String,
    decision: String,
    context: String,
    governed_files: Vec<String>,
    confidence: f32,
    edit_count: usize,
    created_at: DateTime<Utc>,
    last_confirmed: DateTime<Utc>,
}

impl From<DecisionRow> for DecisionRecord {
    fn from(r: DecisionRow) -> Self {
        Self {
            id: r.rid,
            project: r.project,
            decision: r.decision,
            context: r.context,
            governed_files: r.governed_files,
            confidence: r.confidence,
            edit_count: r.edit_count,
            created_at: r.created_at,
            last_confirmed: r.last_confirmed,
        }
    }
}

#[derive(Deserialize)]
struct CoChangeRow {
    rid: String,
    project: String,
    files: Vec<String>,
    created_at: DateTime<Utc>,
}

impl From<CoChangeRow> for CoChangeRecord {
    fn from(r: CoChangeRow) -> Self {
        Self {
            id: r.rid,
            project: r.project,
            files: r.files,
            created_at: r.created_at,
        }
    }
}

#[derive(Deserialize)]
struct StatsJson {
    agent_id: Option<String>,
    project: String,
    total_saved: Option<f64>,
    total_usage: Option<f64>,
}

/// Take statement `n` of a response as rows of `T`.
fn rows<T: DeserializeOwned>(response: &mut surrealdb::IndexedResults, n: usize) -> Result<Vec<T>> {
    let values: Vec<Value> = response.take(n)?;
    values
        .into_iter()
        .map(|v| serde_json::from_value(v).context("unexpected row shape from SurrealDB"))
        .collect()
}

fn floats(e: Vec<f32>) -> Vec<f64> {
    e.into_iter().map(f64::from).collect()
}

impl Store {
    /// Open the embedded SurrealKV store and apply the schema.
    /// A SurrealDB 2 data directory is not readable; this store starts fresh.
    pub async fn open(db_path: &Path) -> Result<Self> {
        std::fs::create_dir_all(db_path)?;
        let db = Surreal::new::<SurrealKv>(db_path.to_string_lossy().as_ref()).await?;
        db.use_ns("klaar").use_db("memory").await?;
        // Embed schema at compile time so the binary carries no external files.
        let schema = include_str!("../../schema/memory.surql");
        db.query(schema).await?.check()?;
        info!("SurrealDB ready at {}", db_path.display());
        Ok(Self(db))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_memory(
        &self,
        key: String,
        content: String,
        tags: Vec<String>,
        project: String,
        agent_id: Option<String>,
        token_saved: Option<usize>,
        token_usage: Option<usize>,
        embedding: Option<Vec<f32>>,
    ) -> Result<()> {
        let db = &self.0;
        let now = Utc::now();
        db.query(
            "INSERT INTO memory (key, content, tags, project, created_at, agent_id, token_saved, token_usage, embedding)
             VALUES ($key, $content, $tags, $project, <datetime> $created_at, $agent_id, $token_saved, $token_usage, $embedding)
             ON DUPLICATE KEY UPDATE
                 content = $content, tags = $tags, created_at = <datetime> $created_at, agent_id = $agent_id,
                 token_saved = IF $token_saved IS NOT NONE THEN (token_saved ?? 0) + $token_saved ELSE token_saved END,
                 token_usage = IF $token_usage IS NOT NONE THEN (token_usage ?? 0) + $token_usage ELSE token_usage END,
                 embedding = $embedding",
        )
        .bind(("key", key))
        .bind(("content", content))
        .bind(("tags", tags))
        .bind(("project", project.clone()))
        .bind(("created_at", now.to_rfc3339()))
        .bind(("agent_id", agent_id))
        .bind(("token_saved", token_saved.map(|v| v as i64)))
        .bind(("token_usage", token_usage.map(|v| v as i64)))
        .bind(("embedding", embedding.map(floats)))
        .await?
        .check()?;

        // R6-M2: purge ROI entries older than 30 days to prevent unbounded growth.
        if token_saved.is_some() {
            let cutoff = (now - Duration::days(30)).to_rfc3339();
            let _ = db
                .query(
                    "DELETE FROM memory WHERE project = $project AND tags CONTAINS 'roi' AND created_at < <datetime> $cutoff",
                )
                .bind(("project", project))
                .bind(("cutoff", cutoff))
                .await;
        }
        Ok(())
    }

    pub async fn search_memories(
        &self,
        bm25_query: String,
        project: String,
        limit: u32,
        query_embedding: Option<Vec<f32>>,
    ) -> Result<Vec<MemoryRecord>> {
        let db = &self.0;
        match query_embedding {
            Some(embedding) => {
                // Vector candidates (up to 20) via the HNSW KNN operator.
                let mut res = db
                    .query(
                        "SELECT * FROM memory
                         WHERE project = $project AND tags CONTAINSNOT 'roi' AND embedding <|20, 100|> $embedding",
                    )
                    .bind(("project", project.clone()))
                    .bind(("embedding", floats(embedding)))
                    .await?;
                let vector: Vec<MemoryRecord> = rows(&mut res, 0)?;

                // Lexical candidates (up to 20).
                let mut res = db
                    .query(
                        "SELECT *, search::score(0) AS score FROM memory
                         WHERE project = $project AND tags CONTAINSNOT 'roi' AND content @@ $query
                         ORDER BY score DESC LIMIT 20",
                    )
                    .bind(("project", project))
                    .bind(("query", bm25_query))
                    .await?;
                let lexical: Vec<MemoryRecord> = rows(&mut res, 0)?;

                Ok(super::fuse(vector, lexical, limit as usize))
            }
            None => {
                let mut res = db
                    .query(
                        "SELECT *, search::score(0) AS score FROM memory
                         WHERE project = $project AND tags CONTAINSNOT 'roi' AND content @@ $query
                         ORDER BY score DESC, created_at DESC LIMIT $limit",
                    )
                    .bind(("project", project))
                    .bind(("query", bm25_query))
                    .bind(("limit", i64::from(limit)))
                    .await?;
                rows(&mut res, 0)
            }
        }
    }

    pub async fn list_memories(
        &self,
        project: Option<String>,
        limit: u32,
    ) -> Result<Vec<MemoryRecord>> {
        let db = &self.0;
        let mut res = match project {
            Some(p) => {
                db.query("SELECT * FROM memory WHERE project = $proj LIMIT $limit")
                    .bind(("proj", p))
                    .bind(("limit", i64::from(limit)))
                    .await?
            }
            None => {
                db.query("SELECT * FROM memory LIMIT $limit")
                    .bind(("limit", i64::from(limit)))
                    .await?
            }
        };
        rows(&mut res, 0)
    }

    pub async fn lock_file(
        &self,
        file_path: String,
        agent_id: String,
        virtual_branch: String,
    ) -> Result<()> {
        // The unique index on file_path makes a second lock fail.
        self.0
            .query(
                "INSERT INTO locks (file_path, agent_id, virtual_branch, locked_at)
                 VALUES ($file_path, $agent_id, $virtual_branch, <datetime> $locked_at)",
            )
            .bind(("file_path", file_path))
            .bind(("agent_id", agent_id))
            .bind(("virtual_branch", virtual_branch))
            .bind(("locked_at", Utc::now().to_rfc3339()))
            .await?
            .check()?;
        Ok(())
    }

    pub async fn unlock_file(&self, file_path: String, agent_id: String) -> Result<()> {
        self.0
            .query("DELETE locks WHERE file_path = $file_path AND agent_id = $agent_id")
            .bind(("file_path", file_path))
            .bind(("agent_id", agent_id))
            .await?
            .check()?;
        Ok(())
    }

    pub async fn check_lock(&self, file_path: String) -> Result<Option<FileLock>> {
        let db = &self.0;
        let mut res = db
            .query("SELECT * FROM locks WHERE file_path = $file_path LIMIT 1")
            .bind(("file_path", file_path.clone()))
            .await?;
        let Some(lock) = rows::<FileLock>(&mut res, 0)?.into_iter().next() else {
            return Ok(None);
        };
        if lock.locked_at < Utc::now() - Duration::minutes(30) {
            tracing::warn!(
                "Auto-expiring stale lock for {} held by agent {} (older than 30m)",
                lock.file_path,
                lock.agent_id
            );
            db.query("DELETE locks WHERE file_path = $file_path")
                .bind(("file_path", file_path))
                .await?
                .check()?;
            return Ok(None);
        }
        Ok(Some(lock))
    }

    pub async fn get_stats(
        &self,
        time_limit: Option<DateTime<Utc>>,
        target_project: Option<String>,
    ) -> Result<Vec<StatsRow>> {
        let mut q = "SELECT agent_id, project, math::sum(token_saved) AS total_saved, math::sum(token_usage) AS total_usage FROM memory".to_string();
        let mut filters = Vec::new();
        if time_limit.is_some() {
            filters.push("created_at >= <datetime> $time_limit");
        }
        if target_project.is_some() {
            filters.push("project = $proj");
        }
        if !filters.is_empty() {
            q.push_str(" WHERE ");
            q.push_str(&filters.join(" AND "));
        }
        q.push_str(" GROUP BY agent_id, project");

        let mut query = self.0.query(q);
        if let Some(t) = time_limit {
            query = query.bind(("time_limit", t.to_rfc3339()));
        }
        if let Some(p) = target_project {
            query = query.bind(("proj", p));
        }
        let mut res = query.await?;
        Ok(rows::<StatsJson>(&mut res, 0)?
            .into_iter()
            .map(|r| StatsRow {
                agent_id: r.agent_id,
                project: r.project,
                total_saved: r.total_saved.unwrap_or(0.0) as usize,
                total_usage: r.total_usage.unwrap_or(0.0) as usize,
            })
            .collect())
    }

    pub async fn insert_decision(
        &self,
        project: &str,
        decision: &str,
        context: &str,
        files: Vec<String>,
    ) -> Result<()> {
        // R4-M2: upsert on (project, decision) to prevent unbounded duplicate rows.
        self.0
            .query(
                "INSERT INTO decisions (project, decision, context, governed_files, confidence, edit_count, created_at, last_confirmed)
                 VALUES ($project, $decision, $context, $files, 1.0f, 0, <datetime> $now, <datetime> $now)
                 ON DUPLICATE KEY UPDATE
                     context = $context, governed_files = $files, last_confirmed = <datetime> $now",
            )
            .bind(("project", project.to_string()))
            .bind(("decision", decision.to_string()))
            .bind(("context", context.to_string()))
            .bind(("files", files))
            .bind(("now", Utc::now().to_rfc3339()))
            .await?
            .check()?;
        Ok(())
    }

    pub async fn get_decisions_for_files(
        &self,
        project: &str,
        files: &[String],
    ) -> Result<Vec<DecisionRecord>> {
        let mut res = self
            .0
            .query(
                "SELECT *, <string> id AS rid FROM decisions
                 WHERE project = $project AND governed_files CONTAINSANY $files
                 ORDER BY confidence DESC LIMIT 20",
            )
            .bind(("project", project.to_string()))
            .bind(("files", files.to_vec()))
            .await?;
        Ok(rows::<DecisionRow>(&mut res, 0)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn confirm_decision(&self, decision_id: &str, project: &str) -> Result<()> {
        // R4-M1: only confirm decisions that belong to the given project.
        self.0
            .query(
                "UPDATE <record> $id SET confidence = 1.0f, edit_count = 0, last_confirmed = <datetime> $now
                 WHERE project = $project",
            )
            .bind(("id", decision_id.to_string()))
            .bind(("project", project.to_string()))
            .bind(("now", Utc::now().to_rfc3339()))
            .await?
            .check()?;
        Ok(())
    }

    pub async fn increment_file_edits(&self, file_path: &str) -> Result<()> {
        self.0
            .query(
                "UPDATE decisions SET
                     confidence = math::max([0.1f, 1.0f - ((edit_count + 1) * 0.1f)]),
                     edit_count += 1
                 WHERE governed_files CONTAINS $file_path",
            )
            .bind(("file_path", file_path.to_string()))
            .await?
            .check()?;
        Ok(())
    }

    pub async fn record_co_changes(&self, project: &str, files: Vec<String>) -> Result<()> {
        let now = Utc::now();
        let db = &self.0;
        db.query(
            "INSERT INTO co_changes (project, files, created_at)
             VALUES ($project, $files, <datetime> $now)",
        )
        .bind(("project", project.to_string()))
        .bind(("files", files))
        .bind(("now", now.to_rfc3339()))
        .await?
        .check()?;

        // R4-L2: purge co_changes older than 30 days to prevent unbounded table growth.
        let _ = db
            .query("DELETE FROM co_changes WHERE project = $project AND created_at < <datetime> $cutoff")
            .bind(("project", project.to_string()))
            .bind(("cutoff", (now - Duration::days(30)).to_rfc3339()))
            .await;
        Ok(())
    }

    pub async fn get_co_changes(&self, project: &str) -> Result<Vec<CoChangeRecord>> {
        let mut res = self
            .0
            .query(
                "SELECT *, <string> id AS rid FROM co_changes
                 WHERE project = $project ORDER BY created_at DESC LIMIT 50",
            )
            .bind(("project", project.to_string()))
            .await?;
        Ok(rows::<CoChangeRow>(&mut res, 0)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn purge_stale_entries(&self) -> Result<()> {
        let cutoff = (Utc::now() - Duration::days(30)).to_rfc3339();
        let db = &self.0;
        let _ = db
            .query("DELETE FROM co_changes WHERE created_at < <datetime> $cutoff")
            .bind(("cutoff", cutoff.clone()))
            .await;
        let _ = db
            .query(
                "DELETE FROM memory WHERE tags CONTAINS 'roi' AND created_at < <datetime> $cutoff",
            )
            .bind(("cutoff", cutoff))
            .await;
        Ok(())
    }
}
