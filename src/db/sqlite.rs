//! SQLite backend: FTS5 for BM25, brute-force cosine for vectors.
//!
//! shortcut: one connection behind a std mutex, used inline from async fns (each
//! call is a short statement, and no lock is held across an await). Move to a
//! pool or `spawn_blocking` if memory writes ever contend.
//! shortcut: vector recall scans up to `VECTOR_SCAN_CAP` embedded rows of a
//! project instead of using an ANN index; upgrade when a project's memory
//! outgrows that (sqlite-vec is the next step).

use super::{CoChangeRecord, DecisionRecord, FileLock, MemoryRecord, StatsRow};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

const FILE_NAME: &str = "klaar.sqlite3";
const VECTOR_SCAN_CAP: i64 = 5000;
const CANDIDATES: usize = 20;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memory (
    id          INTEGER PRIMARY KEY,
    key         TEXT NOT NULL,
    content     TEXT NOT NULL,
    tags        TEXT NOT NULL DEFAULT '[]',
    project     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    agent_id    TEXT,
    token_saved INTEGER,
    token_usage INTEGER,
    embedding   BLOB,
    UNIQUE (project, key)
);
CREATE INDEX IF NOT EXISTS idx_memory_project ON memory (project);
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    content, content='memory', content_rowid='id', tokenize='porter unicode61'
);
CREATE TRIGGER IF NOT EXISTS memory_ai AFTER INSERT ON memory BEGIN
    INSERT INTO memory_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS memory_ad AFTER DELETE ON memory BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content) VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER IF NOT EXISTS memory_au AFTER UPDATE ON memory BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, content) VALUES ('delete', old.id, old.content);
    INSERT INTO memory_fts(rowid, content) VALUES (new.id, new.content);
END;

CREATE TABLE IF NOT EXISTS locks (
    file_path      TEXT PRIMARY KEY,
    agent_id       TEXT NOT NULL,
    virtual_branch TEXT NOT NULL,
    locked_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS decisions (
    id             INTEGER PRIMARY KEY,
    project        TEXT NOT NULL,
    decision       TEXT NOT NULL,
    context        TEXT NOT NULL,
    governed_files TEXT NOT NULL,
    confidence     REAL NOT NULL,
    edit_count     INTEGER NOT NULL,
    created_at     TEXT NOT NULL,
    last_confirmed TEXT NOT NULL,
    UNIQUE (project, decision)
);
CREATE INDEX IF NOT EXISTS idx_decisions_project ON decisions (project);

CREATE TABLE IF NOT EXISTS co_changes (
    id         INTEGER PRIMARY KEY,
    project    TEXT NOT NULL,
    files      TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_co_changes_project ON co_changes (project);
";

/// Memory rows that are not ROI bookkeeping.
const NOT_ROI: &str =
    "NOT EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE json_each.value = 'roi')";

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

fn ts(t: DateTime<Utc>) -> String {
    // Fixed-width UTC so text comparison orders chronologically.
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("bad timestamp {s:?} in the memory database"))?
        .with_timezone(&Utc))
}

