# Changelog

All notable changes to this project are documented in this file. The project follows
[Semantic Versioning](https://semver.org/); before 1.0, minor versions may contain breaking changes.

## [0.1.0] - 2026-11-03

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
