use criterion::{criterion_group, criterion_main, Criterion};

use klaar::utils::compute_diff_stats;

fn bench_diff_generation(c: &mut Criterion) {
    // 1. Short file shapes
    let old_short = "fn main() {\n    println!(\"hello old\");\n}\n";
    let new_short = "fn main() {\n    println!(\"hello new!\");\n}\n";

    c.bench_function("diff_short_files", |b| {
        b.iter(|| {
            let res = compute_diff_stats(old_short, new_short);
            assert!(res.ratio >= 0.0);
        });
    });

    // 2. Long file shapes (1,000 lines)
    let mut old_long_lines = Vec::new();
    let mut new_long_lines = Vec::new();
    for i in 0..1000 {
        if i % 10 == 0 {
            old_long_lines.push(format!("line {} - old text content", i));
            new_long_lines.push(format!("line {} - new text content", i));
        } else {
            old_long_lines.push(format!("line {} - stable unchanged content", i));
            new_long_lines.push(format!("line {} - stable unchanged content", i));
        }
    }
    let old_long = old_long_lines.join("\n");
    let new_long = new_long_lines.join("\n");

    c.bench_function("diff_long_files_1000_lines", |b| {
        b.iter(|| {
            let res = compute_diff_stats(&old_long, &new_long);
            assert!(res.ratio >= 0.0);
        });
    });
}

criterion_group!(benches, bench_diff_generation);
criterion_main!(benches);
