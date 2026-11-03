# API reference

Base URL: `http://<host>:8080`. All API routes are under `/api/v1` and exchange JSON. The machine-readable contract
is the OpenAPI 3 document, served at `GET /openapi.json` (Swagger UI at `GET /docs`) and committed as
[`openapi/memcore.openapi.json`](../openapi/memcore.openapi.json). This page covers the endpoints an application
integrates with; field-level details for every endpoint are in the OpenAPI document.

## Authentication and tenancy

Every `/api/v1` route except `GET /api/v1/version` needs:

```http
Authorization: Bearer <api_key>
X-Organization-ID: <org_id>
```

- In `database` auth mode an API key belongs to one organization and carries scopes; it can only act for its own
  organization. Keys are created with
  `POST /api/v1/api-keys` (see [Keys](#api-keys)).
- In `dev` mode one shared key (`MEMCORE_DEV_API_KEY`) works for every organization and scopes are not enforced.
  Use it only locally.
- `user_id` identifies the end user inside the organization. Memquira does not authenticate end users: your
  backend decides which `user_id` a request acts for. Never let a browser choose it.

| Scope | Allows |
|---|---|
| `MemoryRead` | search, list, context, episodes, entities |
| `MemoryWrite` | ingesting memories, imports |
| `MemoryDelete` | deleting one memory |
| `UserDelete` | erasing a user, retention, exports |
| `AuditRead` | memory audit events, exports |
| `AdminRead` / `AdminWrite` | organization administration, API keys |

## Conventions

- **Errors** have one shape and a stable `code`:

  ```json
  {"error": {"code": "VALIDATION_ERROR", "message": "limit must be between 1 and 100", "request_id": "…", "details": {}}}
  ```

- **Request ids:** every response carries `X-Request-ID` (send your own to correlate logs); errors repeat it as
  `request_id`.
- **Pagination:** list endpoints take `limit` and an opaque forward-only `cursor`, and return `next_cursor` until
  the last page.
- **Timestamps** are RFC 3339 in UTC.

## Memories

### `POST /api/v1/memories` — ingest messages

Extracts facts from a conversation and stores, updates or deletes memories. Scope: `MemoryWrite`.

| Field | Type | Notes |
|---|---|---|
| `user_id` | string, required | End-user id |
| `messages` | array, required | `{ "role", "content", "message_id"?, "name"?, "timestamp"? }`; `role` is the speaker role, e.g. `user` or `assistant`; `name` the speaker's name |
| `session_id` | string | Groups messages of one conversation |
| `session_timestamp` | string | Session time for messages without their own `timestamp` (RFC 3339, `2024-03-12`, `12 March 2024`, `4:15 pm on 12 March, 2024`) |
| `metadata` | object | Stored with the memories |

```bash
curl -s -X POST "$MEMQUIRA_URL/api/v1/memories" \
  -H "Authorization: Bearer $KEY" -H "X-Organization-ID: $ORG" -H "Content-Type: application/json" \
  -d '{"user_id":"alice","session_id":"chat-1",
       "messages":[{"role":"user","content":"I moved to Lisbon last month and I am vegetarian.","message_id":"m-1"}]}'
```

```json
{
  "status": "success",
  "memories": [
    {"id": "…", "content": "User moved to Lisbon.", "memory_type": "Profile", "confidence": 0.9, "importance": 0.8}
  ],
  "summary": {"extracted": 2, "added": 2, "updated": 0, "deleted": 0, "noop": 0, "stored": 2, "skipped": {…}, "discarded": {…}, …}
}
```

`message_id` makes retries safe: a message already stored with that id is not stored again, and the same id with a
different message returns `409 MESSAGE_ID_CONFLICT` (requires `MEMCORE_EPISODES_ENABLED=true`). Typical errors:
`400 VALIDATION_ERROR`, `401`, `403`, `409 MESSAGE_ID_CONFLICT`, `429 RATE_LIMITED` / `QUOTA_EXCEEDED`,
`502 PROVIDER_ERROR` / `PROVIDER_OUTPUT_INVALID`, `503 OVERLOADED` / `PROVIDER_THROTTLED`, `504 TIMEOUT`.

### `POST /api/v1/memories/search` — search memories

Scope: `MemoryRead`.

| Field | Type | Notes |
|---|---|---|
| `user_id`, `query` | string, required | |
| `limit` | integer | Default 10; at most `MEMCORE_SEARCH_MAX_LIMIT` |
| `retrieval_mode` | string | `hybrid`, `semantic`, `fulltext` or `rrf`; default from `MEMCORE_SEARCH_STRATEGY` |
| `filters` | object | e.g. memory types, time range |
| `include_history` | boolean | Also return superseded and invalidated facts |
| `user_names` | array | Names the user goes by (kept out of full-text terms) |

```json
{"status": "success",
 "results": [{"fact_id": "…", "content": "User is vegetarian.", "memory_type": "Preference",
              "score": 0.032, "vector_score": 0.58, "metadata": {}}]}
```

`score` is the ranking score of the chosen mode (fused ranks for `hybrid` / `rrf`), comparable only within one
response; `vector_score` is the raw vector similarity when the vector leg ran.

### `GET /api/v1/users/{user_id}/memories` — list memories

Scope: `MemoryRead`. Query: `limit`, `cursor`, `memory_type`, `include_deleted`. Returns `memories` and
`next_cursor`.

### `DELETE /api/v1/users/{user_id}/memories/{memory_id}` — delete one memory

Scope: `MemoryDelete`. Soft-deletes the fact and removes its vector. `404 NOT_FOUND` when the id does not belong to
this user.

### `DELETE /api/v1/users/{user_id}` — erase a user

Scope: `UserDelete`. Deletes all of the user's facts, vectors and episodes in the organization.

## Context

### `POST /api/v1/memories/context` — prompt context

Retrieves the user's facts (and episodes, when enabled) and packs them, in rank order, into a token budget. Scope:
`MemoryRead`. Requires `MEMCORE_SEARCH_MAX_LIMIT` of at least 100; without episodes the context holds facts only.

| Field | Type | Notes |
|---|---|---|
| `user_id`, `query` | string, required | |
| `token_budget` | integer | `cl100k_base` tokens; default 1,500, range 1–4,000 |
| `packing` | string | `prefix` (default), `fill`, `evidence`; the last two are opt-in and unvalidated |
| `exclude_message_ids` | array | Leave out episodes of these messages (e.g. the turn being answered) |
| `include_items` | boolean | Also return the selected items with their provenance |

```json
{"status": "success", "text": "12 March 2024: User is vegetarian.\n…", "token_count": 42, "token_budget": 500,
 "selected_count": 3, "candidates_considered": 7, "budget_bound": false, "render_mode": "…", "source": "…"}
```

### `POST /api/v1/context` — formatted context

Search plus formatting: `max_tokens`, `max_memories`, `format`, sections by memory type, optional scores,
confidence, importance, timestamps and memory ids, and a summary section. Returns `context`, the `memories` used
and a `budget` breakdown. Scope: `MemoryRead`.

## Episodes and entities

- `POST /api/v1/episodes/search` — semantic search over stored source messages (needs episodes on). Scope:
  `MemoryRead`.
- `GET /api/v1/users/{user_id}/entities`, `GET /api/v1/users/{user_id}/entities/{entity_id}` — the experimental
  entity layer (`MEMCORE_ENTITIES_ENABLED`, off by default; `400` when off).

## Export, import and retention

- `GET /api/v1/users/{user_id}/export` — a `memcore.user_export.v1` JSON document of the user's facts (and audit
  events unless `include_events=false`). Scopes: `UserDelete`, `AuditRead` or `AdminRead`.
- `POST /api/v1/users/{user_id}/import` — import such a document. Body: `{"export": <document>, "mode": "append"`
  or `"replace", "restore_events": false, "dry_run": false}`. Scopes: `MemoryWrite` or `AdminWrite`. See [operations.md](operations.md#export-and-import).
- `POST /api/v1/users/{user_id}/retention/apply` — apply the retention policy to one user (`dry_run` defaults to
  true). Scopes: `UserDelete` or `AdminWrite`.
- `GET /api/v1/users/{user_id}/memory-events` — the user's audit events, paginated, without input text. Scope:
  `AuditRead`.

## API keys

Database auth mode only. Scopes: `AdminRead` (list), `AdminWrite` (create, revoke).

```bash
curl -s -X POST "$MEMQUIRA_URL/api/v1/api-keys" \
  -H "Authorization: Bearer $ADMIN_KEY" -H "X-Organization-ID: $ORG" -H "Content-Type: application/json" \
  -d '{"name": "my-app", "scopes": ["MemoryRead", "MemoryWrite", "MemoryDelete"]}'
```

The response contains `raw_key` once; only a hash is stored. `GET /api/v1/api-keys` lists keys without secrets;
`DELETE /api/v1/api-keys/{api_key_id}` revokes one. The very first admin key of an organization is created with
`scripts/ops/bootstrap_admin_key.sh` ([deployment.md](deployment.md#3-api-keys)).

## Organization administration

Scopes `AdminRead` / `AdminWrite`. None of these return memory content.

| Endpoint | Purpose |
|---|---|
| `GET /api/v1/admin/org/summary` | Aggregate counts |
| `GET /api/v1/admin/org/users` | Users with memory aggregates |
| `GET /api/v1/admin/org/quotas` | Quota status and limits |
| `GET`, `PUT`, `DELETE /api/v1/admin/org/plan` | The organization's plan configuration |
| `GET /api/v1/admin/org/memory-events` | Audit events across the organization |
| `GET /api/v1/admin/org/provider-usage` | Provider calls and token usage (`limit` at most 100) |
| `GET /api/v1/admin/org/usage/provider/daily`, `/usage/dashboard`, `/usage/memory/snapshots` | Usage over time |
| `POST /api/v1/admin/org/provider-usage/retention/apply` | Prune old usage records |
| `GET /api/v1/admin/jobs`, `/jobs/runs`; `POST /api/v1/admin/jobs/{job_kind}/run` | Background jobs |
| `GET /api/v1/admin/org/cache/context/metrics`, `/cache/query-embedding/metrics`, `/embedding/diagnostics` | Process-local diagnostics |

## Health, metadata and metrics

These need no API key.

| Endpoint | Purpose |
|---|---|
| `GET /health/live` (`GET /health`) | Liveness: the process is up |
| `GET /health/ready` | Readiness: database state, Qdrant and provider checks |
| `GET /ready` | Readiness with backend labels and the quality tier |
| `GET /api/v1/version` | Version and build metadata |
| `GET /metrics` | Prometheus metrics when `MEMCORE_METRICS_ENABLED=true` (an API key is required unless `MEMCORE_METRICS_REQUIRE_AUTH=false`) |
| `GET /openapi.json`, `GET /docs` | OpenAPI document and Swagger UI |

## Error codes

| Code | HTTP | Meaning |
|---|---|---|
| `UNAUTHORIZED` | 401 | Missing or invalid API key |
| `FORBIDDEN` | 403 | Missing scope, or organization mismatch |
| `VALIDATION_ERROR` / `BAD_REQUEST` | 400 | Invalid body, header or query (including a missing `X-Organization-ID`) |
| `NOT_FOUND` | 404 | Unknown resource, or one that belongs to another user |
| `CONFLICT`, `MESSAGE_ID_CONFLICT` | 409 | Conflicting state; a `message_id` already names a different message |
| `IMPORT_ALREADY_APPLIED`, `IMPORT_ID_CONFLICT` | 409 | Append import would duplicate or collide with stored facts; nothing was imported |
| `PAYLOAD_TOO_LARGE` | 413 | Body over `MEMCORE_MAX_REQUEST_BODY_BYTES` |
| `RATE_LIMITED`, `QUOTA_EXCEEDED` | 429 | Per-organization rate limit or quota |
| `PROVIDER_GUARDRAIL_VIOLATION` | 403 | A provider guardrail refused the call (input size, output tokens, calls per process) |
| `PROVIDER_ERROR`, `PROVIDER_OUTPUT_INVALID`, `PROVIDER_OUTPUT_TRUNCATED` | 502 | The provider failed or its output could not be used |
| `OVERLOADED`, `REQUEST_DEADLINE_EXCEEDED`, `PROVIDER_THROTTLED` | 503 | Back off and retry after `Retry-After` |
| `TIMEOUT` | 504 | An upstream call timed out |
| `STORAGE_ERROR`, `MIGRATION_ERROR`, `INTERNAL_ERROR` | 500 | Server-side failure; see the logs for the `request_id` |

A request that fails with `503` or `504` may be retried. Writes are safe to retry when every message has a
`message_id`.
