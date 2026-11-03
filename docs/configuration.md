# Configuration

Memquira is configured entirely through environment variables, read and validated once at startup. The `MEMCORE_`
prefix is the engine's configuration namespace. An invalid or contradictory configuration stops the server with a
message naming the setting; nothing is silently corrected.

## Loading rules

- An optional `.env` file in the working directory is loaded first; real environment variables win.
- Every change needs a restart (no hot reload).
- Set `MEMCORE_VECTOR_BACKEND` explicitly (`mock` or `qdrant`); when unset it defaults to `qdrant`.
- Secrets (`MEMCORE_POSTGRES_URL`, `MEMCORE_DATABASE_URL`, `MEMCORE_DEV_API_KEY`, `MEMCORE_API_KEY_PEPPER`,
  `MEMCORE_*_API_KEY`, `OPENAI_API_KEY`, `MEMCORE_REDIS_URL`) are redacted from logs and debug output.

Templates:

| File | Use |
|---|---|
| [`.env.example`](../.env.example) | Local development: SQLite, mock vectors and mock providers |
| [`.env.production.example`](../.env.production.example) | Production: PostgreSQL, Qdrant, OpenAI, database authentication (used by `docker/docker-compose.prod.yml`) |
| [`profiles/byom.env.example`](../profiles/byom.env.example) | Any OpenAI-compatible chat and embedding endpoints, configured independently |
| [`profiles/local-starter.env.example`](../profiles/local-starter.env.example) | Fully local models (Ollama + TEI) |

Copy a template, replace every `CHANGE_ME` / `REPLACE_` value from your secret store, and never commit the copy.
`./scripts/ops/validate_env.sh <file>` runs a quick heuristic check of an env file.

## Development versus production

| Concern | Development | Production |
|---|---|---|
| `MEMCORE_ENV` | `development` | `production` |
| Authentication | `MEMCORE_AUTH_MODE=dev` with `MEMCORE_DEV_API_KEY` (one key for every organization) | `MEMCORE_AUTH_MODE=database` with `MEMCORE_API_KEY_PEPPER` (hashed, scoped, organization-bound keys) |
| Storage | `MEMCORE_STORAGE_MODE=embedded`, SQLite, `MEMCORE_VECTOR_BACKEND=mock` | `MEMCORE_STORAGE_MODE=production`, PostgreSQL, Qdrant |
| Providers | `mock`, with `MEMCORE_ALLOW_MOCK=true` | a real provider, `MEMCORE_ALLOW_MOCK=false` |
| Provider guardrails | defaults | raise `MEMCORE_PROVIDER_MAX_CALLS_PER_RUN` (see below) |
| Logs | `MEMCORE_LOG_FORMAT=pretty` | `json` |

The server refuses to start with mock providers unless `MEMCORE_ALLOW_MOCK=true`.

## Conditional requirements

| Condition | Required |
|---|---|
| `MEMCORE_FACT_BACKEND` or `MEMCORE_EVENT_BACKEND` = `postgres` | `MEMCORE_POSTGRES_URL`, and a build with the `postgres` feature |
| `MEMCORE_VECTOR_BACKEND` = `qdrant` | `MEMCORE_QDRANT_URL` (gRPC, port 6334), `MEMCORE_QDRANT_COLLECTION`, and a build with the `qdrant` feature |
| `MEMCORE_AUTH_MODE` = `database` | `MEMCORE_API_KEY_PEPPER` |
| Authentication on with `dev` mode | a non-empty `MEMCORE_DEV_API_KEY` |
| LLM or embedding provider = `openai` | a key for that role (`MEMCORE_<ROLE>_API_KEY` or `OPENAI_API_KEY`), unless the endpoint is loopback or `MEMCORE_<ROLE>_ALLOW_NO_API_KEY=true` |
| `POST /api/v1/memories/context` | `MEMCORE_SEARCH_MAX_LIMIT` of at least 100 (with `MEMCORE_EPISODES_ENABLED=true` the context also includes source messages) |

## Providers

Extraction (`MEMCORE_LLM_*`) and embeddings (`MEMCORE_EMBEDDING_*`) are configured independently: provider, model,
base URL and key for each. Both speak the OpenAI API (`/chat/completions` with JSON output, `/embeddings`). See
[providers.md](providers.md).

### Provider guardrails

The guardrails bound what a single process can send to a real provider:

| Setting | Default | Notes |
|---|---|---|
| `MEMCORE_REAL_PROVIDER_CALLS_ENABLED` | `false` | Must be `true` to call a real provider |
| `MEMCORE_PROVIDER_TEST_MODE` | `mock_only` | `production` for normal operation |
| `MEMCORE_PROVIDER_MAX_CALLS_PER_RUN` | `10` | Calls allowed per process before every further call is refused. The default is a safety value for tests: **raise it for real use** (the production template sets `1000000`). It is not billing; cap spend with your provider's project limits |
| `MEMCORE_PROVIDER_MAX_INPUT_CHARS` | `4000` | Largest input of one call (an extraction window, or a candidate plus related facts). The production template sets `7000` |
| `MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS` | `2048` | Output ceiling of every LLM call |
| `MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL` | `1` | Caps `MEMCORE_PROVIDER_MAX_RETRIES` |

## Retrieval

