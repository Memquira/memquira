# Deployment

This guide deploys Memquira on one Linux host with Docker Compose: the API server, PostgreSQL 16 and Qdrant 1.18.
It is the configuration the project tests; other orchestrators can run the same image with the same environment.

## Host

| Resource | Recommendation |
|---|---|
| OS | Linux (x86-64) with Docker Engine and Compose v2 |
| CPU / memory | 2 vCPU and 4 GB RAM for a small deployment. Each service has a memory limit (`MEMQUIRA_*_MEM_LIMIT`, 1 GB by default); on a 4 GB host lower them to about 768 MB each and add swap |
| Disk | 20–30 GB: images, logs (capped at 50 MB per service) and data volumes |
| Network | No inbound port except your reverse proxy; outbound HTTPS to the model provider |

Build the image on a machine with enough memory (the Rust release build needs several GB), or build once and copy it
(`docker save` / `docker load`).

## 1. Configure

```bash
git clone https://github.com/memquira/memquira.git
cd memquira
cp .env.production.example .env.production
chmod 600 .env.production
```

Edit `.env.production`:

- `POSTGRES_PASSWORD` and `MEMCORE_API_KEY_PEPPER`: long random values (`openssl rand -hex 32`). Keep the pepper
  stable: changing it invalidates every API key. Use URL-safe characters in the password.
- `MEMCORE_LLM_*` and `MEMCORE_EMBEDDING_*`: your provider ([providers.md](providers.md)).
- Review quotas, rate limits and resource limits.

## 2. Start

```bash
docker compose --env-file .env.production -f docker/docker-compose.prod.yml up -d --build
docker compose --env-file .env.production -f docker/docker-compose.prod.yml ps
curl -s http://127.0.0.1:8080/health/ready
```

The stack:

- runs PostgreSQL and Qdrant on an internal network with no published ports and no route to the internet;
- gives only the API container outbound network access (for the model provider);
- binds the API to `127.0.0.1:${MEMQUIRA_BIND_PORT}` (default 8080);
- restarts failed containers, rotates logs and applies healthchecks to every service;
- applies database migrations at startup.

The first start makes one embedding call to record the embedding model's identity on the new Qdrant collections.

## 3. API keys

Production uses database authentication: keys are stored as HMAC hashes, belong to one organization and carry
scopes. Create the organization's first admin key directly in the database, then use it to mint application keys:

```bash
ADMIN_KEY=$(./scripts/ops/bootstrap_admin_key.sh my-org)

curl -s -X POST http://127.0.0.1:8080/api/v1/api-keys \
  -H "Authorization: Bearer $ADMIN_KEY" -H "X-Organization-ID: my-org" -H "Content-Type: application/json" \
  -d '{"name": "my-app", "scopes": ["MemoryRead", "MemoryWrite", "MemoryDelete"]}'
```

Store the returned `raw_key` in your application's secret store; it is shown once. Give each application only the
scopes it needs (`UserDelete` for user erasure, `AdminRead` for usage reporting). Afterwards revoke the bootstrap key
(`DELETE /api/v1/api-keys/{api_key_id}`) or keep it offline.

## 4. Reverse proxy and TLS

Expose the API only through a TLS-terminating proxy (Caddy, nginx, a cloud load balancer, a tunnel), and only to
your own backend services. Memquira trusts the calling application for `user_id`, so browsers must never call it
directly. A minimal Caddy site:

```caddyfile
memory.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

Keep `/metrics` private or authenticated (`MEMCORE_METRICS_REQUIRE_AUTH=true`, the default).

## 5. Verify

```bash
KEY=mc_live_...   # the application key
curl -s -X POST https://memory.example.com/api/v1/memories \
  -H "Authorization: Bearer $KEY" -H "X-Organization-ID: my-org" -H "Content-Type: application/json" \
  -d '{"user_id":"smoke-user","messages":[{"role":"user","content":"I prefer green tea.","message_id":"smoke-1"}]}'
curl -s -X POST https://memory.example.com/api/v1/memories/search \
  -H "Authorization: Bearer $KEY" -H "X-Organization-ID: my-org" -H "Content-Type: application/json" \
  -d '{"user_id":"smoke-user","query":"What does the user like to drink?"}'
curl -s -X DELETE https://memory.example.com/api/v1/users/smoke-user \
  -H "Authorization: Bearer $KEY" -H "X-Organization-ID: my-org"   # needs UserDelete
```

## Production checklist

- [ ] `MEMCORE_ENV=production`, `MEMCORE_AUTH_MODE=database`, `MEMCORE_ALLOW_MOCK=false`.
- [ ] Secrets only in `.env.production` (mode 600) or a secret manager; nothing in git or the image.
- [ ] `MEMCORE_PROVIDER_MAX_CALLS_PER_RUN` raised from its test default of 10.
- [ ] A spending limit set with the model provider.
- [ ] API reachable only through TLS, only from your backend; PostgreSQL, Qdrant and `/metrics` private.
- [ ] Rate limits, admission control and quotas sized for your traffic.
- [ ] Backups of PostgreSQL and Qdrant scheduled, copied off the host, and a restore tested
      ([operations.md](operations.md)).
- [ ] Monitoring of `/health/ready` and the metrics endpoint.
- [ ] One API instance per database (rate limits, circuit breakers and caches are per process).

## Running elsewhere

The image (`docker/Dockerfile`, built with `--build-arg CARGO_FEATURES=postgres,qdrant`) runs as a non-root user,
listens on port 8080 and needs only environment variables. On Kubernetes or a container service, provide the same
variables as `.env.production`, point `MEMCORE_POSTGRES_URL` and `MEMCORE_QDRANT_URL` (gRPC, port 6334) at your
databases, and use `/health/live` and `/health/ready` as liveness and readiness probes.

## Stopping and removing

```bash
docker compose --env-file .env.production -f docker/docker-compose.prod.yml down      # keeps data
docker compose --env-file .env.production -f docker/docker-compose.prod.yml down -v   # deletes all memories
```
