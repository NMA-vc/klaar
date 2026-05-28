use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{info, warn};
use crate::cache::SearchCache;

use crate::config::GlobalConfig;
use crate::db::KlaarDb;
use crate::embedder::Embedder;
use crate::mcp::{error_content, text_content};
use crate::tools::{apply, gatekeeper, ls, memory, read, search};
use crate::utils::safe_truncate;

/// F4: hard cap on recall_memory limit
const RECALL_LIMIT_MAX: u32 = 50;

/// F4: truncate individual memory content beyond this length
const MEMORY_CONTENT_MAX_CHARS: usize = 4096;

/// R4-L1: cap decision/context input length to prevent unbounded storage
const DECISION_MAX_CHARS: usize = 4096;

// ---------------------------------------------------------------------------
// Tool descriptors (for tools/list)
// ---------------------------------------------------------------------------

pub fn tool_list() -> Value {
    json!({
        "tools": [
            {
                "name": "surgical_read",
                "description": "Read a file in 'full', 'skeleton', or 'compressed' mode. Skeleton mode extracts only signatures (signatures only). Compressed mode is a full file read with all comments removed using tree-sitter. Reduces token usage while remaining semantically lossless.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute or relative path to the file to read"
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["skeleton", "full", "compressed"],
                            "description": "Use 'skeleton' for signatures, 'compressed' for full file without comments, 'full' for complete content"
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "Unique identifier of the agent requesting the read. Used for token tracking."
                        }
                    },
                    "required": ["file_path", "mode", "agent_id"]
                }
            },
            {
                "name": "fast_apply",
                "description": "Apply surgical line replacements to a file without rewriting it entirely. Validates line numbers before applying. Hunks are applied bottom-up for correctness.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Path to the file to patch"
                        },
                        "hunks": {
                            "type": "array",
                            "description": "List of line replacements to apply",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "line_start": { "type": "integer", "description": "First line to replace (1-indexed, inclusive)" },
                                    "line_end":   { "type": "integer", "description": "Last line to replace (1-indexed, inclusive)" },
                                    "new_content": { "type": "string", "description": "Replacement content for the specified line range" }
                                },
                                "required": ["line_start", "line_end", "new_content"]
                            }
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "Unique identifier of the agent applying the code. Required for lock evaluation."
                        }
                    },
                    "required": ["file_path", "hunks", "agent_id"]
                }
            },
            {
                "name": "store_memory",
                "description": "Store a memory entry in the embedded SurrealDB. Call this when you learn something important about a project — decisions, architecture patterns, gotchas. The agent can recall these in future sessions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key":     { "type": "string", "description": "Unique identifier for this memory within the project" },
                        "content": { "type": "string", "description": "The memory content to store" },
                        "tags":    { "type": "array", "items": { "type": "string" }, "description": "Tags for categorization" },
                        "project": { "type": "string", "description": "Project name or identifier" }
                    },
                    "required": ["key", "content", "project"]
                }
            },
            {
                "name": "recall_memory",
                "description": "Search stored memories using hybrid vector+BM25 search (or BM25 fallback), filtered by project. Call this at the start of any task to retrieve prior decisions and context. Max 50 results.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query":   { "type": "string", "description": "Search query" },
                        "project": { "type": "string", "description": "Project name to search within" },
                        "limit":   { "type": "integer", "description": "Max results to return (default: 5, max: 50)", "default": 5, "maximum": 50 }
                    },
                    "required": ["query", "project"]
                }
            },
            {
                "name": "record_decision",
                "description": "Record an architectural decision to the memory store, linking it to the files it governs. If a decision with the same text already exists for this project, its context and governed files are updated but confidence is preserved. Call confirm_decision to explicitly restore confidence to 1.0.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project identifier" },
                        "decision": { "type": "string", "description": "The decision made" },
                        "context": { "type": "string", "description": "Context and reasoning for the decision" },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Files governed by this decision" }
                    },
                    "required": ["project", "decision", "context", "files"]
                }
            },
            {
                "name": "get_why",
                "description": "Get architectural decisions governing the given files. Use before modifying complex files to avoid breaking established patterns.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project identifier" },
                        "files": { "type": "array", "items": { "type": "string" }, "description": "Files to check decisions for" }
                    },
                    "required": ["project", "files"]
                }
            },
            {
                "name": "confirm_decision",
                "description": "Confirm that an existing decision is still valid, restoring its confidence score to 1.0.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "decision_id": { "type": "string", "description": "ID of the decision to confirm" },
                        "project": { "type": "string", "description": "Project identifier to verify ownership" }
                    },
                    "required": ["decision_id", "project"]
                }
            },
            {
                "name": "pre_push_check",
                "description": "Run pre-push validation checks defined in .klaar/targets.toml for a given target (e.g. 'production'). Runs checks sequentially; stops on first failure. Times out after 120s per check. Optionally runs `but validate` if GitButler CLI is present.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project_path": { "type": "string", "description": "Absolute path to the project root" },
                        "target":       { "type": "string", "description": "Target name (e.g. 'production', 'staging')" }
                    },
                    "required": ["project_path", "target"]
                }
            },
            {
                "name": "lock_file",
                "description": "Claim exclusive modification rights for a file tied to your agent ID. Required prior to calling fast_apply.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Path to the file to lock" },
                        "agent_id": { "type": "string", "description": "Your agent identifier" }
                    },
                    "required": ["file_path", "agent_id"]
                }
            },
            {
                "name": "unlock_file",
                "description": "Release your exclusive lock on a file if it is no longer needed or application is complete. Note: agent_id is used for coordination tracking only and is not cryptographically verified.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": { "type": "string", "description": "Path to the file to unlock" },
                        "agent_id": { "type": "string", "description": "Your agent identifier" }
                    },
                    "required": ["file_path", "agent_id"]
                }
            },
            {
                "name": "grep",
                "description": "Token-optimized codebase search. Wraps ripgrep with smart grouping and truncation to save context tokens. Groups matches by file and strips redundant whitespace.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search pattern (regex supported)" },
                        "project_path": { "type": "string", "description": "Absolute path to search within" },
                        "case_insensitive": { "type": "boolean", "description": "Whether to ignore case (default: true)", "default": true },
                        "include_glob": { "type": "string", "description": "Optional glob pattern (e.g. '*.rs')" }
                    },
                    "required": ["query", "project_path"]
                }
            },
            {
                "name": "ls",
                "description": "Noise-filtered directory listing. Auto-excludes artifact directories (node_modules, target, .next, etc.) and returns a compact tree structure to save tokens.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project_path": { "type": "string", "description": "Absolute path to list" },
                        "max_depth": { "type": "integer", "description": "Maximum depth to traverse (default: 2)", "default": 2 }
                    },
                    "required": ["project_path"]
                }
            },
            {
                "name": "find_symbol",
                "description": "Fast symbol lookup across the whole project. Returns the file and line number where a function, struct, or class is defined. Uses an in-memory index built on demand.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string", "description": "Name of the symbol to find" },
                        "project_path": { "type": "string", "description": "Absolute path to the project root" }
                    },
                    "required": ["symbol", "project_path"]
                }
            },
            {
                "name": "get_co_changes",
                "description": "Get file co-change relationships for the project. Returns groups of files that are frequently modified together in recent commits or fast_apply sessions, helping agents understand dependencies.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project identifier" }
                    },
                    "required": ["project"]
                }
            }
        ]
    })
}

