# Klaar Wiki Fixture Corpus

This corpus is deterministic, local-only, ASCII-only test data for the wiki compiler.
Paths are repository-relative. No fixture contains real credentials.

## Expected outcomes

| Fixture | Facts and pages | Stale fan-out | Security outcome |
| --- | --- | --- | --- |
| rust-basic | Rust package; run, Config, greet; architecture, dependencies, testing, component pages | Changing config.rs stales service and architecture; unchanged pages stay byte-identical | No secrets; Cargo manifest is trusted structured input |
| typescript-basic | TypeScript package; welcome, add, User; dependency and testing pages | Changing math.ts stales index and dependent component pages | JSON and TS are bounded evidence |
| python-basic | Python package; greet, User, pytest test | Changing models.py stales service and testing pages | pyproject.toml is trusted configuration |
| go-basic | Go module; main, Greet, Go test | Changing service.go stales main and testing pages | go.mod is trusted structured input |
| mixed-monorepo | API, web, worker components across TS and Go; trusted architecture doc | API change fans out to web and worker capability pages | Workspace boundaries prevent unrelated package leakage |
| no-trusted-docs | Rust and Python facts only; no documentation-derived claims | Source changes affect only source-backed pages | No trusted docs means no invented architecture or executive claims |
| rename-delete/baseline + updated | billing.rs renamed to payments.rs; shared.rs deleted | Rename preserves identity when evidence permits; deletion removes orphan pages and reverse edges | Deleted evidence cannot remain cited |
| shared-dependency/baseline + updated | shared.ts imported by api.ts and index.ts | One shared change stales both dependents, with bounded reported fan-out | No generated content or credentials |
| generated-vendor-heavy | src/real.ts is product code; generated and vendor trees are noisy | Changes below excluded roots do not stale product pages by default | Generated/vendor content is excluded before evidence reads |
| adversarial-secrets | Fake token, password, and private-key markers | No stale assertions depend on secret values | Detect and omit; do not echo secret values or persist them |
| adversarial-prompt-injection | Inert hostile instructions in comments and docs | Text changes affect evidence-backed pages only | Never execute, obey, or let evidence choose paths/citations |
| adversarial-markdown-mermaid | Unsafe URL, HTML, data URL, and Mermaid click directive | Diagram source changes only diagram page | Reject active HTML, unsafe schemes, and unsupported Mermaid directives |

## Fixture conventions

- Files are intentionally small and sorted by stable path.
- Manifests/configs exercise trusted structured facts without allowing commands or paths.
- README files describe expected behavior, not implementation authority.
- Baseline/updated directories are paired inputs, not one repository.
- generated/, vendor/, .env-like files, and key markers exercise hard exclusions.
- Adversarial strings are inert test data. Any secret-looking value is visibly fake.
- Golden facts should normalize separators to /, sort collections, and exclude timestamps.
- Expected pages are conceptual names; implementations may use the plan's artifact layout.

## Suggested assertions

1. Identical scans of each fixture produce byte-identical facts and stable IDs.
2. Each supported language exposes symbols, imports, components, tests, and manifests where present.
3. Rename/delete updates have no orphaned pages, evidence, or reverse provenance edges.
4. Shared dependency edits report both dependent pages and bounded fan-out.
5. Generated/vendor edits are excluded or produce explicit exclusion diagnostics.
6. Secret fixtures never write token values or private-key bodies to wiki artifacts.
7. Prompt text never causes command execution, path escape, or citation invention.
8. Unsafe Markdown and Mermaid constructs are rejected or sanitized while safe flow syntax remains valid.
9. All paths remain within the fixture root and no network is needed.
