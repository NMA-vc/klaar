# Contributing to klaar

We are excited that you want to contribute to **klaar**! Whether you are fixing bugs, proposing performance optimizations, or improving documentation, your contributions help make `klaar` the best optimizer for AI coding agents.

This guide walks you through the local setup, coding standards, and our pull request lifecycle.

---

## 1. Getting Started

### Prerequisites
* **Rust Toolchain:** You need the stable Rust compiler and package manager. Install it via [rustup.rs](https://rustup.rs).
* **Git:** For version control.

### Setup Instructions
1. Clone the repository:
   ```bash
   git clone https://github.com/NMA-vc/klaar.git
   cd klaar
   ```
2. Run tests to ensure your baseline environment is correct:
   ```bash
   cargo test
   ```
3. Run the compiler linter to check standard formatting:
   ```bash
   cargo clippy --all-targets --all-features -- -D warnings
   ```

### Debuggable Release Builds

The default release profile strips debug symbols for binary size. To produce a release build with symbols intact (useful for crash reports and profiling):

```bash
cargo build --profile release-debug
```

---

## 2. Project Architecture

Before making changes, it is helpful to understand `klaar`'s internal structure under `src/`:
* `main.rs`: The entry point, setting up the stdin/stdout connection loop, active semaphores, and starting the recursive file watchers.
* `cache/`: Core in-memory async `moka` cache and targeted prefix indices (`mod.rs`). Manages the `notify` file watcher loop, atomic overflow flagging, and strict project caps (`watcher.rs`).
* `config/`: Configuration manager for the secure fail-closed sandbox `config.toml` and localized `.klaar/targets.toml` templates.
* `mop/` / `tools/`: The Model Context Protocol (MCP) server dispatcher and specific tool implementations (`surgical_read`, `fast_apply`, `pre_push_check`, symbols, etc.).

---

## 3. Coding Guidelines & Standards

To maintain code health and reliability, all contributions must adhere to these three rules:

### Rule A: Never Block the Async Executor
`klaar` runs on a high-throughput multi-threaded Tokio runtime. Synchronous I/O or intensive CPU tasks (e.g. tree-sitter symbol indexing, disk reading, diff calculation) must never be executed directly inside async functions. Wrap them in blocking tasks:
```rust
// Correct offloading
let result = tokio::task::spawn_blocking(move || {
    // Perform intensive disk or CPU operation
}).await?;
```

### Rule B: Strict Secure defaults
Never bypass allowed boundaries (`check_path_allowed`). If you add any tool or feature that reads/writes files or executes commands, you must pass in `GlobalConfig` constraints and enforce path sandboxing. Ensure the posture defaults to a fail-closed deny when unconfigured.

### Rule C: Warning-Free Compliance
All submitted code must compile without *any* compiler or clippy warnings. We enforce this in CI:
```bash
cargo clippy --all-targets --all-features -- -D warnings
```

---

## 4. Submitting a Pull Request

1. **Create a branch:** Create a descriptive feature branch from `main`:
   ```bash
   git checkout -b feature/my-cool-optimization
   ```
2. **Commit changes:** Write clean, modular code and commit. Keep commit messages clear and professional.
3. **Write tests:** Add tests for your code. If you modified cache, watch, or apply layers, add corresponding tests under `tests/` or in unit-test blocks.
4. **Run Verification:** Ensure tests, clippy, and audits pass:
   ```bash
   cargo test
   cargo clippy --all-targets --all-features -- -D warnings
   cargo audit
   ```
5. **Open a PR:** Open a pull request against the `main` branch of `NMA-vc/klaar` on GitHub. Document what the PR changes, the problem solved, and any performance impacts.
