# Changelog

All notable changes to this project are documented in this file. The project follows
[Semantic Versioning](https://semver.org/); before 1.0, minor versions may contain breaking changes.

## [Unreleased]

## [0.3.1] - 2026-11-23

### Added

- `MEMQUIRA_PROVIDER_OPERATION_HEADER_ENABLED` (off by default) with `MEMQUIRA_PROVIDER_OPERATION_HEADER_HOSTS`:
  forwards a request's `X-Memquira-Operation` token on every OpenAI-compatible provider call made while serving it,
  retries included, so a trusted provider-side proxy can charge each call to the caller's operation. Only providers
  on the listed hosts receive it, and such clients follow no redirects; invalid tokens are refused with 400; calls
  outside a request carry none.
- `memcore_ingest_candidates_rejected_total{reason="invalid_classification"}`: candidates rejected because the
  model's classification was invalid (also reported in the write summary).

### Fixed

- Fact classification accepted the operation names `Add`, `add` and `NoOp` but discarded the candidate when a model
  wrote them in capitals (`ADD`, `UPDATE`, `NOOP`): the label was read as `a_d_d`. Operation labels are now matched
  case-insensitively, ignoring surrounding whitespace and the separators space, `_` and `-`. An unrecognized label
  is still rejected, counted as `invalid_classification` in the write summary and now logged by name (only when it
  looks like a label; other model text is withheld).
- With `MEMQUIRA_DEDUP_VALUE_GUARD=on`, a rescheduled date that exists only in the event-time expression (no message
  or session time to settle it) was treated as a duplicate or a classifier NoOp and lost. The expression now counts
  as part of the sentence, and under `MEMQUIRA_RELATED_FACT_MATCHER=semantic_v23` a classifier NoOp is not honoured
  when the candidate differs from every offered fact only in its event time or expression (which the classifier does
  not see).

### Security

- `url` 2.5.9 (RUSTSEC-2026-0592: stack overflow in `Url::origin()` on nested `blob:` URLs). Lockfile only.

## [0.3.0] - 2026-11-20

### Changed

- **Renamed from Memcore to Memquira.** Configuration variables use the `MEMQUIRA_` prefix, the Cargo packages
  are `memquira-*`, and the binaries are `memquira-api` and `memquira-maintenance`. The OpenAPI document is now
  `openapi/memquira.openapi.json` (title `Memquira API`), and `/health` reports `"service": "memquira"`.
  - Every old `MEMCORE_*` variable still works as a deprecated alias and logs a warning. If an old and a new name
    are set to different values, startup fails.
  - The Docker image keeps `memcore-api` and `memcore-maintenance` as symlinks.
  - The Compose service is now `memquira`: recreate the stack with `--remove-orphans`.
  - Metric names, storage defaults, Compose project and volume names, API keys and export formats are unchanged.
  - See [docs/upgrading.md](docs/upgrading.md).

### Added

- Python client in `clients/python` (package `memquira`, sync and async, typed results and errors,
  safe automatic retries, a per-user view for agent integrations). Install it with
  `pip install ./clients/python`; it is not published to PyPI yet.

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
