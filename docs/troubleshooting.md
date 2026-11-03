# Troubleshooting

Startup problems are reported once, with the setting that caused them, and the server exits. Start with the logs:

```bash
docker compose --env-file .env.production -f docker/docker-compose.prod.yml logs --tail 100 memcore
```

## Startup

| Symptom | Cause and fix |
|---|---|
| refuses to start: mock provider | A provider resolves to `mock`. Configure a real provider, or set `MEMCORE_ALLOW_MOCK=true` for local development only |
| refuses to start: quality tier | `MEMCORE_REQUIRE_QUALITY_TIER` does not match the configuration's tier. With your own provider use `compatible-unvalidated` |
| missing API key for a provider role | Set `MEMCORE_LLM_API_KEY` / `MEMCORE_EMBEDDING_API_KEY` (or `OPENAI_API_KEY`); for an unauthenticated non-loopback server set `MEMCORE_<ROLE>_ALLOW_NO_API_KEY=true` |
| refuses to send a key over http | Use `https`, or set `MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP=true` for a private network |
| embedding identity mismatch | The Qdrant collection was built with another embedding model or dimension. Use new collection names or erase the old data ([providers.md](providers.md#embedding-identity)) |
| migration validation failed | The database's migration history differs from the binary's, often after a rollback. Restore the matching backup ([operations.md](operations.md#migrations-and-upgrades)) |
| Qdrant connection errors | `MEMCORE_QDRANT_URL` must use the gRPC port 6334, not the REST port 6333 |
| PostgreSQL authentication or URL errors | Check `POSTGRES_PASSWORD`; use URL-safe characters (hex), because it is embedded in a connection URL |
| context cache refused | Keep `MEMCORE_CONTEXT_CACHE_BACKEND=disabled` outside local single-process development |

## Requests

| Response | Cause and fix |
|---|---|
| `401 UNAUTHORIZED` | Missing or wrong `Authorization: Bearer` key; in `dev` mode the key is `MEMCORE_DEV_API_KEY` |
| `403 FORBIDDEN` | The key lacks the route's scope ([api-reference.md](api-reference.md#authentication-and-tenancy)) |
| `400 VALIDATION_ERROR` mentioning `X-Organization-ID` | The header is missing or empty |
| `400` from `POST /api/v1/memories/context` | Set `MEMCORE_SEARCH_MAX_LIMIT=100` (episodes are optional) |
| `403 PROVIDER_GUARDRAIL_VIOLATION` after a few requests | `MEMCORE_PROVIDER_MAX_CALLS_PER_RUN` is still at its test default of 10; raise it (the production template uses `1000000`) |
| `403 PROVIDER_GUARDRAIL_VIOLATION` on long conversations | The extraction window exceeds `MEMCORE_PROVIDER_MAX_INPUT_CHARS`; raise it moderately or send shorter sessions |
| `409 MESSAGE_ID_CONFLICT` | A `message_id` was reused for a different message; ids must be unique per user and session |
| `429 RATE_LIMITED` / `QUOTA_EXCEEDED` | Per-organization limits; adjust `MEMCORE_RATE_LIMIT_REQUESTS_PER_MINUTE` or the quotas |
| `502 PROVIDER_ERROR` / `PROVIDER_OUTPUT_INVALID` | The model failed or returned unusable output; check the provider and model, and `MEMCORE_LLM_JSON_MODE` for local models |
| `503 OVERLOADED` | Admission limits reached; retry after `Retry-After`, or raise `MEMCORE_ADMISSION_*` if the host has capacity |
| A memory is not stored | Extraction judged it unimportant (`MEMCORE_MIN_IMPORTANCE`) or a duplicate; the response `summary` shows `discarded` and `skipped` counts |

Include the `request_id` from the error response when you report a problem.

## Local stack

- **Search finds nothing with mock models:** mock embeddings do not capture meaning; use words that appear in the
  stored text, or run a real model.
- **Port 8080 already in use:** change the published port in the Compose file, or `MEMQUIRA_BIND_PORT` for the
  production stack.
- **Qdrant uses gigabytes on Windows:** keep Qdrant data on a Docker named volume (the default), not a Windows
  bind mount.
