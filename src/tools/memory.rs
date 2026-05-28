use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;
use tokio::task::spawn_blocking;

use crate::db::{self, KlaarDb, MemoryRecord};
use crate::embedder::Embedder;

// ---------------------------------------------------------------------------
// store_memory
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct StoreResult {
    pub status: &'static str,
    pub key: String,
    pub project: String,
    pub embedded: bool,
}

pub async fn store_memory(
    db: &KlaarDb,
    embedder: &Option<Arc<Embedder>>,
    key: String,
    content: String,
    tags: Vec<String>,
    project: String,
) -> Result<StoreResult> {
    let embedder = embedder.clone();
    let content_clone = content.clone();
    let embedding = match embedder {
        Some(e) => {
            match spawn_blocking(move || e.embed(&content_clone)).await {
                Ok(Ok(emb)) => Some(emb),
                Ok(Err(e)) => {
                    tracing::warn!("Embedding failed during store_memory: {}", e);
                    None
                }
                Err(e) => {
                    tracing::warn!("Embedding task panicked during store_memory: {}", e);
                    None
                }
            }
        }
        None => None,
    };

    let embedded = embedding.is_some();

    db::insert_memory(db, key.clone(), content, tags, project.clone(), None, None, None, embedding).await?;

    Ok(StoreResult {
        status: "stored",
        key,
        project,
        embedded,
    })
}

// ---------------------------------------------------------------------------
// recall_memory
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RecallResult {
    pub count: usize,
    pub search_mode: &'static str,
    pub memories: Vec<MemoryRecord>,
}

pub async fn recall_memory(
    db: &KlaarDb,
    embedder: &Option<Arc<Embedder>>,
    query: String,
    project: String,
    limit: u32,
) -> Result<RecallResult> {
    let query_clone = query.clone();
    let query_embedding = match embedder.clone() {
        Some(e) => {
            match spawn_blocking(move || e.embed(&query_clone)).await {
                Ok(Ok(emb)) => Some(emb),
                Ok(Err(e)) => {
                    tracing::warn!("Embedding failed during recall_memory: {}", e);
                    None
                }
                Err(e) => {
                    tracing::warn!("Embedding task panicked during recall_memory: {}", e);
                    None
                }
            }
        }
        None => None,
    };

    let search_mode = if query_embedding.is_some() {
        "vector+bm25"
    } else {
        "bm25"
    };

    let memories =
        db::search_memories(db, query, project, limit, query_embedding).await?;

    let count = memories.len();
    Ok(RecallResult {
        count,
        search_mode,
        memories,
    })
}

// ---------------------------------------------------------------------------
// Decision Intelligence
// ---------------------------------------------------------------------------

pub async fn record_decision(
    db: &KlaarDb,
    project: String,
    decision: String,
    context: String,
    files: Vec<String>,
) -> Result<StoreResult> {
    db::insert_decision(db, &project, &decision, &context, files).await?;
    Ok(StoreResult {
        status: "decision_recorded",
        key: decision,
        project,
        embedded: false,
    })
}

pub async fn get_why(
    db: &KlaarDb,
    project: String,
    files: Vec<String>,
) -> Result<Vec<db::DecisionRecord>> {
    db::get_decisions_for_files(db, &project, &files).await
}

pub async fn confirm_decision(
    db: &KlaarDb,
    decision_id: String,
    project: String,
) -> Result<()> {
    db::confirm_decision(db, &decision_id, &project).await
}

