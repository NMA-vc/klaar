# Changelog

All notable changes to the **klaar** project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

## [1.0.0] - 2026-08-24
### Added
* Added the `diagram_codebase` MCP tool for bounded Mermaid `overview`, `dependencies`, and `hotspots` views inferred from local source imports and module declarations.
* Added project-scoped diagram caching with filesystem-watcher invalidation, path sandbox enforcement, scan limits, safe Mermaid labels, and focused graph slices.
* Added the local compiled wiki with technical/executive lenses, deterministic facts and provenance, no-op builds, Git-aware refresh/check/status commands, watcher stale marking, read-only localhost viewer, and eight MCP tools.
* Added bounded fact-renderer, MCP-assisted, and headless-command generation paths behind one secret-aware Markdown/Mermaid compiler.
* Added versioned repository exports and a deterministic Realm federation prototype.

### Changed
* Declared Rust 1.89 as the minimum supported toolchain and aligned package, binary, manifest, and tag versions.
* Added cross-platform tests, committed-wiki quality verification, native release smoke tests, checksums, signatures, SBOMs, and optional protected crates.io publication.
* Replaced full single-unit LTO with thin LTO and parallel code generation to keep release builds practical for source installers.

The 0.8.1-0.8.10 entries below describe internal development snapshots. They were not
published as Git tags or crates.io releases; `v0.8.0` is the only pre-1.0 repository tag.

## [0.8.10] - 2026-05-27
### Added
* **Strict Project-Key Capping (Memory Protection):** Enforced a strict maximum limit of **20 active project keys** within the in-memory `modified_files` tracking registry, preventing slow memory growth over extremely long process lifetimes.
* **Targeted Project Volatile Invalidation:** Introduced `clear_volatile_for_active_projects` to evict only the active storming projects' search cache keys during filesystem watcher queue overflows, preserving unaffected project caches and diff baselines.

### Changed
* Standardized codebase branding to lowercase **`klaar`** everywhere.
* Updated default repository URLs to `https://github.com/NMA-vc/klaar`.

---

## [0.8.9] - 2026-05-22
### Added
* Enforced a memory protection cap of **50 modified files** per project root inside the in-memory changes registry to prevent OOM bugs from external folder updates.
* Documented optional zero-warnings supply chain builds without `semantic-search` features (`--no-default-features`).

---

## [0.8.8] - 2026-05-19
### Added
* **O(1) Constant-Time Eviction Registry:** Replaced high-overhead O(N) substring scanning loops during file invalidation with direct hash registries: `search_keys` (mapping projects to cache keys) and `last_sent_agents` (mapping files to agent IDs), accelerating file-change cache clears to constant time.
* Upgraded file watcher overflows to cleanly wipe the diff-aware `last_sent` caches, resolving consistency errors.

---

## [0.8.7] - 2026-05-15
### Added
* **Panic-Free Watcher Overflow Handling:** Replaced unsafe asynchronous `tokio::spawn` calls inside the synchronous `notify` system callback with thread-safe atomic flags (`overflow_flag`).
* **Robust Fail-Closed Sandbox by Default:** Re-aligned security boundary checks to treat `deny_when_unconfigured = Option<bool>` as `true` by default when unset, converting `klaar` to a fail-closed secure default sandbox.

---

## [0.8.6] - 2026-05-10
### Added
* Bounded the filesystem watcher event channel with `mpsc::channel(100)` to control active queue memory usage.
* Implemented a self-healing fallback that purges volatile search caches during queue overflows.

---

## [0.8.5] - 2026-05-02
### Added
* Path boundary sandboxing checks (`check_path_allowed`) for `klaar_grep`, `klaar_ls`, `klaar_find_symbol`, and `auto_watch` watchers.
* Pre-canonicalized allowed roots at startup, resolving performance bottlenecks.

---

## [0.8.4] - 2026-04-27
### Added
* **Zero-Blocking Async Executions:** Offloaded CPU-bound symbol building, diff generation, and I/O reads onto `tokio::task::spawn_blocking`.
* Concurrency semaphore capping concurrent stdio connection queries to 8 to manage database backpressure.
* Single-pass `compute_diff_stats` hunk engine combining ratio calculation and diff construction in a single sweep.

---

## [0.8.3] - 2026-04-22
### Added
* **Native Hybrid Search & RRF:** Fused local vector embeddings (BGESmallENV15) and lexical BM25 database indices using standard Reciprocal Rank Fusion.
* Namespaced KNN embedding subquery parsing workaround for SurrealDB v2.

---

## [0.8.2] - 2026-04-21
### Added
* **Decision Intelligence Layer:** Implemented governed architectural decisions linked directly to source files.
* Confidence score decay on edits and self-healing validation controls (`record_decision`, `get_why`, `confirm_decision`).

---

## [0.8.1] - 2026-04-21
### Added
* Initial release of `klaar` featuring diff-aware hunks compression, tree-sitter symbol indexing, and local filesystem watcher.
