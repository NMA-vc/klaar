use std::sync::Arc;
use tokio::time::Duration;
use tempfile::tempdir;

use klaar::cache::SearchCache;
use klaar::cache::watcher::FileWatcher;
use klaar::db;

#[tokio::test]
async fn test_cache_overflow_recovery_targeted() {
    let cache = Arc::new(SearchCache::new());

    let proj_a = "/tmp/project-a".to_string();
    let proj_b = "/tmp/project-b".to_string();

    let key_a = SearchCache::key_search(&proj_a, "query", false, None);
    let key_b = SearchCache::key_search(&proj_b, "query", false, None);

    // 1. Insert search records into cache for both projects
    cache.insert_search(&proj_a, key_a.clone(), "result_a".to_string()).await;
    cache.insert_search(&proj_b, key_b.clone(), "result_b".to_string()).await;

    // Verify they exist
    assert_eq!(cache.content.get(&key_a).await, Some("result_a".to_string()));
    assert_eq!(cache.content.get(&key_b).await, Some("result_b".to_string()));

    // 2. Simulate modifications exclusively for Project A
    {
        let mut map = cache.modified_files.lock().unwrap();
        map.entry(proj_a.clone()).or_default().insert("/tmp/project-a/src/main.rs".to_string());
    }

    // 3. Trigger targeted eviction for active projects
    cache.clear_volatile_for_active_projects().await;

    // 4. Verify Project A's search is evicted, but Project B's is PRESERVED!
    // Note: clear_volatile_for_active_projects calls content.invalidate_all() for reads, but target-evicts search keys.
    // Let's verify search keys target invalidation.
    assert!(cache.content.get(&key_a).await.is_none());
    assert!(cache.content.get(&key_b).await.is_some());
}

#[tokio::test]
async fn test_watcher_storm_stress() {
    let temp = tempdir().expect("Failed to create tempdir");
    let temp_path = temp.path().to_str().unwrap().to_string();

    // Create a dummy project root marker .klaar so klaar's find_project_root detects it
    let klaar_dir = temp.path().join(".klaar");
    std::fs::create_dir_all(&klaar_dir).expect("Failed to create .klaar dir");

    let cache = Arc::new(SearchCache::new());
    let mut watcher = FileWatcher::new(cache.clone()).expect("Failed to initialize watcher");

    watcher.watch(&temp_path).expect("Failed to watch temp path");

    // Rapidly write 100 files in a storm
    for i in 0..100 {
        let file_path = temp.path().join(format!("storm_file_{}.txt", i));
        std::fs::write(&file_path, "content").expect("Failed to write storm file");
    }

    // Allow some time for watcher event loop to process
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check that we stayed bounded (modified files should not exceed 50 for the project root)
    let map = cache.modified_files.lock().unwrap();
    if let Some(set) = map.get(&temp_path) {
        assert!(set.len() <= 50, "Modified files set grew unbounded: {}", set.len());
    }
    assert!(map.len() <= 20, "Project keys grew unbounded: {}", map.len());
}

#[tokio::test]
async fn test_concurrent_locks() {
    let temp = tempdir().expect("Failed to create db tempdir");
    let db_path = temp.path().join("db");

    let db = db::init(&db_path).await.expect("Failed to initialize SurrealDB");

    let file_path = "/tmp/locked_file.rs".to_string();
    let agent_1 = "agent-1".to_string();
    let agent_2 = "agent-2".to_string();
    let branch = "feature-1".to_string();

    // 1. Initial lock state is empty
    let lock_state = db::check_lock(&db, file_path.clone()).await.expect("Failed to check lock");
    assert!(lock_state.is_none());

    // 2. Lock file under Agent 1
    db::lock_file(&db, file_path.clone(), agent_1.clone(), branch.clone())
        .await
        .expect("Agent 1 failed to acquire lock");

    // 3. Verify Agent 1 owns lock
    let current_lock = db::check_lock(&db, file_path.clone())
        .await
        .expect("Failed to check lock")
        .expect("Lock should exist");
    assert_eq!(current_lock.agent_id, agent_1);

    // 4. Attempt to lock under Agent 2 (should fail because of unique constraint/primary key on lock)
    let attempt_2 = db::lock_file(&db, file_path.clone(), agent_2.clone(), branch.clone()).await;
    assert!(attempt_2.is_err(), "Agent 2 should have been blocked from locking");

    // 5. Unlock the file under Agent 1
    db::unlock_file(&db, file_path.clone(), agent_1.clone())
        .await
        .expect("Failed to unlock file");

    // 6. Verify lock is clear
    let final_check = db::check_lock(&db, file_path.clone()).await.expect("Failed to check lock");
    assert!(final_check.is_none());
}

#[test]
fn test_fast_apply_overlapping_hunks_fail() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(temp.path(), "line 1\nline 2\nline 3\nline 4\nline 5\n").unwrap();

    let config = Arc::new(klaar::config::GlobalConfig {
        deny_when_unconfigured: Some(false),
        ..Default::default()
    });

    let hunks = vec![
        klaar::tools::apply::Hunk {
            line_start: 2,
            line_end: 4,
            new_content: "replaced lines 2 to 4".to_string(),
        },
        klaar::tools::apply::Hunk {
            line_start: 3,
            line_end: 5,
            new_content: "replaced lines 3 to 5".to_string(),
        },
    ];

    let file_path = temp.path().to_str().unwrap();
    let res = klaar::tools::apply::fast_apply(file_path, hunks, &config);
    assert!(res.is_err());
    let err_msg = res.unwrap_err().to_string();
    assert!(err_msg.contains("Hunks overlap"), "Expected overlap error but got: {}", err_msg);
}