// ---------------------------------------------------------------------------
// Tool dispatch — GlobalConfig threaded through for security guards
// ---------------------------------------------------------------------------

pub async fn dispatch(
    name: &str,
    args: &Value,
    db: &Arc<KlaarDb>,
    config: &Arc<GlobalConfig>,
    embedder: &Arc<tokio::sync::RwLock<Option<Arc<Embedder>>>>,
    cache: &Arc<SearchCache>,
) -> Value {
    match name {
        "surgical_read"  => dispatch_surgical_read(args, db, config, cache).await,
        "fast_apply"     => dispatch_fast_apply(args, db, config, cache).await,
        "store_memory"   => dispatch_store_memory(args, db, embedder).await,
        "recall_memory"  => dispatch_recall_memory(args, db, embedder).await,
        "record_decision"=> dispatch_record_decision(args, db).await,
        "get_why"        => dispatch_get_why(args, db).await,
        "confirm_decision" => dispatch_confirm_decision(args, db).await,
        "pre_push_check" => dispatch_pre_push_check(args, config).await,
        "lock_file"      => dispatch_lock_file(args, db, config).await,
        "unlock_file"    => dispatch_unlock_file(args, db, config).await,
        "grep" | "klaar_grep"   => dispatch_klaar_grep(args, config, cache).await,
        "ls" | "klaar_ls"     => dispatch_klaar_ls(args, config).await,
        "find_symbol" | "klaar_find_symbol" => dispatch_klaar_find_symbol(args, config, cache).await,
        "get_co_changes" => dispatch_get_co_changes(args, db).await,
        other => {
            warn!("Unknown tool called: {}", other);
            error_content(format!("Unknown tool: '{}'", other))
        }
    }
}

