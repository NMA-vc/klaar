use moka::future::Cache;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub mod watcher;

/// Global search and read cache for the klaar engine.
pub struct SearchCache {
    /// Key: (path, query_hash, options_hash) or just (path) for reads
    /// Value: Cached JSON output
    pub content: Cache<String, String>,

    /// Key: (agent_id, file_path)
    /// Value: last sent content (full or skeleton)
    pub last_sent: Cache<(String, String), String>,

    /// Key: project_root
    /// Value: ProjectIndex
    pub symbol_index: Cache<String, Arc<crate::tools::index::ProjectIndex>>,

    /// O(1) index: Map of project_path -> HashSet of active search cache keys in `content`
    pub search_keys: Mutex<HashMap<String, HashSet<String>>>,

    /// O(1) index: Map of file_path -> HashSet of agent_ids having active `last_sent` entries
    pub last_sent_agents: Mutex<HashMap<String, HashSet<String>>>,

    /// In-memory tracking of modified files per project root for zero-process co-change analysis
    pub modified_files: Mutex<HashMap<String, HashSet<String>>>,
}

impl Default for SearchCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchCache {
    pub fn new() -> Self {
        Self {
            content: Cache::builder()
                .max_capacity(1000)
                .time_to_live(std::time::Duration::from_secs(300))
                .build(),
            last_sent: Cache::builder()
                .max_capacity(500)
                .time_to_live(std::time::Duration::from_secs(300))
                .build(),
            symbol_index: Cache::builder()
                .max_capacity(10) // Cache up to 10 projects at a time
                .time_to_live(std::time::Duration::from_secs(300)) // 5-minute safety TTL
                .build(),
            search_keys: Mutex::new(HashMap::new()),
            last_sent_agents: Mutex::new(HashMap::new()),
            modified_files: Mutex::new(HashMap::new()),
        }
    }

    /// Generate a cache key for a search query.
    pub fn key_search(path: &str, query: &str, case_insensitive: bool, glob: Option<&str>) -> String {
        format!("search:{}:{}:{}:{}", path, query, case_insensitive, glob.unwrap_or(""))
    }

    /// Generate a cache key for a file read.
    pub fn key_read(path: &str, mode: &str) -> String {
        format!("read:{}:{}", path, mode)
    }

    /// Insert a search cache key and register it for O(1) invalidation.
    pub async fn insert_search(&self, project_path: &str, key: String, val: String) {
        {
            let mut keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.entry(project_path.to_string())
                .or_default()
                .insert(key.clone());
        }
        self.content.insert(key, val).await;
        self.compact_registries().await;
    }

    /// Insert a last_sent entry and register it for O(1) invalidation.
    pub async fn insert_last_sent(&self, agent_id: String, file_path: String, content: String) {
        let key = (agent_id.clone(), file_path.clone());
        {
            let mut map = self.last_sent_agents.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(file_path)
                .or_default()
                .insert(agent_id);
        }
        self.last_sent.insert(key, content).await;
        self.compact_registries().await;
    }

