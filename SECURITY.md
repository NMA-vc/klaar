# Security Policy & Threat Model

We take the security of **klaar** and its host environments seriously. This document defines our security threat model, explains the built-in isolation guardrails, documents our supply chain policies, and provides instructions for reporting security vulnerabilities.

---

## 1. Security Threat Model

`klaar` is designed to run locally as an MCP (Model Context Protocol) server. In this architecture, an AI agent executing instructions acts as the primary caller, while `klaar` serves as an intelligent local proxy for filesystem operations, memory storage, and code compilation.

Our security threat model specifically targets and mitigates the following attack vectors:

### A. Directory Traversal & Malicious Reads/Writes
* **Threat:** A compromised or highly hallucinated AI agent is tricked (via prompt injection or malicious code) into reading sensitive local system credentials (e.g., `~/.ssh/id_rsa`, `.env` files) or modifying system directories.
* **Mitigation (Fail-Closed Sandboxing):** 
  * `klaar` enforces strict boundary rules (`allowed_roots`).
  * If the global config (`~/.config/klaar/config.toml`) is empty or unset, `klaar` defaults to a **fail-closed** state (`deny_when_unconfigured = true`), instantly blocking all read, write, grep, ls, and symbol-finding operations.
  * All paths are fully canonicalized (resolving symlinks and parent directory aliases `..`) before testing boundaries, completely eliminating traversal hacks.

### B. Execution of Malicious Subprocesses
* **Threat:** An agent executes malicious build/install scripts or runs untrusted system commands during a test run or pre-push hook.
* **Mitigation (Trusted Projects Boundary):** 
  * Pre-push checks and deployment tasks run strictly inside pre-configured `trusted_projects` roots.
  * Command executions are wrapped inside isolated timeouts, preventing orphan process runaways.
  * Static caching of tools like `ripgrep` prevents system path injection attacks during runtime lookups.

### C. Resource Exhaustion (Denial of Service)
* **Threat:** Massive filesystem events (filesystem storms) or multiple rapid agent requests overwhelm the system, causing thread lockups, OOM (Out Of Memory) crashes, or CPU exhaustion.
* **Mitigation (Bounded Queues & Self-Healing Eviction):**
  * The file watcher utilizes a bounded event queue (`mpsc::channel(100)`) combined with a non-blocking `try_send`.
  * The non-Tokio listener thread callbacks communicate overflows through thread-safe, panic-free atomic flags (`overflow_flag.store(true, Ordering::SeqCst)`).
  * Main watcher events drain and swap the flag exactly once, performing a targeted `clear_volatile_for_active_projects` eviction.
  * The in-memory modified files tracking registry is capped at a strict upper limit of **20 active project keys** and **50 files per project**, completely preventing OOM conditions under long process lifetimes.
  * Synchronous heavy I/O operations are offloaded using `tokio::task::spawn_blocking` to prevent blocking active Tokio executors.
  * Concurrency semaphore limits active stdio connections to 8 concurrent worker tasks.

---

## 2. Dependency Audit & Supply Chain Policy

We run regular supply chain security scans via `cargo audit`. Currently, `klaar` has unmaintained transitive dependency warnings due to the upstream local vector engine `fastembed`. When the default `semantic-search` feature is active, `klaar` runs entirely locally but will perform a one-time download of ONNX model weights on its very first search invocation.

We actively document these overrides inside `.cargo/audit.toml`:
1. **`lru` (`RUSTSEC-2026-0002`)**: A transitive dependency of `surrealkv` (used internally by SurrealDB). An unsoundness advisory exists for this version of `lru`. We track upstream updates in `surrealkv`/`surrealdb` and allow it under policy because it poses no exploitable vulnerability under our localized query usage.
2. **`bincode` (`RUSTSEC-2025-0141`)**: An unmaintained serialization crate used transitively by SurrealDB. Poses no runtime execution risk under `klaar`'s internal storage constraints.
3. **`atomic-polyfill` (`RUSTSEC-2023-0089`)**: An unmaintained polyfill crate used transitively by SurrealDB. No security impact on macOS systems.
4. **`number_prefix` (`RUSTSEC-2025-0119`)**: An unmaintained crate used transitively by `fastembed` (via `indicatif`). It has no security advisories or unsoundness reports.
5. **`paste` (`RUSTSEC-2024-0436`)**: An unmaintained macro utility crate used transitively by `fastembed` (via `tokenizers`). It executes purely at build-time and introduces zero runtime execution risk.

These dependencies are regularly audited and policy-allowed. If your production environment requires a strict zero-network footprint or a 100% warning-free supply chain out-of-the-box, you may build the binary without default features:
```bash
cargo build --release --no-default-features
```
This builds `klaar` without the `semantic-search` feature (excluding `fastembed` and all its transitive dependencies) while keeping core caching, grep, fast_apply, symbols, and standard memory recall fully functional.

---

## 3. Reporting a Vulnerability

If you discover a security vulnerability in `klaar`, please **do not** open a public issue. Instead, report it privately.

Please send all security disclosures to our security team at **security@nma.vc**. 

Include the following information in your report:
* A detailed description of the vulnerability.
* Step-by-step instructions (or a proof-of-concept script) to reproduce the behavior.
* Potential impact of the issue.

We will acknowledge your report within 24 hours and coordinate a private patch and security advisory release within 7 days.
