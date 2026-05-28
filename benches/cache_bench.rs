use criterion::{criterion_group, criterion_main, Criterion};
use std::sync::Arc;
use tokio::runtime::Runtime;

use klaar::cache::SearchCache;

fn bench_cache_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let cache = Arc::new(SearchCache::new());

    // Benchmark O(1) Cache Reads
    c.bench_function("cache_get_hit", |b| {
        // Pre-populate a key
        let project_path = "/tmp/benchmark-project";
        let key = SearchCache::key_search(project_path, "query", false, None);
        rt.block_on(async {
            cache.insert_search(project_path, key.clone(), "cached_data".to_string()).await;
        });

        b.iter(|| {
            let res = rt.block_on(async {
                cache.content.get(&key).await
            });
            assert!(res.is_some());
        });
    });

    // Benchmark O(1) Cache Inserts
    c.bench_function("cache_insert_search", |b| {
        let project_path = "/tmp/benchmark-project";
        let mut count = 0;

        b.iter(|| {
            count += 1;
            let key = SearchCache::key_search(project_path, &format!("query_{}", count), false, None);
            let cache_clone = cache.clone();
            rt.block_on(async move {
                cache_clone.insert_search(project_path, key, "result".to_string()).await;
            });
        });
    });
}

criterion_group!(benches, bench_cache_operations);
criterion_main!(benches);
