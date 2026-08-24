# Wiki Release Evidence

This file records the acceptance evidence for the Klaar 1.0.0 compiled-wiki release. It
is not a substitute for hosted CI on the release pull request.

## Self-hosted quality gate

On 2026-08-24, the optimized `1.0.0` no-default-features binary rebuilt this repository's
committed wiki and persisted `.klaar/wiki-state/quality-report.json`.

| Gate | Result |
| --- | ---: |
| Canonical pages / lens renders | 21 / 42 |
| Supported claims | 300 / 300 |
| Citation validity | 1.0 |
| Evidence support | 1.0 |
| Core architecture coverage | 1.0 |
| Stale pages | 0 |
| Unsupported executive purpose/risk claims | 0 |
| Generator identity | `fact-renderer:1:true` |
| Overall | Pass |

The immediately repeated build rendered zero pages, reused all 40 renders, made zero
generator calls, delivered zero generator evidence bytes, and left status current.
Evidence truncation and omission diagnostics were explicit for pages that reached their
configured bounds; quality scoring did not silently waive unsupported claims.

## Boundary and integration evidence

The local release suite exercises these boundaries with real processes, sockets, files,
and fixtures:

- Headless command generation returns a typed draft through the same compiler used by the
  deterministic and MCP-assisted paths.
- Generator subprocess tests cover cleared-environment isolation, malformed JSON,
  timeout/termination, and output beyond the 512 KiB cap.
- MCP tests submit an assisted draft and persist a quality report through tool dispatch.
- A native filesystem notification marks a built wiki stale, and rebuilding restores a
  current state.
- Viewer socket tests cover valid localhost requests, forged `Host` rejection, encoded
  traversal rejection, CSP, `nosniff`, and `no-store` headers.
- Cross-language fixtures cover Rust, Go, Python, TypeScript, a mixed monorepo, missing
  trusted documents, generated/vendor exclusion, secret fixtures, rename/delete changes,
  and bounded shared-dependency fanout.
- State tests cover stable serialization, path confinement, interrupted-write recovery,
  and repository locking.

## Performance evidence

Five-run optimized measurements for small, mixed-monorepo, and 1,000-source-file
repositories are in [`wiki-performance.md`](wiki-performance.md), with raw observations in
[`performance/wiki-2026-08-24.csv`](performance/wiki-2026-08-24.csv). On the large case,
localized refresh reduced median wall time by 64%, generator calls by 87%, and generator
evidence bytes by 84% versus the initial build. No claim is made that incremental refresh
always reduces peak RSS.

## Release controls

The release pull request must still pass hosted Linux, macOS, Windows, MSRV, all-feature,
Clippy, package dry-run, RustSec-policy, Gitleaks, and committed-wiki gates. Tags do not
publish until the protected `release` environment approves the GitHub Release job;
crates.io additionally requires the explicit repository variable and registry token.
