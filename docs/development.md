# Development

## Prerequisites

- Rust 1.89 or newer (`rustup`), with `rustfmt` and `clippy`. The workspace uses edition 2024.
- Docker, for the optional integration tests against real PostgreSQL and Qdrant.
- On Linux: a C toolchain and `pkg-config` (some dependencies build native code).

## Layout

```
crates/
  memcore-api/        HTTP server (bin memcore-api), memcore-maintenance CLI, export_openapi
  memcore-core/       memory engine
  memcore-providers/  LLM and embedding providers
  memcore-storage/    SQLite, PostgreSQL, Qdrant; migrations/ (sqlite, postgres)
  memcore-config/     settings
  memcore-common/     shared errors, redaction, key hashing
docker/               Dockerfile and Compose files
openapi/              generated OpenAPI document
examples/             curl, Python and Node.js clients
profiles/             environment templates for provider setups
scripts/              operational helpers
```

## Build and run

```bash
cargo build                                   # SQLite + mock providers
cargo build --release -p memcore-api --features postgres,qdrant
cp .env.example .env && mkdir -p data && cargo run -p memcore-api
```

Cargo features of `memcore-api`:

| Feature | Enables |
|---|---|
| `postgres` | PostgreSQL fact, event and key stores |
| `qdrant` | Qdrant vector store |
| `redis-cache` | Redis client (the Redis context cache is currently refused at startup) |
| `local-rerank` | Local ONNX cross-encoder reranker (experimental) |
| `bedrock-rerank` | Amazon Bedrock reranker (experimental) |

## Tests

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The default test run needs no services: it uses SQLite, mocks and in-process fakes. Tests against real backends
skip unless their environment variables are set:

```bash
docker run -d --name memquira-test-pg -e POSTGRES_PASSWORD=test -p 127.0.0.1:55432:5432 postgres:16
docker run -d --name memquira-test-qdrant -p 127.0.0.1:6334:6334 qdrant/qdrant:v1.18.0
export MEMCORE_TEST_POSTGRES_URL=postgres://postgres:test@127.0.0.1:55432/postgres
export MEMCORE_TEST_QDRANT_URL=http://127.0.0.1:6334
cargo test -p memcore-storage --features postgres,qdrant -- --test-threads 1
```

Use disposable databases: some tests create and drop their own databases and collections.

No test calls a paid model provider. A test that needs model behaviour uses the mock providers or a local fake
HTTP server.

## OpenAPI document

`openapi/memcore.openapi.json` is generated from the code (`utoipa` annotations and doc comments). Regenerate it
after changing a route or a request or response type:

```bash
cargo run -p memcore-api --bin export_openapi -- openapi/memcore.openapi.json
```

## Database migrations

Migrations live in `crates/memcore-storage/migrations/{sqlite,postgres}` and are embedded in the binary. Applied
migrations are checksummed, so never edit a released migration: add a new one with the next number, in both
directories, and register it in `crates/memcore-storage/src/migrations/mod.rs`.

## Conventions

- `memcore-core` depends only on traits for storage and providers; concrete clients live in their crates.
- Every stored object is scoped by `org_id` and `user_id`; tests cover cross-tenant isolation for new routes.
- Errors go through `MemcoreError`; messages returned to clients must not contain secrets, prompts or content.
- Settings are added to `crates/memcore-config/src/settings.rs` with validation and a documented default, and to
  [configuration.md](configuration.md).
- Formatting is `rustfmt`; clippy runs with `-D warnings`.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the pull-request process.
