# Security

How Memquira protects data, what it expects from the deployment around it, and what it does not do. To report a
vulnerability, see [SECURITY.md](../SECURITY.md).

## Trust model

- **Applications authenticate, end users do not.** Memquira authenticates the calling application with an API key
  and trusts it for the `user_id` it sends. Call it only from your backend; a browser or mobile client must never
  hold a Memquira key or choose a `user_id`.
- **Organizations are the tenant boundary.** In database auth mode a key belongs to one organization; every read
  and write is scoped to that organization and to the request's user. Data of one organization is not reachable
  with another organization's key, and one user's memories are not returned for another user.
- **Model providers see conversation content.** Messages (after PII redaction) are sent to the configured LLM and
  embedding endpoints. Choose providers whose data handling you accept, or run local models.

## Authentication

| Mode | Use |
|---|---|
| `database` | Production. Keys `mc_live_…` are stored only as HMAC-SHA256 hashes keyed with `MEMCORE_API_KEY_PEPPER`, belong to one organization and carry scopes. The raw key is shown once at creation |
| `dev` | Local development only: one shared plaintext key (`MEMCORE_DEV_API_KEY`) for every organization, no scope checks |

Give each application the smallest set of scopes it needs, rotate keys by creating a new one and revoking the old,
and keep the pepper stable (changing it invalidates every key).

## Request protection

- **Rate limits** per organization (`MEMCORE_RATE_LIMIT_*`) and **admission control** per request class, with
  deadlines (`MEMCORE_ADMISSION_*`).
- **Body size limit** (`MEMCORE_MAX_REQUEST_BODY_BYTES`, 1 MiB by default) and JSON content-type validation.
- **Security headers** (`MEMCORE_SECURITY_HEADERS_ENABLED`): `X-Content-Type-Options`, `X-Frame-Options`,
  `Referrer-Policy`, `Permissions-Policy` and `Cache-Control: no-store`. HSTS is opt-in; set it on the TLS proxy.
- **CORS** is off by default. Keep it off: browsers should not call the API.
- **Request ids** are accepted only when short and safe, and are used for correlation, never for authentication.

## Data handling

- **PII redaction** (`MEMCORE_ENABLE_PII_REDACTION`, on by default) replaces e-mail addresses, phone numbers and card numbers
  in message content before extraction and storage. It is pattern-based, not a
  complete data-loss-prevention system.
- **Logs** do not contain message content, keys, connection strings or raw provider payloads. They do contain
  organization and user ids, so treat logs as sensitive.
- **Admin and audit endpoints** never return raw keys, key hashes, input text, prompts or memory content.
- **Deletion:** deleting a memory soft-deletes the row and removes its vector; erasing a user removes the user's
  facts, vectors and episodes. Backups still contain deleted data until they expire.
- **Encryption at rest** is the job of the storage layer (encrypted disks or managed databases); Memquira does not
  encrypt data itself.

## Deployment hardening

- Terminate TLS in front of the API and expose it only to your backend.
- Keep PostgreSQL and Qdrant on a private network (the production Compose stack uses an internal network with no
  published ports). Qdrant has no authentication in that stack.
- Keep `/metrics` private or authenticated (`MEMCORE_METRICS_REQUIRE_AUTH=true`, the default; database mode also
  requires an admin scope to scrape).
- Run with `MEMCORE_ENV=production`, `MEMCORE_ALLOW_MOCK=false` and the context cache disabled.
- Keep secrets in `.env.production` (mode 600) or a secret manager; never in git, images or logs.
- Set a spending limit with your model provider: Memquira bounds calls per process but does not enforce a monetary
  budget.
- Back up, encrypt the backups and test restores ([operations.md](operations.md)).

The container image runs as the non-root user `memcore` (uid 10001), contains only the two binaries, CA
certificates and `curl` for the healthcheck, and ships its licence notices under `/usr/share/doc/memcore`.

## Supply chain

CI runs `cargo audit` and `cargo deny` (advisories, licences, banned crates, allowed sources) and a secret scan on
every change. Base images are pinned by digest. Run `cargo deny check` before upgrading dependencies.

## Not provided

Memquira has no end-user authentication (OAuth/OIDC), no WAF or bot protection, no mutual TLS, no field-level
encryption, and no cross-process rate limiting. It has not been audited by a third party.