async fn dispatch_surgical_read(
    args: &Value, 
    db: &Arc<KlaarDb>, 
    config: &Arc<GlobalConfig>,
    cache: &Arc<SearchCache>,
) -> Value {
    let file_path = match args["file_path"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: file_path"),
    };
    let mode = match args["mode"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: mode"),
    };
    let agent_id = match args["agent_id"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: agent_id"),
    };
    
    // R5-H2: Canonicalize path for cache key so watcher invalidation (which uses canonical paths) matches
    let canonical_file_path = std::path::Path::new(file_path)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| file_path.to_string());

    // Check cache first (after agent_id validation)
    let cache_key = SearchCache::key_read(&canonical_file_path, mode);
    if let Some(cached) = cache.content.get(&cache_key).await {
        return text_content(cached);
    }

    let file_path_str = file_path.to_string();
    let mode_str = mode.to_string();
    let config_clone = config.clone();

    let read_result = tokio::task::spawn_blocking(move || {
        read::surgical_read(&file_path_str, &mode_str, &config_clone)
    }).await;

    let res = match read_result {
        Ok(r) => r,
        Err(e) => return error_content(format!("surgical_read thread join failed: {}", e)),
    };

    match res {
        Ok((mut content, saved_bytes, raw_bytes)) => {
            let project_root = crate::utils::find_project_root(file_path);

            // Phase 5: Standards Injection (Perform before caching and ROI logging for consistency)
            if mode == "skeleton" {
                crate::tools::standards::inject_standards(&mut content, file_path, project_root.as_deref());
            }

            if saved_bytes > 0 {
                let estimated_tokens_saved = saved_bytes / 4;
                let estimated_total_tokens = raw_bytes / 4;
                
                // Log token savings to the database using a robust collision-free, timestamped unique key
                let key = format!(
                    "token_savings_{}_{}_{}",
                    agent_id,
                    canonical_file_path,
                    chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
                );
                
                let store_content = format!("Saved {} tokens by pruning {}", estimated_tokens_saved, file_path);
                
                let parsed_project = project_root.clone()
                    .map(|root| {
                        // R6-M3: Use full canonical path to avoid cross-repo collisions
                        std::path::Path::new(&root)
                            .canonicalize()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or(root)
                    })
                    .unwrap_or_else(|| "klaar_internal".to_string());
                
                let db_clone = db.clone();
                let agent_id_clone = agent_id.clone();
                tokio::spawn(async move {
                    let _ = crate::db::insert_memory(
                        &db_clone,
                        key,
                        store_content,
                        vec!["roi".to_string(), "token_savings".to_string()],
                        parsed_project,
                        Some(agent_id_clone),
                        Some(estimated_tokens_saved),
                        Some(estimated_total_tokens),
                        None
                    ).await;
                });
            }

            // Populate cache with the fully injected content on success
            cache.content.insert(cache_key, content.clone()).await;

            // Phase 3: Diff-aware reads
            let last_sent_key = (agent_id.clone(), canonical_file_path.clone());
            if content.len() <= 500_000 {
                if let Some(old_content) = cache.last_sent.get(&last_sent_key).await {
                    if old_content.len() <= 500_000 {
                        let ratio = crate::utils::compute_diff_ratio(&old_content, &content);
                        // If change is minor (< 20%), build the full diff and return it
                        if ratio > 0.0 && ratio < 0.20 {
                            let stats = crate::utils::compute_diff_stats(&old_content, &content);
                            let response = format!(
                                "DIFF SINCE LAST READ (Ratio: {:.2})\n{}\n[Content updated in cache]",
                                stats.ratio, stats.diff
                            );
                            // Update last_sent with new state
                            cache.insert_last_sent(agent_id.clone(), canonical_file_path.clone(), content.clone()).await;
                            return text_content(response);
                        }
                    }
                }
            }

            // Otherwise return full/skeleton content and update last_sent
            cache.insert_last_sent(agent_id.clone(), canonical_file_path.clone(), content.clone()).await;
            text_content(content)
        },
        Err(e) => error_content(format!("surgical_read failed: {}", e)),
    }
}

