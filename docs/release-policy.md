# Release Policy

Klaar 1.0 supports Linux x86_64, macOS x86_64 and arm64, and Windows x86_64.
Rust 1.89 is the minimum supported compiler. GitHub Releases are the primary binary
channel; crates.io publication is enabled only when the protected `release` environment,
`PUBLISH_CRATES_IO` repository variable, and registry token are configured.

Every release tag must exactly match `Cargo.toml`. CI must pass cross-platform core tests,
all-feature Linux tests, Clippy, RustSec policy, committed-wiki quality and freshness,
package verification, and native `--version` smoke tests. Release artifacts include
checksums, a CycloneDX SBOM, and keyless Cosign signatures. Each packaged archive is
extracted and its contained binary is executed before upload. The protected `release`
environment must approve both GitHub Release creation and optional crates.io publication.

## Advisory Exceptions

Accepted exceptions are owned by `security@nma.vc` and expire on 2026-11-24 or at the
next affected dependency update, whichever occurs first. Each review must re-run
reachability analysis and remove the exception when an upgrade or replacement exists.

- `RUSTSEC-2026-0002` and `RUSTSEC-2026-0253`: SurrealKV 0.9.3 declares `lru` without
  Rust call sites. Any SurrealKV change requires a fresh source audit.
- `RUSTSEC-2026-0235`: `rkyv` is lockfile-only through a disabled `rust_decimal` feature.
  Any active dependency path makes this exception invalid.
- `RUSTSEC-2025-0141`, `RUSTSEC-2023-0089`, `RUSTSEC-2025-0119`, and
  `RUSTSEC-2024-0436`: unmaintained transitive crates remain tracked for upstream removal.

The authoritative advisory details are at `https://rustsec.org/advisories/<ID>.html`.
An ignored advisory is an explicit time-bounded risk acceptance, not a clean audit claim.
