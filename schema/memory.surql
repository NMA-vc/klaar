-- SurrealDB schema for klaar agent memory
-- Applied once at startup by db::init()
-- Embedding field is nullable so old records without vectors still work.

DEFINE TABLE IF NOT EXISTS memory SCHEMAFULL;

DEFINE FIELD IF NOT EXISTS key        ON TABLE memory TYPE string;
DEFINE FIELD IF NOT EXISTS content    ON TABLE memory TYPE string;
DEFINE FIELD IF NOT EXISTS tags       ON TABLE memory TYPE array<string>;
DEFINE FIELD IF NOT EXISTS project    ON TABLE memory TYPE string;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE memory TYPE datetime;

-- Optional: 384-dim float vector from BGESmallENV15.
-- NULL when the embedder was unavailable at store time.
DEFINE FIELD IF NOT EXISTS embedding  ON TABLE memory TYPE option<array<float>>;

-- Project lookup index
DEFINE INDEX IF NOT EXISTS idx_memory_project ON TABLE memory COLUMNS project;

-- Unique key index per project (enables ON DUPLICATE KEY UPDATE)
DEFINE INDEX IF NOT EXISTS idx_memory_unique_key ON TABLE memory COLUMNS project, key UNIQUE;

-- HNSW Vector Index for efficient semantic search
DEFINE INDEX IF NOT EXISTS idx_memory_embedding ON TABLE memory FIELDS embedding HNSW DIMENSION 384 DIST COSINE;

-- Full-text search index with BM25 ranking (fallback + hybrid)
DEFINE ANALYZER IF NOT EXISTS klaar_analyzer TOKENIZERS blank,class FILTERS ascii,lowercase,snowball(english);
DEFINE INDEX IF NOT EXISTS idx_memory_fts ON TABLE memory COLUMNS content SEARCH ANALYZER klaar_analyzer BM25(1.2, 0.75) HIGHLIGHTS;

-- ROI Tracking & Multi-Agent Attribution
DEFINE FIELD IF NOT EXISTS agent_id    ON TABLE memory TYPE option<string>;
DEFINE FIELD IF NOT EXISTS token_saved ON TABLE memory TYPE option<int>;
DEFINE FIELD IF NOT EXISTS token_usage ON TABLE memory TYPE option<int>;

-- Agentic Engine: Distributed Mutex Locks
DEFINE TABLE IF NOT EXISTS locks SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS file_path      ON TABLE locks TYPE string;
DEFINE FIELD IF NOT EXISTS agent_id       ON TABLE locks TYPE string;
DEFINE FIELD IF NOT EXISTS virtual_branch ON TABLE locks TYPE string;
DEFINE FIELD IF NOT EXISTS locked_at      ON TABLE locks TYPE datetime;

-- Enforce strict single-ownership per file
DEFINE INDEX IF NOT EXISTS idx_locks_file_path ON TABLE locks COLUMNS file_path UNIQUE;

-- Decisions Table
DEFINE TABLE IF NOT EXISTS decisions SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project         ON TABLE decisions TYPE string;
DEFINE FIELD IF NOT EXISTS decision        ON TABLE decisions TYPE string;
DEFINE FIELD IF NOT EXISTS context         ON TABLE decisions TYPE string;
DEFINE FIELD IF NOT EXISTS governed_files  ON TABLE decisions TYPE array<string>;
DEFINE FIELD IF NOT EXISTS confidence      ON TABLE decisions TYPE float; -- 1.0 default
DEFINE FIELD IF NOT EXISTS edit_count      ON TABLE decisions TYPE int;
DEFINE FIELD IF NOT EXISTS created_at      ON TABLE decisions TYPE datetime;
DEFINE FIELD IF NOT EXISTS last_confirmed  ON TABLE decisions TYPE datetime;

-- Project index for fast retrieval
DEFINE INDEX IF NOT EXISTS idx_decisions_project ON TABLE decisions COLUMNS project;

-- R4-M2: Unique index on (project, decision) to enable idempotent upserts
DEFINE INDEX IF NOT EXISTS idx_decisions_unique ON TABLE decisions COLUMNS project, decision UNIQUE;

-- Co-changes Table
DEFINE TABLE IF NOT EXISTS co_changes SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS project    ON TABLE co_changes TYPE string;
DEFINE FIELD IF NOT EXISTS files      ON TABLE co_changes TYPE array<string>;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE co_changes TYPE datetime;
