# Providers

Memquira needs two models: an **LLM** that extracts and classifies facts, and an **embedding model** that turns facts
and queries into vectors. Both are reached through the OpenAI HTTP API, so OpenAI and any server that implements the
same endpoints work. The two roles are configured independently and can point at different services.

| Provider name | Use |
|---|---|
| `openai` | OpenAI or any OpenAI-compatible server (`/chat/completions` with JSON output, `/embeddings`) |
| `mock` | Deterministic offline models for development and tests; refused unless `MEMCORE_ALLOW_MOCK=true` |

Native clients for other APIs (Anthropic, Bedrock, ...) are not included; use an OpenAI-compatible endpoint in front
of them if you need one.

## OpenAI

```bash
MEMCORE_LLM_PROVIDER=openai
MEMCORE_LLM_MODEL=gpt-4o-mini
MEMCORE_LLM_BASE_URL=https://api.openai.com/v1
MEMCORE_LLM_API_KEY=sk-...
MEMCORE_EMBEDDING_PROVIDER=openai
MEMCORE_EMBEDDING_MODEL=text-embedding-3-small
MEMCORE_EMBEDDING_BASE_URL=https://api.openai.com/v1
MEMCORE_EMBEDDING_API_KEY=sk-...
MEMCORE_EMBEDDING_DIMENSIONS=1536
MEMCORE_REAL_PROVIDER_CALLS_ENABLED=true
MEMCORE_PROVIDER_TEST_MODE=production
MEMCORE_PROVIDER_MAX_CALLS_PER_RUN=1000000
MEMCORE_REQUIRE_QUALITY_TIER=compatible-unvalidated
```

Use a dedicated OpenAI project with a monthly budget limit for the server's key. `OPENAI_API_KEY` and
`OPENAI_BASE_URL` are used for a role whose own key or URL is unset.

## Other OpenAI-compatible servers

Any server with `/chat/completions` (JSON mode) and `/embeddings` works: vLLM, LiteLLM, Ollama, Hugging Face TEI
(embeddings), OpenRouter and similar.

| Setting | Notes |
|---|---|
| `MEMCORE_<ROLE>_BASE_URL` | The server's `/v1` URL |
| `MEMCORE_<ROLE>_ALLOW_NO_API_KEY=true` | For a server without authentication that is not on loopback (loopback needs no key) |
| `MEMCORE_PROVIDER_ALLOW_INSECURE_HTTP=true` | Needed to send a key over plain `http` to a non-loopback host; prefer TLS |
| `MEMCORE_LLM_JSON_MODE` | `json_object` (default) or `json_schema` (sends the expected schema; needed by some local models) |
| `MEMCORE_EMBEDDING_DIMENSIONS` | Vector size; the collection is bound to it |
| `MEMCORE_EMBEDDING_SEND_DIMENSIONS=false` | For servers that reject the `dimensions` request field |
| `MEMCORE_EMBEDDING_QUERY_PREFIX`, `MEMCORE_EMBEDDING_DOCUMENT_PREFIX` | Instruction prefixes some embedding models expect |

[`profiles/byom.env.example`](../profiles/byom.env.example) is a complete template for separate chat and embedding
endpoints.

## Fully local models

[`docker/docker-compose.local-starter.yml`](../docker/docker-compose.local-starter.yml) runs the API with Qdrant,
Hugging Face TEI serving `BAAI/bge-small-en-v1.5` for embeddings, and Ollama serving `qwen2.5:3b-instruct` for
extraction. No API keys and no network calls leave the host.

```bash
docker compose -f docker/docker-compose.local-starter.yml up -d --build
```

On hosts where TEI does not run (for example arm64), add
`-f docker/docker-compose.local-starter.ollama-embed.yml` to use Ollama for embeddings as well. Settings are in
[`profiles/local-starter.env.example`](../profiles/local-starter.env.example) and `docker/local-starter.env.example`.

Small local models are much weaker extractors than hosted models: on a laptop CPU one ingest request takes about a
minute or more, some stored facts are fragments, and update and delete decisions are unreliable. Use the local
starter to try the API offline, not to judge memory quality.

## Embedding identity

A Qdrant collection records the identity of the embedding model that filled it (model, dimension, prefixes,
normalisation). At startup the server checks it and refuses to run against a collection built with a different
model, because mixing vector spaces silently breaks retrieval. To change the embedding model, use new collection
names (`MEMCORE_QDRANT_COLLECTION`, `MEMCORE_QDRANT_EPISODE_COLLECTION`) and re-import, or erase the old data.

`MEMCORE_PROVIDER_PROBE=on` (default) makes one embedding call at startup to verify this identity; with a paid
provider each start costs that one call.

## Reliability

| Mechanism | Settings | Behaviour |
|---|---|---|
| Timeout | `MEMCORE_PROVIDER_TIMEOUT_SECONDS` | Per attempt; the HTTP client waits at least 90 s |
| Retries | `MEMCORE_PROVIDER_MAX_RETRIES` (capped by `MEMCORE_PROVIDER_MAX_RETRIES_PER_CALL`), backoff settings | Timeouts, 429 and 5xx are retried with exponential backoff and jitter; a provider's `Retry-After` is honoured up to 30 s |
| Circuit breaker | `MEMCORE_PROVIDER_CIRCUIT_BREAKER_*` | After repeated exhausted retries a provider operation is skipped for a cool-down period |
| Fallback | `MEMCORE_PROVIDER_FALLBACK_ENABLED`, `MEMCORE_*_FALLBACK_ORDER` | Optional ordered list of providers |
| Concurrency | `MEMCORE_PROVIDER_CHAT_MAX_CONCURRENCY`, `MEMCORE_PROVIDER_EMBEDDING_MAX_CONCURRENCY` | Process-wide bounds; waiting beyond `MEMCORE_PROVIDER_PERMIT_TIMEOUT_MS` returns `503 OVERLOADED` |
| Guardrails | `MEMCORE_PROVIDER_MAX_CALLS_PER_RUN`, `MEMCORE_PROVIDER_MAX_INPUT_CHARS`, `MEMCORE_PROVIDER_MAX_OUTPUT_TOKENS` | Refuse calls outside the bounds (`PROVIDER_GUARDRAIL_VIOLATION`); see [configuration.md](configuration.md#provider-guardrails) |

Authentication, validation and malformed-response errors are not retried. Provider errors returned to clients never
include keys, prompts or request bodies.

## Cost

Each ingest request makes one extraction call per window of messages, one classification call per candidate fact
(skippable for candidates without related facts with `MEMCORE_CLASSIFICATION_SKIP_WHEN_NO_RELATED=true`), and embedding calls for what it stores. Searches and context requests make one
embedding call for the query. With `MEMCORE_PROVIDER_USAGE_PERSISTENCE_ENABLED=true` the token usage of every call is
recorded and reported by `GET /api/v1/admin/org/provider-usage`. Memquira does not enforce a monetary budget: set
spending limits with your provider.

## Quality tiers

`GET /ready` reports the tier of the running configuration, and `MEMCORE_REQUIRE_QUALITY_TIER` makes the server
refuse to start in any other tier:

| Tier | Meaning |
|---|---|
| `development-mock` | Mock providers: tests and local development only |
| `compatible-unvalidated` | A real provider and configuration this project has not evaluated, including OpenAI. Expected for most deployments |
| `reference-validated` | The maintainers' reference configuration; a statement about that historical evaluation, not a live measurement |

This project publishes no benchmark results. Measure retrieval and answer quality on your own data with your own
models.