    /// Lightweight compaction GC to clean up side indexes when their total size grows
    /// due to automated Moka cache evictions under long process lifetimes.
    pub async fn compact_registries(&self) {
        let needs_compaction = {
            let keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            // R5-L4: Per-project threshold (200 keys) instead of global total
            keys.values().any(|cache_keys| cache_keys.len() > 200)
        };

        if !needs_compaction {
            return;
        }

        // Perform search keys GC
        // 1. Copy keys out of MutexGuard to avoid holding it across await points
        let keys_to_check: Vec<(String, Vec<String>)> = {
            let keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.iter()
                .map(|(proj, set)| (proj.clone(), set.iter().cloned().collect()))
                .collect()
        };

        // 2. Perform async checks without holding any locks
        let mut active_search_keys: HashMap<String, HashSet<String>> = HashMap::new();
        for (proj, cache_keys) in &keys_to_check {
            let mut active = HashSet::new();
            for k in cache_keys {
                if self.content.get(k.as_str()).await.is_some() {
                    active.insert(k.clone());
                }
            }
            if !active.is_empty() {
                active_search_keys.insert(proj.clone(), active);
            }
        }

        // 3. R5-M1: Diff-based write-back — remove only stale keys, preserving concurrent inserts
        {
            let mut keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.retain(|proj, existing_set| {
                if let Some(active_set) = active_search_keys.get(proj) {
                    // Keep only keys that are still active in Moka
                    existing_set.retain(|k| active_set.contains(k));
                    !existing_set.is_empty()
                } else {
                    // This project had no active keys from our snapshot.
                    // But it may have been freshly inserted during compaction — keep it.
                    // Only remove if the project was in our original snapshot.
                    !keys_to_check.iter().any(|(p, _)| p == proj)
                }
            });
        }

        // Perform last sent agents GC
        // 1. Copy keys out of MutexGuard to avoid holding it across await points
        let agents_to_check: Vec<(String, Vec<String>)> = {
            let map = self.last_sent_agents.lock().unwrap_or_else(|e| e.into_inner());
            map.iter()
                .map(|(file, set)| (file.clone(), set.iter().cloned().collect()))
                .collect()
        };

        // 2. Perform async checks without holding any locks
        let mut active_agents: HashMap<String, HashSet<String>> = HashMap::new();
        for (file_path, agents) in &agents_to_check {
            let mut active = HashSet::new();
            for agent_id in agents {
                let key = (agent_id.clone(), file_path.clone());
                if self.last_sent.get(&key).await.is_some() {
                    active.insert(agent_id.clone());
                }
            }
            if !active.is_empty() {
                active_agents.insert(file_path.clone(), active);
            }
        }

        // 3. R5-M1: Diff-based write-back — remove only stale keys, preserving concurrent inserts
        {
            let mut map = self.last_sent_agents.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|file_path, existing_set| {
                if let Some(active_set) = active_agents.get(file_path) {
                    existing_set.retain(|a| active_set.contains(a));
                    !existing_set.is_empty()
                } else {
                    !agents_to_check.iter().any(|(f, _)| f == file_path)
                }
            });
        }
    }

    /// Evict all cache entries related to a specific file.
    pub async fn invalidate_path(&self, path: &str) {
        // 1. Direct O(1) evictions for read cache (which is the hot path)
        self.content.invalidate(&Self::key_read(path, "skeleton")).await;
        self.content.invalidate(&Self::key_read(path, "compressed")).await;
        self.content.invalidate(&Self::key_read(path, "full")).await;
        
        // 2. Target-evict stale search cache keys when a file in that project changes
        let mut keys_to_invalidate = Vec::new();
        {
            let mut keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.retain(|project_path, cache_keys| {
                // R5-H1: Use Path::starts_with for directory-boundary-aware matching
                if Path::new(path).starts_with(Path::new(project_path)) {
                    for k in cache_keys.drain() {
                        keys_to_invalidate.push(k);
                    }
                    false // remove matched project from index
                } else {
                    true
                }
            });
        }
        for k in keys_to_invalidate {
            self.content.invalidate(&k).await;
        }
        
        // 3. Target-evict last_sent entries for this specific file in O(1)
        let mut agents_to_invalidate = Vec::new();
        {
            let mut map = self.last_sent_agents.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(agents) = map.remove(path) {
                for agent_id in agents {
                    agents_to_invalidate.push((agent_id, path.to_string()));
                }
            }
        }
        for key in agents_to_invalidate {
            self.last_sent.invalidate(&key).await;
        }

        // 4. Also invalidate symbol_index if the path is within a cached project root
        // (symbol_index only has max 10 entries, so we can do it directly)
        let path_str_idx = path.to_string();
        self.symbol_index.invalidate_entries_if(move |k, _v| {
            // R5-H1: Use Path::starts_with for directory-boundary-aware matching
            Path::new(&path_str_idx).starts_with(Path::new(k))
        }).ok();
    }

    /// Flush only volatile cache layers (reads and search results) and registries,
    /// preserving expensive symbol grammars and diff-aware agent baselines.
    pub fn clear_volatile(&self) {
        self.content.invalidate_all();
        {
            let mut keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.clear();
        }
    }

    /// Flush volatile cache layers for active projects experiencing modifications,
    /// target-evicting storming projects while preserving unaffected ones, symbols, and baselines.
    pub async fn clear_volatile_for_active_projects(&self) {
        let active_projects: Vec<(String, HashSet<String>)> = {
            let mut map = self.modified_files.lock().unwrap_or_else(|e| e.into_inner());
            map.drain().collect()
        };

        if active_projects.is_empty() {
            self.clear_volatile();
            return;
        }

        // Target-evict active projects' search keys and specific read keys in content cache
        let mut keys_to_invalidate = Vec::new();
        {
            let mut keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            for (proj, files) in &active_projects {
                if let Some(cache_keys) = keys.remove(proj) {
                    for k in cache_keys {
                        keys_to_invalidate.push(k);
                    }
                }
                for file_path in files {
                    keys_to_invalidate.push(Self::key_read(file_path, "skeleton"));
                    keys_to_invalidate.push(Self::key_read(file_path, "compressed"));
                    keys_to_invalidate.push(Self::key_read(file_path, "full"));
                }
            }
        }

        for k in keys_to_invalidate {
            self.content.invalidate(&k).await;
        }
    }

    /// Flush the entire SearchCache and all its registries/indexes to ensure clean state.
    #[allow(dead_code)]
    pub fn clear_all(&self) {
        self.content.invalidate_all();
        self.last_sent.invalidate_all();
        self.symbol_index.invalidate_all();
        {
            let mut keys = self.search_keys.lock().unwrap_or_else(|e| e.into_inner());
            keys.clear();
        }
        {
            let mut map = self.last_sent_agents.lock().unwrap_or_else(|e| e.into_inner());
            map.clear();
        }
        {
            let mut map = self.modified_files.lock().unwrap_or_else(|e| e.into_inner());
            map.clear();
        }
    }
}
