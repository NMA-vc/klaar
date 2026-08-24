# Wiki Threat Model

## Trust Boundaries

Repository files, comments, trusted documents, generated drafts, Markdown, Mermaid,
and Git path output are untrusted. Global user configuration and the local operating
system account are trusted. Repository configuration is constrained and cannot select
commands, credentials, endpoints, or paths outside `.klaar`.

Klaar is the deterministic control plane. A generator receives one typed request with
opaque evidence IDs and bounded `quoted_data`. The protocol grants no internal tools
and no ability to select input files, page IDs, or output paths.
An explicitly configured headless executable still runs with the local user's OS
permissions and is therefore trusted code, not a sandboxed model process.

## Mitigations

### Reads and paths

- CLI and MCP operations enforce global `allowed_roots`.
- Inputs are canonicalized, remain below the repository root, and reject symlink
  escapes, binaries, oversized files, generated/vendor directories, credential-like
  paths, `.git`, and `.klaar` artifacts.
- Trusted-document and exclude globs are bounded. Excludes can only narrow input.
- Git revisions and NUL-delimited paths are validated; Git runs directly without a
  shell, timeout is 30 seconds, and output is capped at 4 MiB.

### Secrets and prompt injection

- Marker-based secret detection runs before evidence enters a generator and again on
  generated Markdown.
- Findings report categories and evidence identity without echoing the suspected value.
- Repository text is framed as `quoted_data` under a fixed instruction stating that it
  has no authority.
- Draft claims and citations may reference only evidence supplied for the deterministic
  page plan.

### Output and state

- The compiler owns front matter, page identity, lens path, citations, and output path.
- It rejects active HTML, unsafe URL schemes, parent links, model front matter,
  unsupported Mermaid directives, unterminated Mermaid, secrets, and oversized output.
- All drafts are compiled before writes. Artifacts commit first and the versioned
  manifest commits last. Temporary files are synced and atomically renamed.
- A repository-scoped lock rejects concurrent builds. Staging recovery promotes or
  removes incomplete files on the next operation.
- Generated `.klaar` artifacts are excluded from scanning to prevent recursive builds.

### Viewer and MCP

- The viewer binds to loopback only, validates `Host`, supports only GET/HEAD, exposes
  no write/refresh endpoint, checks canonical artifact paths, caps files, escapes HTML,
  and sends a restrictive CSP with no remote assets.
- MCP lists, reads, plans, and submissions are bounded and execute blocking scans on
  Tokio's blocking pool.
- Evidence lookup returns provenance metadata rather than source content.

### Processes and resources

- Headless generators use an absolute executable and argument vector with no implicit
  shell. The child environment is cleared and only explicitly named variables are
  forwarded.
- JSON input is capped at 256 KiB, stdout at 512 KiB, stderr at 64 KiB, arguments at
  64/16 KiB, and timeout at 1 to 600 seconds. Timed-out children are killed.
- Repository scans cap files, per-file bytes, dependency edges, auxiliary documents,
  diagnostics, pages, evidence, and output.

## Residual Risk

- Marker-based secret scanning can miss encodings and novel credential formats or
  produce false positives. Use a dedicated external scanner for high-assurance repos.
- Static import and configured-command diagrams cannot prove runtime behavior.
- Atomicity is manifest-last per-file staging, not a filesystem-wide atomic directory
  swap. A crash can leave newer artifacts with an older manifest; the next status/build
  detects and repairs this safely.
- The headless executable is selected by trusted global configuration. Environment
  forwarding is allowlisted, but Klaar does not sandbox or authenticate the executable
  and cannot revoke the local user's filesystem permissions.
- The viewer's inert Markdown renderer intentionally does not execute Mermaid. A user
  opening portable Markdown in another renderer inherits that renderer's security.
- Cross-repository Realm inference is currently limited to versioned exports and exact
  shared source names.

## Release Evidence

Required deterministic evidence includes adversarial secrets, prompt injection,
Markdown/Mermaid, path traversal, symlink, timeout, malformed JSON, no-op, rename,
delete, shared dependency, watcher storm, lock, and recovery tests. Cross-platform CI
must cover Windows, macOS, and Linux before broad release claims.
