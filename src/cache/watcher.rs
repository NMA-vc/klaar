use notify::{Watcher, RecursiveMode, Event, Config};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;
use tokio::sync::mpsc;

use crate::cache::SearchCache;

pub struct FileWatcher {
    _cache: Arc<SearchCache>,
    watcher: notify::RecommendedWatcher,
    watched_roots: HashSet<String>,
}

impl FileWatcher {
    pub fn new(cache: Arc<SearchCache>) -> Result<Self, anyhow::Error> {
        let cache_clone = cache.clone();
        let overflow_flag = Arc::new(AtomicBool::new(false));
        let overflow_flag_cb = overflow_flag.clone();
        
        // Use a bounded channel of size 100 to prevent OOM on filesystem storms
        let (tx, mut rx) = mpsc::channel(100);

        let watcher = notify::RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    if tx.try_send(event).is_err() {
                        // Queue full under directory write/churn storm.
                        // Thread-safe and panic-free fallback: set the atomic flag to trigger a full flush.
                        overflow_flag_cb.store(true, Ordering::Release);
                    }
                }
            },
            Config::default(),
        )?;

        // Spawn the event processor
        let overflow_flag_proc = overflow_flag.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if overflow_flag_proc.swap(false, Ordering::AcqRel) {
                    tracing::warn!("Watcher event queue overflowed. Flushing volatile SearchCache for active projects once to maintain consistency.");
                    cache_clone.clear_volatile_for_active_projects().await;
                }
                for path in event.paths {
                    if let Some(path_str) = path.to_str() {
                        info!("File change detected: {}. Evicting cache.", path_str);
                        cache_clone.invalidate_path(path_str).await;

                        // Track modified file in-memory for zero-process co-change debouncer (strictly capped to 20 projects, 50 files each)
                        if let Some(project_root) = crate::utils::find_project_root(path_str) {
                            let mut map = cache_clone.modified_files.lock().unwrap_or_else(|e| e.into_inner());
                            if map.contains_key(&project_root) || map.len() < 20 {
                                let set = map.entry(project_root).or_default();
                                if set.len() < 50 {
                                    set.insert(path_str.to_string());
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            _cache: cache,
            watcher,
            watched_roots: HashSet::new(),
        })
    }

    pub fn watch(&mut self, path: &str) -> Result<(), anyhow::Error> {
        let path_owned = path.to_string();
        if self.watched_roots.contains(&path_owned) {
            return Ok(());
        }

        let p = Path::new(path);
        if !p.exists() {
            return Err(anyhow::anyhow!("Path does not exist: {}", path));
        }

        info!("Starting file watcher for: {}", path);
        self.watcher.watch(p, RecursiveMode::Recursive)?;
        self.watched_roots.insert(path_owned);
        Ok(())
    }
}