async fn dispatch_fast_apply(
    args: &Value, 
    db: &Arc<KlaarDb>, 
    config: &Arc<GlobalConfig>,
    cache: &Arc<SearchCache>,
) -> Value {
    let file_path = match args["file_path"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: file_path"),
    };

    let project_root_opt = crate::utils::find_project_root(&file_path);

    let hunks_val = &args["hunks"];
    if hunks_val.is_null() {
        return error_content("Missing required field: hunks");
    }

    let hunks: Vec<apply::Hunk> = match serde_json::from_value(hunks_val.clone()) {
        Ok(h) => h,
        Err(e) => return error_content(format!("Invalid hunks format: {}", e)),
    };
    if hunks.is_empty() {
        return error_content("hunks must be non-empty");
    }
    
    let agent_id = match args["agent_id"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: agent_id"),
    };

    let mut auto_claimed = false;

    // Sub-Agent Coordination Protocol: Pre-flight lock evaluation
    match crate::db::check_lock(db, file_path.clone()).await {
        Ok(Some(lock)) => {
            if lock.agent_id != agent_id {
                return error_content(format!(
                    "ResourceLocked: File '{}' is actively locked by agent '{}' on virtual branch '{}'.",
                    file_path, lock.agent_id, lock.virtual_branch
                ));
            }
            // Fast-path: already locked by us. No need to auto-claim again.
        },
        Ok(None) => {
            // Auto-claim lock if free
            let project_root = project_root_opt.clone()
                .unwrap_or_else(|| file_path.clone());
            let vb = get_current_git_branch(&project_root).await;
            if let Err(e) = crate::db::lock_file(db, file_path.clone(), agent_id.clone(), vb).await {
                return error_content(format!(
                    "ResourceLocked: Failed to acquire lock for file '{}'. Another agent may have acquired it: {}",
                    file_path, e
                ));
            }
            auto_claimed = true;
        },
        Err(e) => return error_content(format!("Failed to verify locks: {}", e)),
    }

    let file_path_clone = file_path.clone();
    let config_clone = config.clone();
    let apply_res = tokio::task::spawn_blocking(move || {
        apply::fast_apply(&file_path_clone, hunks, &config_clone)
    }).await;

    let res = match apply_res {
        Ok(r) => r,
        Err(e) => {
            if auto_claimed {
                let _ = crate::db::unlock_file(db, file_path.clone(), agent_id.clone()).await;
            }
            return error_content(format!("fast_apply thread join failed: {}", e));
        }
    };

    match res {
        Ok(result) => {
            // Invalidate cache for this file since it changed
            let cache_clone = cache.clone();
            let path_clone = file_path.clone();
            tokio::spawn(async move {
                cache_clone.invalidate_path(&path_clone).await;
            });

            // Increment edit count for Decision Intelligence staleness and schedule debounced co-change check
            let db_clone = db.clone();
            let cache_clone = cache.clone();
            let path_clone2 = file_path.clone();
            if result.lines_changed > 0 {
                let project_root_opt_clone = project_root_opt.clone();
                tokio::spawn(async move {
                    let _ = crate::db::increment_file_edits(&db_clone, &path_clone2).await;
                    if let Some(project_root) = project_root_opt_clone {
                        // R6-M3: Use full canonical path to avoid cross-repo collisions
                        let parsed_project = std::path::Path::new(&project_root)
                            .canonicalize()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|_| project_root.clone());
                        schedule_co_change_check(db_clone, cache_clone, project_root, parsed_project);
                    }
                });
            }

            let response_msg = if auto_claimed {
                format!(
                    "Applied {} line replacement(s) to {}. NOTE: A write lock was automatically acquired for this file under agent '{}' and remains active. Remember to call unlock_file when you are finished modifying this file.",
                    result.lines_changed, result.file_path, agent_id
                )
            } else {
                format!(
                    "Applied {} line replacement(s) to {}",
                    result.lines_changed, result.file_path
                )
            };
            text_content(response_msg)
        },
        Err(e) => {
            if auto_claimed {
                let _ = crate::db::unlock_file(db, file_path, agent_id).await;
            }
            error_content(format!("fast_apply failed: {}", e))
        }
    }
}