fn embedding_to_blob(e: &[f32]) -> Vec<u8> {
    e.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_embedding(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Turn free text into a safe FTS5 query: alphanumeric words, each quoted, OR-ed.
/// `None` when nothing searchable is left (punctuation only), which matches nothing.
fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(32)
        .map(|w| format!("\"{w}\""))
        .collect();
    (!words.is_empty()).then(|| words.join(" OR "))
}

fn memory_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(MemoryRecord, String)> {
    let tags: String = r.get("tags")?;
    let created: String = r.get("created_at")?;
    Ok((
        MemoryRecord {
            key: r.get("key")?,
            content: r.get("content")?,
            tags: serde_json::from_str(&tags).unwrap_or_default(),
            project: r.get("project")?,
            created_at: DateTime::UNIX_EPOCH,
            agent_id: r.get("agent_id")?,
            token_saved: r.get::<_, Option<i64>>("token_saved")?.map(|v| v as usize),
            token_usage: r.get::<_, Option<i64>>("token_usage")?.map(|v| v as usize),
        },
        created,
    ))
}

fn finish(rows: Vec<(MemoryRecord, String)>) -> Result<Vec<MemoryRecord>> {
    rows.into_iter()
        .map(|(mut m, created)| {
            m.created_at = parse_ts(&created)?;
            Ok(m)
        })
        .collect()
}

/// `decisions:7` (what the other backend hands out) and plain `7` both mean row 7.
fn decision_row_id(id: &str) -> Result<i64> {
    id.rsplit(':')
        .next()
        .unwrap_or(id)
        .trim()
        .parse()
        .map_err(|_| anyhow!("not a decision id: {id:?}"))
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        std::fs::create_dir_all(db_path)?;
        let conn = Connection::open(db_path.join(FILE_NAME))
            .with_context(|| format!("open {}", db_path.join(FILE_NAME).display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        tracing::info!("SQLite memory ready at {}", db_path.display());
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow!("memory database lock poisoned"))
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
        let now = Utc::now();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO memory (key, content, tags, project, created_at, agent_id, token_saved, token_usage, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (project, key) DO UPDATE SET
                 content = excluded.content, tags = excluded.tags, created_at = excluded.created_at,
                 agent_id = excluded.agent_id,
                 token_saved = CASE WHEN excluded.token_saved IS NOT NULL
                                    THEN COALESCE(memory.token_saved, 0) + excluded.token_saved
                                    ELSE memory.token_saved END,
                 token_usage = CASE WHEN excluded.token_usage IS NOT NULL
                                    THEN COALESCE(memory.token_usage, 0) + excluded.token_usage
                                    ELSE memory.token_usage END,
                 embedding = excluded.embedding",
            params![
                key,
                content,
                serde_json::to_string(&tags)?,
                project,
                ts(now),
                agent_id,
                token_saved.map(|v| v as i64),
                token_usage.map(|v| v as i64),
                embedding.as_deref().map(embedding_to_blob),
            ],
        )?;
        // R6-M2: purge ROI entries older than 30 days to prevent unbounded growth.
        if token_saved.is_some() {
            conn.execute(
                "DELETE FROM memory WHERE project = ?1 AND created_at < ?2
                 AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE json_each.value = 'roi')",
                params![project, ts(now - Duration::days(30))],
            )?;
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
        let conn = self.conn()?;

        let lexical = |n: usize| -> Result<Vec<MemoryRecord>> {
            let Some(q) = fts_query(&bm25_query) else {
                return Ok(Vec::new());
            };
            let sql = format!(
                "SELECT memory.* FROM memory_fts JOIN memory ON memory.id = memory_fts.rowid
                 WHERE memory_fts MATCH ?1 AND memory.project = ?2 AND {NOT_ROI}
                 ORDER BY bm25(memory_fts), memory.created_at DESC LIMIT ?3"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![q, project, n as i64], memory_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            finish(rows)
        };

        let Some(query) = query_embedding else {
            return lexical(limit as usize);
        };

        let sql = format!(
            "SELECT memory.*, memory.embedding AS emb FROM memory
             WHERE project = ?1 AND embedding IS NOT NULL AND {NOT_ROI}
             ORDER BY created_at DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut scored: Vec<(f32, (MemoryRecord, String))> = stmt
            .query_map(params![project, VECTOR_SCAN_CAP], |r| {
                let blob: Vec<u8> = r.get("emb")?;
                Ok((
                    cosine(&query, &blob_to_embedding(&blob)),
                    memory_from_row(r)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(CANDIDATES);
        let vector = finish(scored.into_iter().map(|(_, row)| row).collect())?;

        Ok(super::fuse(vector, lexical(CANDIDATES)?, limit as usize))
    }

    pub async fn list_memories(
        &self,
        project: Option<String>,
        limit: u32,
    ) -> Result<Vec<MemoryRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM memory WHERE (?1 IS NULL OR project = ?1) ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![project, limit], memory_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        finish(rows)
    }

    pub async fn lock_file(
        &self,
        file_path: String,
        agent_id: String,
        virtual_branch: String,
    ) -> Result<()> {
        // PRIMARY KEY on file_path makes a second lock fail.
        self.conn()?.execute(
            "INSERT INTO locks (file_path, agent_id, virtual_branch, locked_at) VALUES (?1, ?2, ?3, ?4)",
            params![file_path, agent_id, virtual_branch, ts(Utc::now())],
        )?;
        Ok(())
    }

    pub async fn unlock_file(&self, file_path: String, agent_id: String) -> Result<()> {
        self.conn()?.execute(
            "DELETE FROM locks WHERE file_path = ?1 AND agent_id = ?2",
            params![file_path, agent_id],
        )?;
        Ok(())
    }

    pub async fn check_lock(&self, file_path: String) -> Result<Option<FileLock>> {
        let conn = self.conn()?;
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT agent_id, virtual_branch, locked_at FROM locks WHERE file_path = ?1",
                params![file_path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((agent_id, virtual_branch, locked_at)) = row else {
            return Ok(None);
        };
        let locked_at = parse_ts(&locked_at)?;
        if locked_at < Utc::now() - Duration::minutes(30) {
            tracing::warn!(
                "Auto-expiring stale lock for {file_path} held by agent {agent_id} (older than 30m)"
            );
            conn.execute("DELETE FROM locks WHERE file_path = ?1", params![file_path])?;
            return Ok(None);
        }
        Ok(Some(FileLock {
            file_path,
            agent_id,
            virtual_branch,
            locked_at,
        }))
    }

    pub async fn get_stats(
        &self,
        time_limit: Option<DateTime<Utc>>,
        target_project: Option<String>,
    ) -> Result<Vec<StatsRow>> {
        let mut sql = "SELECT agent_id, project, COALESCE(SUM(token_saved), 0), COALESCE(SUM(token_usage), 0) FROM memory".to_string();
        let mut filters = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        if let Some(t) = time_limit {
            filters.push("created_at >= ?");
            args.push(Value::Text(ts(t)));
        }
        if let Some(p) = target_project {
            filters.push("project = ?");
            args.push(Value::Text(p));
        }
        if !filters.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&filters.join(" AND "));
        }
        sql.push_str(" GROUP BY agent_id, project");
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(args), |r| {
                Ok(StatsRow {
                    agent_id: r.get(0)?,
                    project: r.get(1)?,
                    total_saved: r.get::<_, i64>(2)? as usize,
                    total_usage: r.get::<_, i64>(3)? as usize,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub async fn insert_decision(
        &self,
        project: &str,
        decision: &str,
        context: &str,
        files: Vec<String>,
    ) -> Result<()> {
        let now = ts(Utc::now());
        // R4-M2: upsert on (project, decision) to prevent unbounded duplicate rows.
        self.conn()?.execute(
            "INSERT INTO decisions (project, decision, context, governed_files, confidence, edit_count, created_at, last_confirmed)
             VALUES (?1, ?2, ?3, ?4, 1.0, 0, ?5, ?5)
             ON CONFLICT (project, decision) DO UPDATE SET
                 context = excluded.context, governed_files = excluded.governed_files,
                 last_confirmed = excluded.last_confirmed",
            params![project, decision, context, serde_json::to_string(&files)?, now],
        )?;
        Ok(())
    }

    pub async fn get_decisions_for_files(
        &self,
        project: &str,
        files: &[String],
    ) -> Result<Vec<DecisionRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, project, decision, context, governed_files, confidence, edit_count, created_at, last_confirmed
             FROM decisions
             WHERE project = ?1 AND EXISTS (
                 SELECT 1 FROM json_each(decisions.governed_files)
                 WHERE json_each.value IN (SELECT value FROM json_each(?2)))
             ORDER BY confidence DESC, id LIMIT 20",
        )?;
        let rows = stmt
            .query_map(params![project, serde_json::to_string(files)?], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, f64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(
                |(id, project, decision, context, files, conf, edits, created, confirmed)| {
                    Ok(DecisionRecord {
                        id: format!("decisions:{id}"),
                        project,
                        decision,
                        context,
                        governed_files: serde_json::from_str(&files)?,
                        confidence: conf as f32,
                        edit_count: edits as usize,
                        created_at: parse_ts(&created)?,
                        last_confirmed: parse_ts(&confirmed)?,
                    })
                },
            )
            .collect()
    }

    pub async fn confirm_decision(&self, decision_id: &str, project: &str) -> Result<()> {
        let id = decision_row_id(decision_id)?;
        // R4-M1: only confirm decisions that belong to the given project.
        self.conn()?.execute(
            "UPDATE decisions SET confidence = 1.0, edit_count = 0, last_confirmed = ?1
             WHERE id = ?2 AND project = ?3",
            params![ts(Utc::now()), id, project],
        )?;
        Ok(())
    }

    pub async fn increment_file_edits(&self, file_path: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE decisions SET
                 confidence = MAX(0.1, 1.0 - (edit_count + 1) * 0.1),
                 edit_count = edit_count + 1
             WHERE EXISTS (SELECT 1 FROM json_each(decisions.governed_files) WHERE json_each.value = ?1)",
            params![file_path],
        )?;
        Ok(())
    }

    pub async fn record_co_changes(&self, project: &str, files: Vec<String>) -> Result<()> {
        let now = Utc::now();
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO co_changes (project, files, created_at) VALUES (?1, ?2, ?3)",
            params![project, serde_json::to_string(&files)?, ts(now)],
        )?;
        // R4-L2: purge co_changes older than 30 days to prevent unbounded table growth.
        conn.execute(
            "DELETE FROM co_changes WHERE project = ?1 AND created_at < ?2",
            params![project, ts(now - Duration::days(30))],
        )?;
        Ok(())
    }

    pub async fn get_co_changes(&self, project: &str) -> Result<Vec<CoChangeRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, project, files, created_at FROM co_changes
             WHERE project = ?1 ORDER BY created_at DESC, id DESC LIMIT 50",
        )?;
        let rows = stmt
            .query_map(params![project], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, project, files, created)| {
                Ok(CoChangeRecord {
                    id: format!("co_changes:{id}"),
                    project,
                    files: serde_json::from_str(&files)?,
                    created_at: parse_ts(&created)?,
                })
            })
            .collect()
    }

    pub async fn purge_stale_entries(&self) -> Result<()> {
        let cutoff = ts(Utc::now() - Duration::days(30));
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM co_changes WHERE created_at < ?1",
            params![cutoff],
        )?;
        conn.execute(
            "DELETE FROM memory WHERE created_at < ?1
             AND EXISTS (SELECT 1 FROM json_each(memory.tags) WHERE json_each.value = 'roi')",
            params![cutoff],
        )?;
        Ok(())
    }
}
