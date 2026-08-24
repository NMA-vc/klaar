# Wiki Performance Evidence

This document records reproducible measurements for the compiled repository wiki. It is
evidence for the Klaar 1.0.0 release candidate, not a universal latency guarantee.
Repository shape, filesystem, generator choice, lenses, and compiler profile affect results.
The raw observations are committed in
[`performance/wiki-2026-08-24.csv`](performance/wiki-2026-08-24.csv).

## Environment

Measured on 2026-08-24 after the release-hardening source changes and before evidence-only
documentation updates.

| Variable | Value |
| --- | --- |
| Machine | Apple M4, 16 GiB RAM |
| Operating system | macOS 26.3 (25D125), arm64 |
| Rust | `rustc 1.89.0 (29483883e 2025-08-04)` |
| Klaar | `1.0.0`, 44 MiB stripped binary |
| Build | `--release --no-default-features --locked`, thin LTO, 16 codegen units |
| Clean release build | 20m29s on this busy development host |
| Renderer | Built-in deterministic `FactRenderer` |
| Lenses | `technical`, `executive` |

The default semantic-search feature was disabled to isolate wiki behavior and avoid model
setup. It is not used by wiki compilation. The clean build time is observational: other
compilation workloads were active, so it is not a controlled build-time benchmark.

## Scenarios

Each scenario used five fresh repositories on the same local filesystem. `initial` started
with configuration but no compiled state, `unchanged` immediately repeated the build, and
`incremental` changed one existing source file. Every measured build ran in a new process.

| Scenario | Repository shape | Planned renders |
| --- | --- | ---: |
| Small | Rust fixture, 3 source files | 14 |
| Medium | Mixed TypeScript/Go monorepo fixture | 18 |
| Large | Synthetic 20-component repository, 1,000 Rust source files | 46 |

Wall time and peak RSS use `/usr/bin/time -l`. Values are medians; parentheses show the
full observed range. Evidence bytes count bytes delivered across generator calls, not all
repository facts scanned to establish freshness.

| Scenario | Phase | Wall time | Peak RSS max | Rendered / reused | Generator calls | Generator evidence |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Small | Initial | 0.09 s (0.07-0.09) | 14.3 MiB | 14 / 0 | 14 | 10,388 B |
| Small | Unchanged | 0.00 s (0.00-0.01) | 14.2 MiB | 0 / 14 | 0 | 0 B |
| Small | Incremental | 0.07 s (0.06-0.11) | 14.4 MiB | 8 / 6 | 8 | 8,468 B |
| Medium | Initial | 0.10 s (0.10-0.11) | 14.6 MiB | 18 / 0 | 18 | 19,386 B |
| Medium | Unchanged | 0.00 s (0.00-0.01) | 14.2 MiB | 0 / 18 | 0 | 0 B |
| Medium | Incremental | 0.06 s (0.06-0.07) | 14.5 MiB | 8 / 10 | 8 | 12,218 B |
| Large | Initial | 0.33 s (0.31-0.36) | 24.0 MiB | 46 / 0 | 46 | 564,050 B |
| Large | Unchanged | 0.09 s (0.08-0.14) | 29.1 MiB | 0 / 46 | 0 | 0 B |
| Large | Incremental | 0.12 s (0.11-0.15) | 29.5 MiB | 6 / 40 | 6 | 89,006 B |

For the large scenario, localized refresh reduced median wall time by 64%, generator calls
by 87%, and generator evidence by 84% versus a full build. The unchanged path made no
generator calls and rewrote no byte-identical artifact, but it still rescanned and hashed
repository facts. It is O(input bytes), not O(1).

Peak RSS for the large no-op and incremental runs exceeded the initial-build process by
about 5 MiB. The implementation meets its bounds, but the data does not support claiming
that incremental refresh always lowers peak memory.

## Reproduce

Build the measured feature set in an isolated target directory:

```bash
RUSTC_WRAPPER= CARGO_TARGET_DIR=/tmp/klaar-wiki-perf-target \
  cargo build --locked --release --no-default-features --bin klaar
```

Use a fresh repository, retain `.klaar/wiki.toml`, and leave `.klaar/wiki` and
`.klaar/wiki-state` absent for the initial measurement:

```bash
BIN=/tmp/klaar-wiki-perf-target/release/klaar
/usr/bin/time -l "$BIN" wiki build --project . --json
/usr/bin/time -l "$BIN" wiki build --project . --json
printf '\n// localized benchmark change\n' >> path/to/existing/source.rs
/usr/bin/time -l "$BIN" wiki build --project . --json
"$BIN" wiki status --project . --json
```

Record `rendered_pages`, `reused_pages`, `generator_calls`, and `evidence_bytes` from each
JSON report. Use at least five fresh copies and report medians plus ranges; do not compare a
warm in-process library call with a cold CLI process.

Microbenchmarks for configuration parsing, path normalization, Markdown validation,
secret scanning, manifest serialization, fact scanning, and no-op builds are available
through:

```bash
cargo bench --locked --no-default-features --bench wiki_bench
```

Criterion output depends on the host and is intentionally not checked in. CI compiles the
benchmark and treats correctness and hard bounds as gates; performance regressions require
repeated measurements on a stable runner.

## Interpretation

- These subsecond results use the local deterministic renderer. An explicitly configured
  external generator can dominate latency and must be measured separately.
- A localized change can affect index, architecture, dependency, and component pages. The
  exact fanout follows provenance and repository topology.
- The 64-item evidence cap and per-page byte budgets bound compiler work. Diagnostics
  report omitted or truncated evidence rather than silently expanding work.
- The large scenario is synthetic and tests scale shape, not real-world documentation
  quality. The medium fixture supplies the cross-language monorepo behavior check.
