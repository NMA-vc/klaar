# klaar

> **klaar** *(plattdüütsch/low german) — "clear, ready" — as in "klaar kommen" (to manage, to handle).* A universal AI coding agent optimizer that makes agents **klaar** to work.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Language: Rust](https://img.shields.io/badge/Language-Rust-orange.svg)](https://www.rust-lang.org/)
[![Runtime: Local-first](https://img.shields.io/badge/Runtime-Local--first-brightgreen.svg)](#why-klaar)

`klaar` is a single-binary, local Model Context Protocol (MCP) server that equips AI coding agents (such as Google Antigravity, Claude Code, or any MCP-compatible environment) with persistent, hybrid-search memory and localized development guardrails. Running local-first with zero runtime network dependencies (model weights are downloaded once on first run if semantic-search is enabled), it embeds an intelligence suite directly inside the agent's stdio lifecycle.

---

## 🚀 Why klaar?

AI coding agents are powerful, but they suffer from significant operational inefficiencies:
* **Context Bloat:** They read entire 1000-line files to locate a single method signature.
* **Aggressive Rewrites:** They rewrite whole files to apply a 3-line bug fix, wasting tokens and causing compile errors.
* **Amnesia:** They forget design decisions, auth strategies, and conventions between conversation sessions.
* **Brittle Pushes:** They attempt to push code to remote branches without running local verification, breaking CI/CD pipelines.

**`klaar` solves all four problems natively** with 13 MCP tools and local utilities:

| Feature Area | MCP Tool | What it does | Expected ROI |
|---|---|---|---|
| **Context Compression** | `surgical_read` | Reads files in `skeleton`, `compressed`, or `full` mode to reduce unnecessary context. | Lower token usage for navigation-heavy tasks |
| **Localized Patching** | `fast_apply` | Validates and applies bottom-up line hunks without rewriting entire files. | Smaller edits, fewer rewrite regressions |
| **Hybrid Memory Recall** | `recall_memory` | Runs SurrealDB hybrid semantic + lexical retrieval fused via Reciprocal Rank Fusion (RRF). | Better recall across semantic and exact-match queries |
| **Decision Intelligence** | `record_decision` / `get_why` | Tracks architectural decisions, file scopes, and confidence decay as code changes. | Less decision drift across sessions |
| **CI/CD Guardrails** | `pre_push_check` | Runs target checks from `.klaar/targets.toml` with timeout and bounded output. | Earlier failure detection before push |

---

## 📊 Token ROI Tracking

`klaar stats` reports token savings and usage from its embedded ledger. Example output:

```text
AGENT                     | PROJECT                   |    TOKENS SAVED |    TOTAL TOKENS |  % SAVED
--------------------------+---------------------------+-----------------+-----------------+---------
antigravity               | my-project                |           11962 |           12422 |    96.3%
antigravity-system        | klaar_internal            |            2951 |               0 |     0.0%
antigravity-system        | my-project                |            9822 |            5187 |   189.4%
planning-agent            | my-project                |            3490 |            3638 |    95.9%
copilot-agent             | my-project                |             912 |            1005 |    90.7%
--------------------------+---------------------------+-----------------+-----------------+---------
TOTAL                     |                           |           29409 |           22616 |   130.0%
```

> [!NOTE]
> This is an example snapshot. Actual savings depend on repository shape, agent behavior, and tool usage patterns.
> **Disclaimer:** These ROI numbers are recorded from specific, internal development sessions under targeted agent workflows. They are illustrative of potential savings and do not represent controlled, standardized open-source benchmarks.
> 
> [!TIP]
> **How savings are tracked:** `klaar` calculates raw token savings by measuring the difference between the full source file and the compressed/skeleton stream returned. Real-world savings vary depending on whether the agent requests follow-up reads for full files.

---

## 🛠️ Core Upgrades & Architectural Pillars

### 1. Decision Intelligence (Repowise-Inspired)
Unlike flat keyword-based memory stores, `klaar` links architectural decisions directly to the specific source files they govern.
* **Confidence Decay:** As governed files are modified via `fast_apply`, their governing decisions automatically decay in confidence (by `0.1` per edit).
* **Self-Healing Verification:** When an agent inspects files, it calls `get_why()` to retrieve active decisions. Once it verifies they are still valid, it calls `confirm_decision()` to restore the confidence score to `1.0`.
* **Co-Change Tracker:** Tracks groups of files frequently committed together, alerting agents to cross-component implications.

### 2. Native Hybrid Search with Reciprocal Rank Fusion (RRF)
`klaar` integrates local vector embeddings with lexical BM25 full-text indexing, fusing results using mathematical Reciprocal Rank Fusion (RRF):
* **Semantic Layer:** Generates 384-dimensional vector embeddings locally using the ONNX-backed `BGESmallENV15` model (no external API keys; first run may download model weights).
* **Lexical Layer:** Computes full-text search scores using a BM25 snowball english stemming analyzer.
* **Rust Fusion Engine:** Results are merged server-side using the standard RRF formula:
  $$Score = \sum_{d \in D} \frac{1.0}{60.0 + Rank_d}$$
  This automatically balances conceptual matching with exact symbol names, error codes, and line numbers.

### 3. Asynchronous Cache & Proactive Invalidation
* Serves repeated file reads and symbols via an asynchronous `moka` cache with $O(1)$ key lookups.
* Backed by a recursive `notify` filesystem watcher that proactively invalidates cache entries the moment external edits occur.

---

## 📦 Installation & Setup

### Prerequisites
* Rust toolchain (`rustup.rs`)

### 1. Run the Installer
```bash
git clone https://github.com/NMA-vc/klaar.git
cd klaar
./install/setup.sh
```

The installer will:
1. Compile the codebase in fully optimized release mode (`cargo build --release`).
2. Copy the binary to `/usr/local/bin` (uses `sudo` when needed).
3. Automatically detect and register the MCP server with your active AI environments:
   - **Google Antigravity:** `~/.gemini/antigravity/mcp_config.json`
   - **Claude Code:** `~/.config/claude/claude_desktop_config.json`

### 2. Initialize a Project
To configure `klaar` checks inside your coding project:
```bash
klaar install --project /path/to/your-project
```
This generates a localized `.klaar/targets.toml` file in your root folder.

---

## ⚙️ Configuration Reference

### Project Config (`.klaar/targets.toml`)
Placed in your project root, this defines test gates and deploy pathways per target:

```toml
[project]
name = "my-rust-app"
language = "rust" # "rust" | "typescript" | "mixed"

[[targets]]
name = "production"
pre_push_checks = [
  "cargo check --workspace",
  "cargo clippy -- -D warnings",
  "cargo test --workspace"
]
deploy_command = "dokploy deploy --app my-app-prod"
```

### Global Config (`~/.config/klaar/config.toml`)
Customizes path sandboxing, memory locations, and trusted execution limits:

```toml
db_path          = "~/.local/share/klaar/db" # default location
log_level        = "info"                    # logging filter
allowed_roots    = ["/Users/username/projects"]  # Sandbox file reads/writes
trusted_projects = ["/Users/username/projects/my-project", "/Users/username/projects/klaar"] # Paths allowed to run checks
deny_when_unconfigured = true                # Secure-by-default fail-closed setting
```

---

## 🔒 Security & Sandboxing Guidelines

`klaar` enforces a **secure-by-default, fail-closed** sandboxing model to ensure that AI agents only read/write files and execute commands in designated directories.

### Fail-Closed Default (`deny_when_unconfigured = true`)
If the list of `allowed_roots` or `trusted_projects` is empty or the configuration file is missing, `klaar` will **deny all access requests** and throw an error. This prevents newly installed agents from reading sensitive system directories or running random push commands before you have explicitly authorized them.

### Step-by-Step Security Setup
To authorize an agent to work on your projects:
1. Open the global config file `~/.config/klaar/config.toml`.
2. Add your development parent directory to `allowed_roots` (this enables `surgical_read`, `fast_apply`, `grep`, and `ls` operations):
   ```toml
   allowed_roots = ["/Users/yourname/Projects"]
   ```
3. Add the specific repositories where you want `klaar` to run tests and pre-push hooks to `trusted_projects`:
   ```toml
   trusted_projects = [
     "/Users/yourname/Projects/my-rust-app",
     "/Users/yourname/Projects/klaar"
   ]
   ```
4. If you intentionally want to turn off the sandbox and fail-open with warnings (not recommended for production), you can explicitly set:
   ```toml
   deny_when_unconfigured = false
   ```

> **Note:** `trusted_projects` is an MCP guardrail — it restricts which repositories AI agents can execute commands in via the `pre_push_check` tool. It provides no additional security boundary for direct CLI invocations by the local user, who already has full shell access.

For detailed disclosures and threat models, see [SECURITY.md](SECURITY.md).

---

## 🧰 MCP Tool Specifications

### `surgical_read`
Reads a file in `skeleton`, `compressed`, or `full` mode.
* **Request:** `{ "file_path": "/src/main.rs", "mode": "skeleton", "agent_id": "agent-1" }`
* **Response:** Signature-focused or compressed content to reduce context bloat.

### `fast_apply`
Applies discrete hunks of edits, validating boundaries bottom-up.
* **Request:** `{ "file_path": "/src/lib.rs", "hunks": [{ "line_start": 10, "line_end": 12, "new_content": "let x = 42;" }], "agent_id": "agent-1" }`

### `store_memory`
Saves a structured memory inside the local SurrealDB instance.
* **Request:** `{ "key": "jwt-cookie", "content": "Cookie: __Secure-authjs.session-token", "tags": ["jwt", "cookie"], "project": "my-project" }`

### `recall_memory`
Performs fused vector + BM25 RRF search.
* **Request:** `{ "query": "cookie authentication", "project": "my-project", "limit": 3 }`

### `grep`
Runs a token-optimized codebase search, grouping matches by file and stripping redundant whitespace.
* **Request:** `{ "query": "surgical_read", "project_path": "/Users/username/projects/klaar", "case_insensitive": true, "include_glob": "*.rs" }`

### `ls`
Noise-filtered tree-based directory listing, automatically excluding target, node_modules, and similar artifacts.
* **Request:** `{ "project_path": "/Users/username/projects/klaar", "max_depth": 2 }`

### `find_symbol`
Fast definition lookup across the project using tree-sitter indices.
* **Request:** `{ "symbol": "surgical_read", "project_path": "/Users/username/projects/klaar" }`

---

## 🔍 Trust, Security, & Testing Disclosures

### Known Security Advisories & Policy Exceptions
* **`RUSTSEC-2026-0002` (Transitive `lru` Unsoundness):** This warning is policy-ignored inside `.cargo/audit.toml`. It is a transitive dependency of the `fastembed` library. The unsoundness refers to potential concurrent race conditions in thread-unsafe `lru` operations. Because `klaar` executes the embedder in isolated tasks with strictly controlled single-threaded access patterns, this is non-exploitable in our host runtime environment. Downstream users requiring a 100% warning-free supply chain out of the box can build `klaar` without its default semantic search feature (`cargo build --release --no-default-features`).

### Test Coverage & Integration Verification
* **Test Coverage Gap:** While `CONTRIBUTING.md` establishes a target of 100% coverage for newly added logic, our current core integration test suite has a thin baseline (7 comprehensive end-to-end integration and stress tests). Expanding our integration and load testing matrix across all dispatchers is a high-priority item on our public roadmap, and we actively welcome community contributions in this area!

---

## 📄 License

Dual-licensed under either:
* **MIT License** ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
* **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

Choose the license that best fits your development or corporate integration goals.
