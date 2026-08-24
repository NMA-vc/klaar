# Wiki CI

Klaar provides non-interactive freshness and deterministic quality gates. CI should build
with the trusted local renderer, evaluate the result, and reject any repository diff:

```yaml
- name: Verify committed wiki and quality report
  shell: bash
  run: |
    mkdir -p "$HOME/.config/klaar"
    printf 'allowed_roots = ["%s"]\n' "$GITHUB_WORKSPACE" > "$HOME/.config/klaar/config.toml"
    cargo run --locked --no-default-features -- wiki build \
      --project "$GITHUB_WORKSPACE" --json
    cargo run --locked --no-default-features -- wiki evaluate \
      --project "$GITHUB_WORKSPACE" --json
    git diff --exit-code -- .klaar/wiki .klaar/wiki-state
```

`wiki evaluate` exits non-zero when citation validity, evidence support, core architecture
coverage, freshness, or executive-claim thresholds fail. The final diff check proves that
committed artifacts match the exact source revision being tested.

For a base/head review without regeneration, use:

```bash
klaar wiki check --project . --base "$BASE_SHA" --head "$HEAD_SHA" --json
```

Select base and head from immutable CI commit SHAs, not untrusted shell fragments. Klaar
validates revisions and passes them to `git` as arguments without a shell. CI verifies but
does not generate and push commits; run `wiki update` locally for remediation.

Deterministic tests use `FactRenderer`. Live model evaluation is opt-in and must not receive
credentials or unredacted secret findings. Benchmark runs record hardware, OS, Rust
version, feature set, repository shape, lenses, process state, medians, and observed ranges.
