# Klaar Compiled Wiki

Klaar provides a local-first repository wiki compiler. Deterministic code selects
repository inputs, page identities, evidence, lenses, output paths, and stale pages.
Generators receive one bounded evidence pack and cannot choose files or writes.

## Setup

Wiki commands enforce the global `allowed_roots` boundary. Configure the project root
in `~/.config/klaar/config.toml`, then run:

```bash
klaar wiki init --project .
klaar wiki build --project .
klaar wiki status --project .
klaar wiki serve --project . --port 8765
```

`wiki init` creates `.klaar/wiki.toml`. A build writes portable Markdown below
`.klaar/wiki/<lens>/` and versioned state below `.klaar/wiki-state/`.

## Commands

```text
klaar wiki init [--project PATH]
klaar wiki build [--project PATH] [--lens LENS] [--json]
klaar wiki update --base REV --head REV [--project PATH] [--json]
klaar wiki check --base REV --head REV [--project PATH] [--json]
klaar wiki status [--project PATH] [--json]
klaar wiki evaluate [--project PATH] [--json]
klaar wiki explain-stale PAGE_ID [--project PATH] [--json]
klaar wiki serve [--project PATH] [--port PORT]
```

`wiki evaluate` writes `.klaar/wiki-state/quality-report.json` and exits non-zero when
citations, evidence support, core architecture coverage, freshness, or executive-claim
thresholds fail. The report stores generator identities and aggregate scores only.

`build` uses the deterministic `FactRenderer`. The library also supports a bounded
headless JSON command adapter. MCP hosts can plan a build and submit typed drafts
through the same compiler boundary.

An unchanged build does not call the generator or rewrite byte-identical files. The
Git update path calculates `base..head`, reports affected canonical pages, and relies
on evidence hashes to regenerate only changed renderings.

## MCP Tools

- `wiki_status`
- `wiki_list_pages`
- `wiki_read_page`
- `wiki_get_evidence`
- `wiki_plan_build`
- `wiki_submit_page_draft`
- `wiki_explain_stale`
- `wiki_evaluate`

All tools enforce `allowed_roots`. Page reads and lists are bounded. Evidence lookup
returns provenance metadata, not source content.

## Configuration

Repository configuration may set lenses, trusted-document globs, narrowing excludes,
safe budgets, and automation policy. It cannot set credentials, executable commands,
remote endpoints, or paths outside `.klaar`.

```toml
[wiki]
enabled = true
lenses = ["technical", "executive"]
output_dir = ".klaar/wiki"
state_dir = ".klaar/wiki-state"

[wiki.budgets]
max_pages = 100
max_evidence_bytes_per_page = 131072
max_output_bytes_per_page = 32768
max_input_files = 20000

[wiki.documents]
trusted = ["README.md", "docs/**/*.md", "adr/**/*.md"]
exclude = ["**/.env*", "**/secrets/**"]

[wiki.automation]
watcher = "mark-stale"
pre_push = "check"
ci = "check"
```

Hard exclusions run before file reads and cannot be overridden. Configured excludes
only narrow input. Glob count and length, files, edges, evidence, output, process I/O,
and viewer reads all have hard bounds.

Headless generation is configured only in the global user config:

```toml
[wiki_generator]
adapter = "headless-command"
executable = "/absolute/path/to/wiki-generator"
arguments = ["--model", "local"]
timeout_seconds = 120
retries = 0
max_calls_per_build = 100
credential_environment = ["PROVIDER_API_KEY"]
```

The child process starts with a cleared environment. Only variables named in
`credential_environment` are forwarded, and their values are never copied into
repository artifacts, diagnostics, or receipts. Each retry consumes the configured
process-call budget.

## Artifacts

```text
.klaar/
  wiki.toml
  wiki/
    technical/
    executive/
  wiki-state/
    manifest.json
    repository-facts.json
    provenance.json
    quality-report.json
    source-hashes.json
```

The manifest contains canonical pages, lens renders, source hashes, build receipts,
and forward/reverse provenance. The quality report records deterministic release-gate
scores and thresholds. Timestamps are excluded from deterministic state.

## Current Limits

- Secret detection is heuristic and marker-based, not a substitute for a dedicated
  secret scanner.
- Static imports are not runtime traces. Architecture diagrams label them as static.
- The built-in fact renderer is intentionally factual but terse. Rich prose requires
  an explicitly configured headless generator or MCP-assisted drafts.
- Watcher policy `update` currently marks affected state stale; a later CLI/pre-push
  build performs regeneration.
- The viewer renders Markdown inertly and does not execute Mermaid JavaScript. Mermaid
  source remains portable in the generated Markdown.
- Realm federation is a versioned library prototype over repository exports; there is
  not yet a separate Realm CLI or ownership/dependency inference layer.

See [wiki-threat-model.md](wiki-threat-model.md) and [wiki-ci.md](wiki-ci.md).