async fn dispatch_store_memory(args: &Value, db: &Arc<KlaarDb>, embedder_cfg: &Arc<tokio::sync::RwLock<Option<Arc<Embedder>>>>) -> Value {
    let key = match args["key"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: key"),
    };
    let content = match args["content"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: content"),
    };
    let project = match args["project"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: project"),
    };
    let tags: Vec<String> = args["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let embedder = embedder_cfg.read().await.clone();
    match memory::store_memory(db, &embedder, key, content, tags, project).await {
        Ok(result) => text_content(format!(
            "Stored memory '{}' in project '{}' (embedding: {})",
            result.key,
            result.project,
            if result.embedded { "yes" } else { "no — model unavailable, BM25 only" }
        )),
        Err(e) => error_content(format!("store_memory failed: {}", e)),
    }
}

async fn dispatch_recall_memory(args: &Value, db: &Arc<KlaarDb>, embedder_cfg: &Arc<tokio::sync::RwLock<Option<Arc<Embedder>>>>) -> Value {
    let query = match args["query"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: query"),
    };
    let project = match args["project"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: project"),
    };

    // F4: hard-cap limit at RECALL_LIMIT_MAX regardless of caller input
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(5)
        .min(RECALL_LIMIT_MAX as u64) as u32;

    let embedder = embedder_cfg.read().await.clone();
    match memory::recall_memory(db, &embedder, query, project, limit).await {
        Ok(result) => {
            if result.memories.is_empty() {
                text_content(format!("No memories found for this query (mode: {}).", result.search_mode))
            } else {
                let formatted: Vec<String> = result
                    .memories
                    .into_iter()
                    .map(|mut m| {
                        // F4: truncate content to keep response payload bounded safely
                        safe_truncate(&mut m.content, MEMORY_CONTENT_MAX_CHARS);

                        format!(
                            "### [{}] {}\nTags: {}\nStored: {}\n\n{}",
                            m.project,
                            m.key,
                            m.tags.join(", "),
                            m.created_at.format("%Y-%m-%d %H:%M UTC"),
                            m.content
                        )
                    })
                    .collect();
                text_content(format!(
                    "Found {} memories (mode: {}):\n\n{}",
                    result.count,
                    result.search_mode,
                    formatted.join("\n---\n")
                ))
            }
        }
        Err(e) => error_content(format!("recall_memory failed: {}", e)),
    }
}

async fn dispatch_record_decision(args: &Value, db: &Arc<KlaarDb>) -> Value {
    let project = match args["project"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: project"),
    };
    let decision = match args["decision"].as_str() {
        Some(s) => {
            let mut d = s.to_string();
            safe_truncate(&mut d, DECISION_MAX_CHARS);
            d
        }
        None => return error_content("Missing required field: decision"),
    };
    let context = match args["context"].as_str() {
        Some(s) => {
            let mut c = s.to_string();
            safe_truncate(&mut c, DECISION_MAX_CHARS);
            c
        }
        None => return error_content("Missing required field: context"),
    };
    let files: Vec<String> = args["files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    match memory::record_decision(db, project, decision, context, files).await {
        Ok(res) => text_content(format!("Recorded decision '{}' for project '{}'", res.key, res.project)),
        Err(e) => error_content(format!("record_decision failed: {}", e)),
    }
}

async fn dispatch_get_why(args: &Value, db: &Arc<KlaarDb>) -> Value {
    let project = match args["project"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: project"),
    };
    let files: Vec<String> = args["files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    match memory::get_why(db, project, files).await {
        Ok(decisions) => {
            if decisions.is_empty() {
                text_content("No governing decisions found for these files.")
            } else {
                let formatted: Vec<String> = decisions
                    .into_iter()
                    .map(|d| {
                        format!(
                            "### Decision: {}\nID: {}\nConfidence: {:.2}\nContext: {}\nGoverned Files: {}",
                            d.decision,
                            d.id,
                            d.confidence,
                            d.context,
                            d.governed_files.join(", ")
                        )
                    })
                    .collect();
                text_content(format!(
                    "Found {} governing decisions:\n\n{}",
                    formatted.len(),
                    formatted.join("\n---\n")
                ))
            }
        }
        Err(e) => error_content(format!("get_why failed: {}", e)),
    }
}

async fn dispatch_confirm_decision(args: &Value, db: &Arc<KlaarDb>) -> Value {
    let decision_id = match args["decision_id"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: decision_id"),
    };
    let project = match args["project"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: project"),
    };

    match memory::confirm_decision(db, decision_id.clone(), project).await {
        Ok(_) => text_content(format!("Successfully confirmed decision {}", decision_id)),
        Err(e) => error_content(format!("confirm_decision failed: {}", e)),
    }
}


async fn dispatch_pre_push_check(args: &Value, config: &Arc<GlobalConfig>) -> Value {
    let project_path = match args["project_path"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: project_path"),
    };
    let target = match args["target"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: target"),
    };

    match gatekeeper::pre_push_check(&project_path, &target, config).await {
        Ok(result) => {
            let status_line = if result.passed {
                format!("✅ All checks passed for target '{}'", result.target)
            } else {
                format!("❌ Checks FAILED for target '{}'", result.target)
            };

            let check_lines: Vec<String> = result
                .checks
                .iter()
                .map(|c| {
                    let icon = if c.passed { "✅" } else { "❌" };
                    let code = c
                        .exit_code
                        .map(|n| format!(" (exit {})", n))
                        .unwrap_or_default();
                    let out = if c.output.trim().is_empty() {
                        String::new()
                    } else {
                        format!("\n```\n{}\n```", c.output.trim())
                    };
                    format!("{} `{}`{}{}", icon, c.command, code, out)
                })
                .collect();

            let msg = format!("{}\n\n{}", status_line, check_lines.join("\n\n"));

            if result.passed { text_content(msg) } else { error_content(msg) }
        }
        Err(e) => error_content(format!("pre_push_check failed: {}", e)),
    }
}

async fn dispatch_lock_file(args: &Value, db: &Arc<KlaarDb>, config: &Arc<GlobalConfig>) -> Value {
    let file_path = match args["file_path"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: file_path"),
    };
    let agent_id = match args["agent_id"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: agent_id"),
    };

    // R4-H1: Enforce allowed_roots boundary — lock_file must not operate outside the sandbox
    let path = std::path::Path::new(&file_path);
    if let Err(e) = crate::config::check_path_allowed(path, &config.allowed_roots, "lock_file", "allowed_roots", config.deny_when_unconfigured) {
        return error_content(e.to_string());
    }

    match crate::db::check_lock(db, file_path.clone()).await {
        Ok(Some(lock)) => {
            if lock.agent_id != agent_id {
                return error_content(format!(
                    "ResourceLocked: File '{}' is actively locked by agent '{}' on virtual branch '{}'.",
                    file_path, lock.agent_id, lock.virtual_branch
                ));
            }
            text_content(format!("Lock already held by you for file '{}'", file_path))
        },
        Ok(None) => {
            // R4-M3: Derive project root from file path so git branch detection uses the correct CWD
            let project_root = crate::utils::find_project_root(&file_path)
                .unwrap_or_else(|| file_path.clone());
            let vb = get_current_git_branch(&project_root).await;
            match crate::db::lock_file(db, file_path.clone(), agent_id, vb).await {
                Ok(_) => text_content(format!("Successfully locked file '{}'", file_path)),
                Err(e) => error_content(format!("Failed to acquire lock: {}", e)),
            }
        },
        Err(e) => error_content(format!("Failed to verify locks: {}", e)),
    }
}

async fn dispatch_unlock_file(args: &Value, db: &Arc<KlaarDb>, config: &Arc<GlobalConfig>) -> Value {
    let file_path = match args["file_path"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: file_path"),
    };
    let agent_id = match args["agent_id"].as_str() {
        Some(s) => s.to_string(),
        None => return error_content("Missing required field: agent_id"),
    };

    // R4-H1: Enforce allowed_roots boundary — unlock_file must not operate outside the sandbox
    let path = std::path::Path::new(&file_path);
    if let Err(e) = crate::config::check_path_allowed(path, &config.allowed_roots, "unlock_file", "allowed_roots", config.deny_when_unconfigured) {
        return error_content(e.to_string());
    }

    match crate::db::unlock_file(db, file_path.clone(), agent_id).await {
        Ok(_) => text_content(format!("Successfully unlocked file '{}'", file_path)),
        Err(e) => error_content(format!("Failed to unlock file: {}", e)),
    }
}

async fn get_current_git_branch(project_root: &str) -> String {
    if let Ok(output) = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .output()
        .await
    {
        if output.status.success() {
            if let Ok(branch_raw) = String::from_utf8(output.stdout) {
                let branch = branch_raw.trim().to_string();
                if branch != "HEAD" {
                    return branch;
                }
                // Detached HEAD fallback to commit SHA
                if let Ok(sha_output) = tokio::process::Command::new("git")
                    .args(["rev-parse", "--short", "HEAD"])
                    .current_dir(project_root)
                    .output()
                    .await
                {
                    if sha_output.status.success() {
                        if let Ok(sha) = String::from_utf8(sha_output.stdout) {
                            return sha.trim().to_string();
                        }
                    }
                }
                return "HEAD".to_string();
            }
        }
    }
    "unknown".to_string()
}

static CO_CHANGE_DEBOUNCERS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>> = std::sync::OnceLock::new();

fn schedule_co_change_check(
    db: Arc<KlaarDb>,
    cache: Arc<SearchCache>,
    project_root: String,
    parsed_project: String,
) {
    let debouncers_lock = CO_CHANGE_DEBOUNCERS.get_or_init(|| {
        std::sync::Mutex::new(std::collections::HashMap::new())
    });

    let mut debouncers = debouncers_lock.lock().unwrap_or_else(|e| e.into_inner());

    // Abort the existing task for this project if any to debounce
    if let Some(existing_handle) = debouncers.remove(&project_root) {
        existing_handle.abort();
    }

    let db_task = db.clone();
    let cache_task = cache.clone();
    let project_root_clone = project_root.clone();
    let project_root_task = project_root.clone();
    let parsed_project_clone = parsed_project.clone();
    
    let handle = tokio::spawn(async move {
        // Debounce period: sleep for 2 seconds
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Drain modified files from the in-memory registry
        let modified: Vec<String> = {
            let mut map = cache_task.modified_files.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(&project_root_task)
                .unwrap_or_default()
                .into_iter()
                .collect()
        };

        // Record co-changes if there are multiple modified files
        if modified.len() > 1 {
            let _ = crate::db::record_co_changes(&db_task, &parsed_project_clone, modified).await;
        }

        // Clean up our handle from the debouncers map
        if let Some(lock) = CO_CHANGE_DEBOUNCERS.get() {
            let mut debouncers = lock.lock().unwrap_or_else(|e| e.into_inner());
            debouncers.remove(&project_root_task);
        }
    });

    debouncers.insert(project_root_clone, handle);
}


async fn dispatch_klaar_grep(
    args: &Value, 
    config: &Arc<GlobalConfig>,
    cache: &Arc<SearchCache>,
) -> Value {
    let query = match args["query"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: query"),
    };
    let project_path = match args["project_path"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: project_path"),
    };

    // Check path security boundary
    let path = std::path::Path::new(project_path);
    if let Err(e) = crate::config::check_path_allowed(path, &config.allowed_roots, "grep", "allowed_roots", config.deny_when_unconfigured) {
        return error_content(e.to_string());
    }

    // R5-H2: Canonicalize project path for cache key so watcher invalidation matches
    let canonical_project_path = std::path::Path::new(project_path)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| project_path.to_string());

    let case_insensitive = args["case_insensitive"].as_bool().unwrap_or(true);
    let include_glob = args["include_glob"].as_str();

    // Check cache first
    let cache_key = SearchCache::key_search(&canonical_project_path, query, case_insensitive, include_glob);
    if let Some(cached) = cache.content.get(&cache_key).await {
        return text_content(cached);
    }

    let query_str = query.to_string();
    let project_path_str = project_path.to_string();
    let include_glob_str = include_glob.map(|s| s.to_string());

    let search_res = tokio::task::spawn_blocking(move || {
        search::search(&query_str, &project_path_str, case_insensitive, include_glob_str.as_deref())
    }).await;

    let res = match search_res {
        Ok(r) => r,
        Err(e) => return error_content(format!("search thread join failed: {}", e)),
    };

    match res {
        Ok(results) => {
            // Populate cache
            cache.insert_search(&canonical_project_path, cache_key, results.clone()).await;
            text_content(results)
        },
        Err(e) => error_content(format!("Search failed: {}", e)),
    }
}

