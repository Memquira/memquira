# Changelog

All notable changes to this project are documented in this file. The project follows
[Semantic Versioning](https://semver.org/); before 1.0, minor versions may contain breaking changes.

## [Unreleased]

## [0.2.0] - 2026-11-18

### Added

- Opt-in memory update settings, all off by default: `MEMCORE_UNKNOWN_MEMORY_TYPE`, `MEMCORE_EVENT_UPDATE_POLICY`,
  `MEMCORE_RELATED_FACT_SIMILARITY_FLOOR` and `MEMCORE_DEDUP_VALUE_GUARD` (value-aware near-duplicate detection,
  metric `memcore_dedup_value_guard_total`). Together with the existing opt-in matcher, prompt, rewrite, importance
  and classification-skip settings they form one evaluated configuration; see
  [Opt-in memory update settings](docs/configuration.md#opt-in-memory-update-settings) for its limitations.
- `MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED` is now documented in the configuration reference.

### Fixed

- With the semantic related-fact matchers, an Update or Delete whose target stopped being active while the model was
  deciding no longer supersedes it a second time; the candidate is handled as an Add.
- With `MEMCORE_TEMPORAL_REWRITE=preserving`, a calendar expression that the date prefix restates is removed, and a
  date after an article ("is on the 16th of June") is kept as the sentence's value.
- Structured extraction accepts a reply whose only defect is a trailing comma before a closing bracket, instead of
  rejecting the whole window. Commas inside strings are never changed.

### Security

- Updated quinn-udp to 0.5.16 (RUSTSEC-2026-0538). It is reachable only through an optional HTTP/3 dependency
  that no feature enables.
- RUSTSEC-2026-0459 (bundled SQLite in libsqlite3-sys 0.30.1) and RUSTSEC-2026-0543 (chrono unmaintained) are
  accepted risks under the supported deployment trust model, not patched vulnerabilities. Each is excepted by its
  advisory id only; rationale, residual exposure and removal conditions are in
  [docs/security.md](docs/security.md#advisory-exceptions) (tracking issues #2 and #3).

## [0.1.0] - 2026-11-06

Initial open-source release.

- Memory ingestion from chat messages with LLM-based fact extraction and add / update / delete / no-op decisions.
- Hybrid retrieval (vector and keyword), optional full-text with Reciprocal Rank Fusion.
- Prompt context assembly within a token budget.
- Per-user listing, deletion, erasure, export and import.
- Organization-scoped, hashed API keys with scopes; per-user isolation.
- SQLite and PostgreSQL storage, Qdrant vectors, checksummed migrations.
- OpenAI-compatible providers with retries, circuit breaker, concurrency limits and guardrails.
- Rate limiting, admission control, quotas, health probes, Prometheus metrics, OpenAPI and Swagger UI.
- Docker image and Compose stacks for local development and self-hosting.

Licensed under the Apache License, Version 2.0. The repository commits published before this release
(`a117259` to `ab149c7`) were made available under AGPL-3.0-or-later; copies obtained under those terms keep them.