| Setting | Default | Notes |
|---|---|---|
| `MEMCORE_SEARCH_STRATEGY` | `hybrid` | `hybrid` (vector + keyword), `semantic`, `fulltext` or `rrf` (vector + full-text, Reciprocal Rank Fusion); a request can override it with `retrieval_mode` |
| `MEMCORE_SEARCH_MAX_LIMIT` | `50` | Largest `limit` a search accepts (50–200); set `100` for the context endpoint |
| `MEMCORE_EPISODES_ENABLED` | `false` | Store source messages as searchable episodes; used by the context endpoint and needed by message-id deduplication and conflict detection |
| `MEMCORE_CONTEXT_DEFAULT_PACKING` | `prefix` | Packing of `POST /api/v1/memories/context`: `prefix` (the validated default), `fill` or `evidence` (opt-in, unvalidated) |
| `MEMCORE_QDRANT_SEARCH_HNSW_EF` | unset | Query-time HNSW beam; `256` in the production template |

## Context cache

`MEMCORE_CONTEXT_CACHE_BACKEND` must stay `disabled` in production. `memory` is accepted only for local
single-process development (`MEMCORE_ENV=development`, embedded storage and
`MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true`); `redis` is always refused, because cache invalidation is not
guaranteed across processes.

## Limits and protection

Per-organization rate limiting (`MEMCORE_RATE_LIMIT_*`), concurrency admission control (`MEMCORE_ADMISSION_*`),
request size (`MEMCORE_MAX_REQUEST_BODY_BYTES`), quotas (`MEMCORE_QUOTAS_ENABLED`, `MEMCORE_MAX_*`) and security
headers are on or available by default; see the reference below and [security.md](security.md). All of them are
process-local.

## Experimental settings

Settings marked **experimental** or **opt-in; not validated** are off by default and are not recommended for
production. Turning one on can change the quality tier the server reports to `compatible-unvalidated`.

## Reference

Columns: whether the variable is required, its default when unset, whether it is a secret, and typical local and
production values.