async fn dispatch_klaar_ls(args: &Value, config: &Arc<GlobalConfig>) -> Value {
    let project_path = match args["project_path"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: project_path"),
    };

    // Check path security boundary
    let path = std::path::Path::new(project_path);
    if let Err(e) = crate::config::check_path_allowed(path, &config.allowed_roots, "ls", "allowed_roots", config.deny_when_unconfigured) {
        return error_content(e.to_string());
    }

    let max_depth = args["max_depth"].as_u64().map(|d| d as usize);

    let project_path_str = project_path.to_string();
    let ls_res = tokio::task::spawn_blocking(move || {
        ls::list_dir(&project_path_str, max_depth)
    }).await;

    let res = match ls_res {
        Ok(r) => r,
        Err(e) => return error_content(format!("list_dir thread join failed: {}", e)),
    };

    match res {
        Ok(results) => text_content(results),
        Err(e) => error_content(format!("Listing failed: {}", e)),
    }
}

async fn dispatch_klaar_find_symbol(
    args: &Value, 
    config: &Arc<GlobalConfig>,
    cache: &Arc<SearchCache>,
) -> Value {
    let symbol_name = match args["symbol"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: symbol"),
    };
    let project_path = match args["project_path"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: project_path"),
    };

    // Check path security boundary
    let path = std::path::Path::new(project_path);
    if let Err(e) = crate::config::check_path_allowed(path, &config.allowed_roots, "find_symbol", "allowed_roots", config.deny_when_unconfigured) {
        return error_content(e.to_string());
    }

    // R5-H2+M3: Canonicalize project path for cache key consistency with watcher invalidation
    let canonical_project_path = std::path::Path::new(project_path)
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| project_path.to_string());

    // Get or build index
    let index = if let Some(cached) = cache.symbol_index.get(&canonical_project_path).await {
        cached
    } else {
        info!("Building symbol index for: {}", project_path);
        let project_path_str = project_path.to_string();
        let build_res = tokio::task::spawn_blocking(move || {
            crate::tools::index::build_index(&project_path_str)
        }).await;
        
        let new_index = match build_res {
            Ok(idx) => Arc::new(idx),
            Err(e) => return error_content(format!("build_index thread join failed: {}", e)),
        };
        
        cache.symbol_index.insert(canonical_project_path, new_index.clone()).await;
        new_index
    };

    if let Some(locations) = index.symbols.get(symbol_name) {
        let mut out = String::new();
        out.push_str(&format!("Found {} matches for `{}`:\n", locations.len(), symbol_name));
        for loc in locations {
            out.push_str(&format!("  {} → {}:L{}\n", loc.kind, loc.file, loc.line));
        }
        text_content(out)
    } else {
        text_content(format!("Symbol `{}` not found in project.", symbol_name))
    }
}

async fn dispatch_get_co_changes(args: &Value, db: &Arc<KlaarDb>) -> Value {
    let project = match args["project"].as_str() {
        Some(s) => s,
        None => return error_content("Missing required field: project"),
    };

    match crate::db::get_co_changes(db, project).await {
        Ok(records) => {
            if records.is_empty() {
                text_content("No co-change records found for this project.")
            } else {
                let mut out = String::new();
                out.push_str("Recent file co-change clusters:\n\n");
                for rec in records {
                    out.push_str(&format!(
                        "- Files modified together at {}:\n",
                        rec.created_at.to_rfc3339()
                    ));
                    for file in rec.files {
                        out.push_str(&format!("  * {}\n", file));
                    }
                    out.push('\n');
                }
                text_content(out)
            }
        }
        Err(e) => error_content(format!("Failed to retrieve co-changes: {}", e)),
    }
}
