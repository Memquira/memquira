# Getting started

This guide runs Memquira locally, stores a memory, finds it again and builds a prompt context for a later
conversation. The first part needs only Docker; the second switches to a real model.

## 1. Prerequisites

- Docker with Compose v2 (`docker compose version`).
- `curl`. The JSON output is easier to read with [`jq`](https://jqlang.github.io/jq/), which is optional.
- About 3 GB of free disk for the build image. The first build compiles the Rust release binary and takes several
  minutes; later builds reuse the cache.

## 2. Start the local stack

```bash
git clone https://github.com/memquira/memquira.git
cd memquira
docker compose -f docker/docker-compose.local.yml up -d --build
```

The local stack (`docker/docker-compose.local.yml`) runs only the API server, with:

- SQLite for facts and audit events, on the `memcore_data` volume;
- an in-memory mock vector store;
- deterministic mock LLM and embedding providers: no API keys, no network calls, no cost;
- `dev` authentication with the fixed key `memcore_dev_key`, bound to `127.0.0.1` only.

Mock models make the API behave end to end, but they do not understand language: they show the plumbing, not memory
quality.

Wait until the server is ready:

```bash
curl -s http://127.0.0.1:8080/health/ready
curl -s http://127.0.0.1:8080/api/v1/version
```

## 3. Store a memory

Every protected request sends an API key and the organization it acts for. `user_id` is your application's id for
the end user; memories are isolated per organization and per user.

```bash
export MEMQUIRA_URL=http://127.0.0.1:8080
export AUTH="Authorization: Bearer memcore_dev_key"
export ORG="X-Organization-ID: demo"

curl -s -X POST "$MEMQUIRA_URL/api/v1/memories" -H "$AUTH" -H "$ORG" -H "Content-Type: application/json" \
  -d '{
        "user_id": "alice",
        "session_id": "chat-1",
        "messages": [
          {"role": "user", "content": "I am vegetarian and I live in Lisbon.", "message_id": "m-1"},
          {"role": "user", "content": "I prefer meetings after 2pm.", "message_id": "m-2"}
        ]
      }'
```

The response lists the stored memories and a `summary` of what extraction did (`extracted`, `added`, `updated`,
`deleted`, `noop`, ...). `message_id` is optional but recommended: sending the same message again is recognised and
not stored twice, and reusing an id for a different message is rejected with `409 MESSAGE_ID_CONFLICT`.

## 4. Search

```bash
curl -s -X POST "$MEMQUIRA_URL/api/v1/memories/search" -H "$AUTH" -H "$ORG" -H "Content-Type: application/json" \
  -d '{"user_id": "alice", "query": "vegetarian Lisbon", "limit": 5}'
```

Results are ranked; `score` is the fused ranking score and `vector_score` the raw similarity of the vector leg.
With mock embeddings only shared words match, which is why this example query repeats words from the message.

List everything stored for the user:

```bash
curl -s "$MEMQUIRA_URL/api/v1/users/alice/memories?limit=20" -H "$AUTH" -H "$ORG"
```

## 5. Build context for a later interaction

In a new session, ask Memquira for the memories that matter for the next prompt, packed into a token budget:

```bash
curl -s -X POST "$MEMQUIRA_URL/api/v1/memories/context" -H "$AUTH" -H "$ORG" -H "Content-Type: application/json" \
  -d '{"user_id": "alice", "query": "Plan a team dinner with this user", "token_budget": 500}'
```

`text` is ready to place in a system prompt; `token_count` and `selected_count` tell you how much of the budget it
used. `POST /api/v1/context` is an alternative with more formatting options (sections, scores, timestamps).

## 6. Delete data

```bash
# one memory (its id comes from the list or search results)
curl -s -X DELETE "$MEMQUIRA_URL/api/v1/users/alice/memories/<memory_id>" -H "$AUTH" -H "$ORG"
# everything stored for the user: facts, vectors and episodes
curl -s -X DELETE "$MEMQUIRA_URL/api/v1/users/alice" -H "$AUTH" -H "$ORG"
```

## 7. Stop the stack

```bash
docker compose -f docker/docker-compose.local.yml down      # keeps the SQLite volume
docker compose -f docker/docker-compose.local.yml down -v   # also deletes it
```

## Use a real model

To see real extraction and retrieval quality, run the production stack (PostgreSQL + Qdrant) with an
OpenAI-compatible provider:

```bash
cp .env.production.example .env.production
# set POSTGRES_PASSWORD, MEMCORE_API_KEY_PEPPER (openssl rand -hex 32) and your provider keys
chmod 600 .env.production
docker compose --env-file .env.production -f docker/docker-compose.prod.yml up -d --build
curl -s http://127.0.0.1:8080/health/ready
```

This stack uses database authentication: create the first admin key, then an application key with only the scopes
it needs:

```bash
ADMIN_KEY=$(./scripts/ops/bootstrap_admin_key.sh demo)
curl -s -X POST http://127.0.0.1:8080/api/v1/api-keys \
  -H "Authorization: Bearer $ADMIN_KEY" -H "X-Organization-ID: demo" -H "Content-Type: application/json" \
  -d '{"name": "my-app", "scopes": ["MemoryRead", "MemoryWrite", "MemoryDelete"]}'
# -> {"raw_key": "mc_live_…", …}   shown once; store it as a secret
```

Use the returned key in place of `memcore_dev_key` and repeat steps 3 to 5. Every write and search now makes real,
billed provider calls; set a spending limit with your provider. Production details: [deployment.md](deployment.md).

To run without any cloud service, `docker/docker-compose.local-starter.yml` uses local models through Ollama and
Hugging Face TEI ([providers.md](providers.md)).

## Without Docker

```bash
cp .env.example .env            # set MEMCORE_DEV_API_KEY to any local value
mkdir -p data                   # the SQLite database is created here
cargo run -p memcore-api        # reads .env from the working directory
```

The default build has SQLite and the mock providers; add `--features postgres,qdrant` for the production backends
([development.md](development.md)).

## Next steps

- [API reference](api-reference.md): every endpoint, request and response field, and error code.
- [Configuration](configuration.md): retrieval, limits, providers.
- [Examples](../examples/): Python, Node.js and curl clients.