| Variable | Required | Default (unset) | Sensitive | Local | Production | Notes |
|----------|----------|-----------------|-----------|-------|------------|-------|
| `MEMCORE_ENV` | no | `development` | no | `development` | `production` | `dev`/`prod` aliases accepted |
| `MEMCORE_HOST` | no | `0.0.0.0` | no | `0.0.0.0` | `0.0.0.0` | Bind address |
| `MEMCORE_PORT` | no | `8080` | no | `8080` | `8080` | HTTP port |
| `MEMCORE_AUTH_ENABLED` | no | `true` | no | `true` | `true` | |
| `MEMCORE_AUTH_MODE` | no | `dev` | no | `dev` | `database` | See [deployment.md](deployment.md) |
| `MEMCORE_DEV_API_KEY` | conditional | `memcore_dev_key` | **yes** | local only | unset | Dev plaintext key |
| `MEMCORE_API_KEY_PEPPER` | conditional | none | **yes** | optional | required (db auth) | HMAC pepper |
| `MEMCORE_STORAGE_MODE` | no | `embedded` | no | `embedded` | `production` | |
| `MEMCORE_FACT_BACKEND` | no | `sqlite` | no | `sqlite` | `postgres` | `mock`/`sqlite`/`postgres` |
| `MEMCORE_EVENT_BACKEND` | no | inherits fact | no | `sqlite` | `postgres` | |
| `MEMCORE_DATABASE_URL` | no | `sqlite://./data/memcore.db` | **yes** | SQLite path | unused if Postgres | Append `?mode=rwc` so a new database file is created; the directory must exist |
| `MEMCORE_POSTGRES_URL` | conditional | none | **yes** | — | required | Postgres DSN |
| `MEMCORE_DATABASE_MIGRATIONS_ENABLED` | no | `true` | no | `true` | `true` | See [operations.md](operations.md) |
| `MEMCORE_DATABASE_MIGRATION_MODE` | no | `auto` | no | `auto` | `auto` or `validate_only` | Avoid `disabled` in prod |
| `MEMCORE_DATABASE_REQUIRE_CLEAN_MIGRATIONS` | no | `true` | no | `true` | `true` | |
| `MEMCORE_VECTOR_BACKEND` | **set explicitly** | `qdrant` | no | `mock` | `qdrant` | Set always |
| `MEMCORE_QDRANT_URL` | conditional | `http://localhost:6333` | no | — | Qdrant URL | |
| `MEMCORE_QDRANT_COLLECTION` | conditional | `memcore_vectors` | no | — | collection name | |
| `MEMCORE_ADMISSION_CONTROL` | no | `on` | no | — | `on` \| `off` | Admission control on protected API routes (health, readiness, metrics and docs are never admitted). `off` is a rollback switch only: a release profile keeps it on. `off` also removes the provider concurrency bounds |
| `MEMCORE_ADMISSION_READ_MAX_IN_FLIGHT` | no | `16` | no | — | 1–100000 | Read requests (search, candidates, context, episode search, every protected GET) executing at once |
| `MEMCORE_ADMISSION_READ_MAX_QUEUE` | no | `64` | no | — | 0–1000000 | Read requests allowed to wait for a slot; beyond it an immediate `503 OVERLOADED` with `Retry-After` |
| `MEMCORE_ADMISSION_READ_DEADLINE_MS` | no | `30000` | no | — | > queue timeout | Deadline of an admitted read request, queue wait included; at the deadline the handler is cancelled and the answer is `503 REQUEST_DEADLINE_EXCEEDED` with `Retry-After` |
| `MEMCORE_ADMISSION_WRITE_MAX_IN_FLIGHT` | no | `8` | no | — | 1–100000 | Write requests (ingest, import, delete, other protected POST / PUT / PATCH / DELETE) executing at once |
| `MEMCORE_ADMISSION_WRITE_MAX_QUEUE` | no | `32` | no | — | 0–1000000 | Write requests allowed to wait |
| `MEMCORE_ADMISSION_WRITE_DEADLINE_MS` | no | `120000` | no | — | > queue timeout | Deadline of an admitted write request. A write cancelled at its deadline may already have committed part of its work |
| `MEMCORE_ADMISSION_ADMIN_MAX_IN_FLIGHT` | no | `4` | no | — | 1–100000 | Admin requests (`/api/v1/admin/*`, API keys) executing at once |
| `MEMCORE_ADMISSION_ADMIN_MAX_QUEUE` | no | `16` | no | — | 0–1000000 | Admin requests allowed to wait |
| `MEMCORE_ADMISSION_ADMIN_DEADLINE_MS` | no | `60000` | no | — | > queue timeout | Deadline of an admitted admin request |
| `MEMCORE_ADMISSION_QUEUE_TIMEOUT_MS` | no | `2000` | no | — | > 0 | Longest wait for a slot before `503 OVERLOADED` |
| `MEMCORE_ADMISSION_RETRY_AFTER_SECONDS` | no | `1` | no | — | 1–3600 | `Retry-After` on every overload / deadline 503 |
| `MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY` | no | `8` | no | — | > 0 | Process-wide bound on concurrent LLM calls (extraction, classification, consolidation, summarisation, entities, decomposition). A call holds its permit through its retries |
| `MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY` | no | `16` | no | — | > 0 | Process-wide bound on concurrent embedding calls (retries included) |
| `MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS` | no | `10000` | no | — | > 0 | Longest wait for a provider permit, also bounded by the request deadline; on timeout `503 OVERLOADED` |
| `MEMCORE_QDRANT_WRITE_MODE` | no | `optimized` | no | — | `optimized` \| `legacy` | How fact and episode vectors are written to Qdrant. `optimized`: a fact created in the same request, and an episode (point ID = episode ID), upsert without the filtered pre-delete; an update is one filtered delete plus the upsert. `legacy`: the exact earlier requests (rollback). One point per fact is kept either way. Unrelated to `MEMCORE_QDRANT_ENSURE_INDEXES` |
| `MEMCORE_QDRANT_SEARCH_HNSW_EF` | no | unset | no | — | `256` (reference profile; integer 1–4096) | The query-time HNSW beam (`params.hnsw_ef`) of every Qdrant fact and episode search. **Reference profile: `256`**, the value the maintainers validated, and the only value the reference contract accepts. **Application default: unset**, which sends no search parameter (Qdrant's default, the earlier request). Unset and every other value make the configuration `compatible-unvalidated`. The beam changes no index, write, delete or payload; it only reaches searches Qdrant runs on the HNSW graph, not plain or small-cardinality scans. **Rollback:** unset it and restart (no reindex, no collection change); the configuration is then `compatible-unvalidated`, so a deployment pinning `MEMCORE_REQUIRE_QUALITY_TIER=reference-validated` must change that pin to start. Invalid values refuse startup |
| `MEMCORE_CONTEXT_DEFAULT_PACKING` | no | `prefix` | no | `prefix` | `prefix` (reference profile) | Packing of `POST /api/v1/memories/context` when a request names none (`prefix` / `fill` / `evidence`; any other value is refused at startup). An explicit request `packing` always wins. `prefix` (default, the reference packing) is the only reference-contract value; `fill` and `evidence` are `compatible-unvalidated`. `evidence` (fill over a fact-anchored order: each fact followed by the retrieved turns it cites, at most budget / 5 tokens; a fact whose source turns are all kept is skipped) was the default from 2026-10-27 to 2026-10-28. It improved evidence recall over fill in an internal long-conversation evaluation but **lowered end-to-end answer accuracy** in the internal answer evaluation, so `prefix` was restored. Changing the value needs only a restart; no migration, reindex or data change |
| `MEMCORE_QDRANT_EXACT_SEARCH_MAX_TENANT_POINTS` | no | unset | no | — | integer 1–1,000,000 | **Experimental** (not part of the reference configuration). Unset (the default and the reference configuration) sends the standard search request and no count. Set to N: before every fact and episode vector search, memcore counts the tenant's points in the queried collection (exact count, the same `org_id` + `user_id` filter). A tenant with at most N points is searched with Qdrant `params.exact = true` (brute force over that tenant only), a larger one with the unchanged approximate request. Costs one extra Qdrant request per search, plus a scan that grows with the tenant up to N. A count failure fails the search explicitly (no fallback). A write between the count and the search can move a tenant across N for that one search; both paths filter on the tenant. Any value makes the configuration `compatible-unvalidated`. Rollback: unset it. Invalid values refuse startup |
| `MEMCORE_QDRANT_ENSURE_INDEXES` | no | `true` | no | — | bool | When a Qdrant collection is opened or created, create the missing keyword payload indexes (`org_id`, `user_id`, `fact_id`, plus `memory_type` on the fact collection). It reads the schema first, so a repeat start writes nothing. Points, vectors and HNSW are never rewritten, and `is_tenant` is not set. An existing index of another type fails the start. `false` skips the check (escape hatch); indexes already created stay and are harmless |
| `MEMCORE_QDRANT_EPISODE_COLLECTION` | no | `<MEMCORE_QDRANT_COLLECTION>_episodes` | no | — | collection name | Episode vectors, used only when `MEMCORE_EPISODES_ENABLED=true`; must differ from the fact collection |
| `MEMCORE_LLM_PROVIDER` | no | `mock` | no | `mock` | `openai` etc. | |
| `MEMCORE_LLM_MODEL` | no | `mock-llm` | no | mock | model id | |
| `MEMCORE_EMBEDDING_PROVIDER` | no | `mock` | no | `mock` | `openai` | |
| `MEMCORE_EMBEDDING_MODEL` | no | `mock-embedding` | no | mock | model id | |
| `OPENAI_API_KEY` | conditional | none | **yes** | — | secret manager | Not `MEMCORE_`-prefixed |
| `OPENAI_BASE_URL` | no | OpenAI v1 URL | no | default | default/compatible | |
| `MEMCORE_LLM_BASE_URL` | no | `OPENAI_BASE_URL` | no | — | URL | The extraction / chat endpoint (OpenAI-compatible) |
| `MEMCORE_LLM_API_KEY` | no | `OPENAI_API_KEY` | **yes** | — | secret | Key for the LLM endpoint; optional for loopback hosts |
| `MEMCORE_LLM_ALLOW_NO_API_KEY` | no | `false` | no | — | bool | Accept a non-loopback LLM endpoint without a key (a server that needs no auth) |
| `MEMCORE_EMBEDDING_BASE_URL` | no | `OPENAI_BASE_URL` | no | — | URL | The embedding endpoint (OpenAI-compatible) |
| `MEMCORE_EMBEDDING_API_KEY` | no | `OPENAI_API_KEY` | **yes** | — | secret | Key for the embedding endpoint; optional for loopback hosts |
| `MEMCORE_EMBEDDING_ALLOW_NO_API_KEY` | no | `false` | no | — | bool | Accept a non-loopback embedding endpoint without a key |
| `MEMCORE_EMBEDDING_DIMENSIONS` | no | from the model name | no | — | 1–65536 | Expected vector size (identity) |
| `MEMCORE_EMBEDDING_SEND_DIMENSIONS` | no | `true` | no | — | bool | Send the `dimensions` request field (some servers reject it) |
| `MEMCORE_EMBEDDING_NORMALIZE` | no | `false` | no | — | bool | L2-normalise vectors in memcore (identity) |
| `MEMCORE_EMBEDDING_QUERY_PREFIX` | no | none | no | — | text | Prepended to search queries (identity, hashed) |
| `MEMCORE_EMBEDDING_DOCUMENT_PREFIX` | no | none | no | — | text | Prepended to stored text (identity) |
| `MEMCORE_EMBEDDING_MODEL_REVISION` | no | none | no | — | text | Model revision (identity) |
| `MEMCORE_EMBEDDING_SERVING_VARIANT` | no | none | no | — | text | Serving variant, e.g. quantisation (identity) |
| `MEMCORE_EMBEDDING_DEDUP_THRESHOLD` | no | policy | no | — | (0, 1] | Explicit embedding-dedup cosine threshold; default: 0.92 for Amazon Titan Text Embeddings V2, exact text only for other models (identity) |
| `MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP` | no | `false` | no | — | bool | Allow sending a key over plain http to a non-loopback endpoint |
| `MEMCORE_PROVIDER_PROBE` | no | `on` | no | — | `on` \| `off` | Startup embedding probe: identity fingerprint, legacy-store compatibility proof (`off` = config identity only; legacy stores used unverified) |
| `MEMCORE_CONTEXT_CACHE_BACKEND` | no | `disabled` | no | `disabled` | `memory` (local single-process development only) | `redis` is refused at startup; see [Context cache](#context-cache) |
| `MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS` | no | `false` | no | `false` (must stay unset / false) | `true` with `memory` | operator statement, not enforced across processes |
| `MEMCORE_REDIS_URL` | no | none | **yes** | — | — | unused while the `redis` backend is refused |
| `MEMCORE_CONTEXT_CACHE_TTL_SECONDS` | no | `300` | no | — | tune | |
| `MEMCORE_CONTEXT_CACHE_MAX_ENTRIES` | no | `1000` | no | memory only | — | |
| `MEMCORE_CONTEXT_CACHE_KEY_PREFIX` | no | `memcore` | no | — | namespace | |
| `MEMCORE_CONTEXT_CACHE_STAMPEDE_PROTECTION_ENABLED` | no | `true` | no | on | on | Process-local |
| `MEMCORE_CONTEXT_CACHE_LOCK_TIMEOUT_SECONDS` | no | `30` | no | — | — | |
| `MEMCORE_CONTEXT_CACHE_STALE_WHILE_REVALIDATE_ENABLED` | no | `false` | no | off | optional | |
| `MEMCORE_CONTEXT_CACHE_STALE_TTL_SECONDS` | no | `120` | no | — | — | |
| `MEMCORE_CONTEXT_CACHE_METRICS_ENABLED` | no | `true` | no | on | on | |
| `MEMCORE_LLM_JSON_MODE` | no | `json_object` | no | default | default | How chat-completions JSON calls ask for JSON. `json_object` (default) leaves request bodies unchanged. `json_schema` also sends the prompt's schema as a structured-output `response_format` (OpenAI; Ollama >= 0.5); the local starter uses it. `instruction` sends no `response_format` (the prompt still demands JSON) for servers that reject the field. Only `json_object` keeps the reference tier |
| `MEMCORE_REQUIRE_QUALITY_TIER` | no | unset | no | `reference-validated` | per profile | Refuse to start unless the runtime quality tier is this (`reference-validated`, `compatible-unvalidated`, `development-mock`). See [providers.md](providers.md#quality-tiers) |
| `MEMCORE_LLM_STARTUP_PROBE` | no | `false` | no | `false` | optional | One classification call at startup that must return valid JSON, or the server refuses to start. On a paid endpoint every start costs one call; a skipped probe is never reported as a live verification |
| `MEMCORE_PROTECTED_COLLECTIONS` | no | unset | no | — | — | Extra collections (`name`, `prefix*`, comma-separated) that `memcore-maintenance` must never modify; a built-in list of reserved names is always protected |
| `MEMCORE_PROVIDER_TIMEOUT_SECONDS` | no | `30` | no | default | default | See [providers.md](providers.md). The OpenAI-compatible HTTP client uses the larger of this and 90 s (the local starter sets 300) |
| `MEMCORE_PROVIDER_MAX_RETRIES` | no | `2` | no | default | default | |
| `MEMCORE_PROVIDER_FALLBACK_ENABLED` | no | `false` | no | off | optional | |
| `MEMCORE_LLM_FALLBACK_ORDER` | no | `openai` | no | — | chain | |
| `MEMCORE_EMBEDDING_FALLBACK_ORDER` | no | `openai` | no | — | chain | |
| `MEMCORE_PROVIDER_CIRCUIT_BREAKER_ENABLED` | no | `true` | no | on | on | Process-local |
| `MEMCORE_PROVIDER_USAGE_METRICS_ENABLED` | no | `true` | no | on | on | |
| `MEMCORE_PROVIDER_USAGE_PERSISTENCE_ENABLED` | no | `false` | no | off | often on | |
| `MEMCORE_PROVIDER_USAGE_RETENTION_DAYS` | no | `180` | no | — | retention | |
| `MEMCORE_PROVIDER_COST_TRACKING_ENABLED` | no | `false` | no | off | optional | |
| `MEMCORE_QUOTAS_ENABLED` | no | `false` | no | off | optional | |
| `MEMCORE_MAX_*` / `MEMCORE_DAILY_*` | no | `0` unlimited | no | — | set limits | |
| `MEMCORE_BACKGROUND_JOBS_ENABLED` | no | `false` | no | off | deliberate | In-process maintenance jobs (retention, usage snapshots) |
| `MEMCORE_BACKGROUND_JOB_ORG_IDS` | no | empty | no | — | org list | Org-scoped jobs |
| `MEMCORE_BACKGROUND_JOB_LOCK_ENABLED` | no | `false` | no | off | multi-instance | Database locks |
| `MEMCORE_BACKGROUND_JOB_RETRIES_ENABLED` | no | `true` | no | on | on | |
| `MEMCORE_GRACEFUL_SHUTDOWN_TIMEOUT_SECONDS` | no | `30` | no | `30` | `30`+ | Drain bound after SIGTERM / Ctrl+C (Windows: also Ctrl+Break, console close, system shutdown); new connections are refused while draining and `/health/ready` reports `draining` |
| `MEMCORE_ALLOW_MOCK` | no | `false` | no | `true` (mock providers only) | **`false`** | The server refuses to start when any LLM / embedding provider (or fallback chain) resolves to `mock` unless this is `true`; a fallback chain mixing `mock` with a real provider is always refused |
| `MEMCORE_STARTUP_DEPENDENCY_TIMEOUT_SECONDS` | no | `60` | no | `60` | `60` | Bounded wait (backoff 0.5 s → 5 s) for Qdrant / Postgres at startup; fatal errors (bad URL, authentication, missing database) stop at once; `0` = one attempt. Health: `/health/live` (process), `/health/ready` (200 / 503: database, Qdrant reachability, provider mode, draining); legacy `/health` and `/ready` unchanged |
| `MEMCORE_BACKGROUND_JOB_SHUTDOWN_TIMEOUT_SECONDS` | no | `30` | no | `30` | `30` | |
| `MEMCORE_RATE_LIMIT_ENABLED` | no | `true` | no | on | on | Per-process |
| `MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE` | no | `60` | no | `60` | higher | Per org |
| `MEMCORE_SECURITY_HEADERS_ENABLED` | no | `true` | no | on | on | See [security.md](security.md) |
| `MEMCORE_MAX_REQUEST_BODY_BYTES` | no | `1048576` | no | 1 MiB | 1 MiB+ | |
| `MEMCORE_CORS_ENABLED` | no | `false` | no | off | off unless browsers | Exact origins only |
| `MEMCORE_CORS_ALLOW_CREDENTIALS` | no | `false` | no | false | false | No `*` + credentials |
| `MEMCORE_BACKUP_ENABLED` | no | `false` | no | optional | SQLite ops | SQLite file backups; see [operations.md](operations.md) |
| `MEMCORE_BACKUP_DIR` | no | `./backups` | no | `./backups` | protected path | |
| `MEMCORE_RESTORE_ENABLED` | no | `false` | no | **false** | **false** | Destructive; never public API |
| `MEMCORE_ENABLE_PII_REDACTION` | no | `true` | no | on | on | Regex-based. Redacts message content only; speaker names, message timestamps and message IDs are kept for attribution |
| `MEMCORE_STRUCTURED_EXTRACTION_ENABLED` | no | `true` | no | on | on | Extract speaker-attributed, time-resolved facts that cite their source messages (on by default). `false` sends exactly the older unstructured extraction request. Relative dates are resolved against message or session timestamps; send them — see the requirement on timestamp-less requests |
| `MEMCORE_STRUCTURED_EXTRACTION_POLICY` | no | `retentive` | no | `retentive` | `retentive` | Retention rule of the structured extraction prompt. `retentive` (default) keeps names, relationships and distinguishing detail; `atomic` is the original Part B prompt |
| `MEMCORE_SEARCH_STRATEGY` | no | `hybrid` | no | `hybrid` | `hybrid` | Retrieval for searches whose request names no `retrieval_mode`. `hybrid`: the fused search (vector + substring keyword). `semantic`: vector only. `fulltext`: ranked full-text only (migration 0011). `rrf`: vector + full-text fused by Reciprocal Rank Fusion over rank positions |
| `MEMCORE_EPISODES_ENABLED` | no | `false` | no | `false` | `false` | Store every added message as an episode (source-message-level memory; content after PII redaction) before extraction, embed messages of at least 20 characters, and serve `POST /api/v1/episodes/search` (semantic only). `observed_at` is the client's message or session time, never the receipt time. Off: no episode rows, vectors or embedding calls, and every existing response is unchanged. Episodes are removed by user deletion, follow the fact retention window, and are exported as `memcore.user_export.v2`. **Lifecycle does not depend on the flag:** with it off, forget-user and retention still delete persisted episode rows and vectors, export still includes a user's persisted episodes (v2), and import still restores episodes (rows and vectors, exactly as with the flag on). Restoring never enables episode search, E-merge or episode creation on ingestion. The episode vector collection / table is opened only if it exists and created only by an import that writes episode vectors, never at startup |
| `MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED` | no | `false` | no | `false` | `false` | EXPERIMENTAL: serve `POST /api/v1/memories/candidates`, one user's semantic fact results and episode results as a single list in the E-merge order (raw vector similarity, deterministic exact-tie rules; no RRF, normalisation or reranking). Needs `MEMCORE_EPISODES_ENABLED`. Off: the endpoint answers 400 and nothing else changes. Not a stable retrieval contract |
| `MEMCORE_QUERY_ANALYSIS_ENABLED` | no | `false` | no | `false` | `false` | EXPERIMENTAL: run the deterministic English-only query analyzer (question type, date range, temporal ordinal intent, clause sub-queries; no model call, no entity system) on candidate requests. A request's `reference_time` resolves relative expressions; the server clock is never used. Needs `MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED` |
| `MEMCORE_TEMPORAL_RANKING_ENABLED` | no | `false` | no | `false` | `false` | EXPERIMENTAL: event-time ranking of candidate lists. Only a fact's structured `event_time` is event time; message / observed / receipt time never is, so episodes are never boosted or reordered. Range boost, temporal-ordinal ordering in the window, `when` quota of 5; no hard filter. Needs `MEMCORE_QUERY_ANALYSIS_ENABLED` |
| `MEMCORE_TEMPORAL_BOOST` | no | `0.05` | no | `0.05` | `0.05` | The boost added to the raw vector score of facts whose event time overlaps the question's date range; greater than 0 and at most 1. Read only with temporal ranking on |
| `MEMCORE_MULTI_QUERY` | no | `none` | no | `none` | `none` | EXPERIMENTAL: `none` or `deterministic` (clause sub-queries from the analyzer; each retrieves the baseline list; union by maximum raw score). `llm` is not available in this build. Needs `MEMCORE_QUERY_ANALYSIS_ENABLED` |
| `MEMCORE_RERANK_PROVIDER` | no | `none` | no | `none` | `none` | EXPERIMENTAL: reranker for candidate lists. `none`, `local` (bge-reranker-v2-m3 INT8 on ONNX Runtime from local files; build with the `local-rerank` feature) or `bedrock` (Amazon Rerank 1.0 through the Agents Runtime `Rerank` API, SigV4 from the standard AWS credential chain; build with `bedrock-rerank`). Without the feature, startup fails with a clear message. Only the first `MEMCORE_RERANK_CANDIDATES` items are reordered, purely by the reranker's scores; any failure keeps the prior order and is reported. memcore never requires a reranker, and credentials are never needed unless `bedrock` is selected |
| `MEMCORE_RERANK_CANDIDATES` | no | `60` | no | `60` | `60` | The rerank / temporal-ranking window, 1-100 |
| `MEMCORE_RERANK_TIMEOUT_MS` | no | `30000` | no | `30000` | `30000` | Bound on one reranker call (100-600000); on expiry the prior order is kept and the fallback is reported |
| `MEMCORE_RERANK_LOCAL_MODEL_PATH` / `MEMCORE_RERANK_LOCAL_MODEL_SHA256` | with `local` | — | no | — | — | The local ONNX model file and its SHA-256. The file is never downloaded or bundled; a checksum mismatch refuses to start |
| `MEMCORE_RERANK_LOCAL_TOKENIZER_PATH` / `MEMCORE_RERANK_LOCAL_TOKENIZER_SHA256` | with `local` | — | no | — | — | The model's `tokenizer.json` and its SHA-256 (checked at startup) |
| `MEMCORE_RERANK_LOCAL_THREADS` / `MEMCORE_RERANK_LOCAL_BATCH_SIZE` | no | `4` / `16` | no | — | — | ONNX Runtime intra-op threads (pinned) and pairs per inference batch (1-64 each); pairs are truncated to 512 tokens |
| `MEMCORE_RERANK_BEDROCK_REGION` / `MEMCORE_RERANK_BEDROCK_MODEL_ARN` / `MEMCORE_RERANK_BEDROCK_PROFILE` | for `bedrock` | none (required) / Amazon Rerank 1.0 ARN in the configured region (must match it) / unset | no | — | — | Bedrock reranker region, model ARN and optional AWS profile (else the default credential chain). Candidate text is sent to that AWS region |
| `MEMCORE_QUERY_DECOMPOSITION` / `MEMCORE_QUERY_DECOMPOSITION_TIMEOUT_MS` | no | `none` / `10000` | no | `none` | `none` | EXPERIMENTAL: `none`, `noop` (model-free) or `llm` (the frozen `decompose-v1` contract through the configured LLM provider; chat-completions transport only). At most one decomposition operation per logical question: requests with the same fingerprint (organisation, normalised question, reference time, contract, model, configuration; never the speaker) share one result through a 1,024-entry, 10-minute cache with single-flight, and a failure leaves a sentinel for the same 10 minutes. The original query always stays; any failure falls back to it; no query or error text is logged. Needs `MEMCORE_EXPERIMENTAL_CANDIDATES_ENABLED` |
| `MEMCORE_CONSOLIDATION_MODE` | no | `legacy` | no | `legacy` | `legacy` | `legacy` (default) keeps the original consolidation path. `llm_v2` gathers related active facts, takes deterministic fast paths (exact duplicate, nothing related) and otherwise asks the model for one validated ADD / UPDATE / INVALIDATE / NOOP decision; INVALIDATE never deletes: it sets `superseded_at`, sets `invalid_at` only from deterministic source temporal evidence, and links the replacement. In `llm_v2`, search and context leave inactive facts out unless `include_history` |
| `MEMCORE_CONSOLIDATION_CANDIDATES` | no | `10` | no | | | Related facts a v2 decision sees (1–50) |
| `MEMCORE_CONSOLIDATION_SIMILARITY_FLOOR` | no | `0.5` | no | | | v2 fast path: with no related fact at or above this vector similarity (0–1) the fact is added without a model call |
| `MEMCORE_RELATED_FACT_MATCHER` | no | `legacy` | no | `legacy` | `legacy` | **Experimental and opt-in; not validated with the real classifier. Not recommended for production.** Legacy consolidation path only. `legacy` (default) keeps the previous related-fact lookup (a stored fact containing the whole candidate text) unchanged. `semantic` offers the classifier up to 10 of the user's facts of the same memory type nearest the candidate's own embedding (vector similarity >= 0.5, the consolidation floor; never another subject, another tenant, or a stale or inactive row; no full-text hits), and applies a classifier UPDATE / DELETE only to an offered fact of the same (or an unknown) subject that is not an event and not at a different event time: the candidate is stored as a new fact and the old one is superseded (`superseded_at`, `superseded_by`; never overwritten or deleted). Anything else is stored as an ADD (`memcore_ingest_related_decision_downgraded_total{reason}`). Search and context then leave superseded facts out unless `include_history`; `/memories/context` never shows them. A history result of `POST /memories/search` carries a `lifecycle` object (`superseded` / `invalidated`, `superseded_by`, times) and a `POST /context` built with history prefixes it with `[no longer current] `. `semantic_v23` (candidate, same caveats) is `semantic` without the memory-type filter (an `event` fact is offered only to an `event` candidate) and with canonical subject names (case, whitespace, surrounding punctuation, possessive and leading article ignored; `user` / `the user` / `I` one name; distinct names never merged), in the lookup and the guard; same floor, limit and history rule. Any value other than `legacy` is a reference-contract mismatch: the quality tier is `compatible-unvalidated`. With `MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED` the classifier is skipped only when nothing is offered. Projected (not billed) ingest provider cost on the tested workloads: +14 % to +40 % (G6-7 <= +10 %: projected FAIL for activation); matcher I/O about 6 ms p50 / 30 ms p95 per candidate (one vector search and up to 20 row reads) |
| `MEMCORE_TEMPORAL_REWRITE` | no | `legacy` | no | `legacy` | `legacy` | **Opt-in; not answer-validated.** How structured extraction writes a dated fact's sentence. `legacy` (default, the reference behaviour) prefixes the settled date and removes the model's time expression, whatever it is (for example, "The project kickoff will be in Room 4B." with the model expression "in Room 4B" was stored as "On 28 February 2026, The project kickoff will be."). `preserving` removes only an expression the deterministic resolver recognised and only where the sentence stays complete: a span with no time vocabulary gives no event time; a recurrence ("every Sunday", "each month") is non-datable; a model date the resolver could not check is prefixed but nothing is removed; a date that is the sentence's value (after is / to / until / by / for / due) stays in place (written dates as written, relative ones replaced by the settled date); otherwise the legacy output byte for byte. Event time, expression and source message ids stay in the structure. `preserving` is a reference-contract mismatch (`compatible-unvalidated`) |
| `MEMCORE_IMPORTANCE_POLICY` | no | `legacy` | no | `legacy` | `legacy` | **Opt-in; not answer-validated.** `legacy` (default): a candidate is stored when its adjusted importance reaches `MEMCORE_MIN_IMPORTANCE`. `durable` also stores a candidate of any type but `conversation` and `system` whose adjusted importance is at least 0.5 (the extractor's medium rating; for example, plain facts such as "The cloud provider is AWS." typed `entity` / `other` / `event` were extracted at 0.5 and discarded). The short- and vague-content penalties apply first; ratings below 0.5 are still dropped; dedup is unchanged. A reference-contract mismatch |
| `MEMCORE_CLASSIFICATION_PROMPT` | no | `legacy` | no | `legacy` | `legacy` | **experimental and opt-in; not measured with a real model.** Instructions of the legacy-path fact-classification request. `legacy` (default) is the production prompt, byte-identical. `operations_v1` defines Add / Update / Delete / NoOp, the distinctions that decide them and six invented examples; the JSON schema, payload, parser and operation allow-list are unchanged. A reference-contract mismatch |
| `MEMCORE_ENTITIES_ENABLED` | no | `false` | no | `false` | `false` | **Experimental; keep off in production:** not validated to production-quality entity accuracy (an accuracy audit did not pass); no entity-aware retrieval. Entity layer: after an add has stored its facts, one entity-annotation request per extraction window (the fact-extraction request is unchanged), deterministic resolution, and entity / link / relation storage; serves `GET /api/v1/users/{user_id}/entities[/{entity_id}]`. Entity names are PII-redacted like messages. Off: no annotation or confirmation requests, no entity rows, and every existing response is unchanged. Entities are removed by user deletion and fact deletion, follow fact retention (links of pruned facts go, and entities left without a link), and are exported as `memcore.user_export.v3`. **Lifecycle does not depend on the flag:** with it off, forget-user, fact deletion, retention, export and import still act on entity data stored while it was on (the read endpoints stay off). Name embeddings record their embedding model and dimension and are compared only within one model and dimension |
| `MEMCORE_ENTITY_NAME_SIMILARITY_THRESHOLD` | no | `0.70` | no | | | Name-embedding similarity (0–1) at or above which a mention is a candidate match for a stored entity of a compatible type (predeclared before any entity measurement) |
| `MEMCORE_ENTITY_LLM_CONFIRMATION_ENABLED` | no | `true` | no | | | Ask the model to confirm a candidate match, at most once per new entity. Off: a candidate that is not an exact name or alias becomes a new entity |
| `MEMCORE_SEARCH_MAX_LIMIT` | no | `50` | no | `50` | `50` | Highest search `limit` accepted (50–200; other values are rejected at startup). With the default, search behaves exactly as before. Raising it lets a request ask for more than 50 results; such a request fetches exactly `limit` candidates per retrieval leg. Requests up to 50 are unaffected either way |
| `MEMCORE_RRF_K` | no | `60` | no | | | RRF constant `k` (> 0): rank r in a list contributes weight / (k + r) |
| `MEMCORE_RRF_WEIGHT_SEMANTIC` | no | `1.0` | no | | | RRF weight of the vector leg (>= 0) |
| `MEMCORE_RRF_WEIGHT_KEYWORD` | no | `1.0` | no | | | RRF weight of the full-text leg (>= 0) |
| `MEMCORE_EXTRACTION_WINDOW_TURNS` | no | `20` | no | | | Messages per structured extraction window |
| `MEMCORE_EXTRACTION_WINDOW_OVERLAP` | no | `2` | no | | | Messages repeated between consecutive windows; must be smaller than the window |
| `MEMCORE_EXTRACTION_CONCURRENCY` | no | `4` | no | | | Windows extracted at once per request |
| `MEMCORE_EXTRACTION_MAX_OUTPUT_TOKENS` | no | = `MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS` | no | | | Extraction output limit; must not exceed the ceiling |
| `MEMCORE_CLASSIFICATION_MAX_OUTPUT_TOKENS` | no | `300` | no | | | Classification output limit; capped at the ceiling |
| `MEMCORE_MIN_IMPORTANCE` | no | `0.55` | no | default | tune | |
| `MEMCORE_RETENTION_ENABLED` | no | `false` | no | off | deliberate | |
| `MEMCORE_LOG_FORMAT` | no | `json` | no | `pretty` | `json` | |
| `MEMCORE_LOG_LEVEL` | no | `info` | no | `info` | `info` | Avoid debug in prod |
| `MEMCORE_REQUEST_ID_HEADER` | no | `X-Request-ID` | no | default | default | |
| `MEMCORE_METRICS_ENABLED` | no | `false` | no | off | on + private/auth | Disabled by default |
| `MEMCORE_METRICS_PATH` | no | `/metrics` | no | `/metrics` | private path ok | Must start with `/`; no collision with `/health` `/ready` `/api/v1/version` |
| `MEMCORE_METRICS_REQUIRE_AUTH` | no | `true` | no | true | **true** | Set false only on private networks |
| `MEMCORE_METRICS_INCLUDE_PROCESS` | no | `false` | no | optional | optional | Optional build_info gauge |
| `MEMCORE_BUILD_GIT_SHA` | no | build `unknown` | no | — | CI inject | Build-time only |
| `MEMCORE_BUILD_TIMESTAMP` | no | build `unknown` | no | — | CI inject | Build-time only |
| `MEMCORE_BUILD_PROFILE` | no | Cargo profile | no | — | `release` | Build-time only |
| `POSTGRES_DB` / `USER` / `PASSWORD` | compose | — | password **yes** | — | match DSN | Postgres image only |

A few more provider back-off, job-interval and cache settings exist; `crates/memcore-config/src/settings.rs` is the
authoritative list.
