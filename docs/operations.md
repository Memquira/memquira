# Operations

Backups, export and import, schema migrations, upgrades and maintenance for a running deployment. Commands use the
production Compose stack; set this shorthand first:

```bash
C="docker compose --env-file .env.production -f docker/docker-compose.prod.yml"
```

## Backups

PostgreSQL is the source of truth (facts, events, episodes, API keys); Qdrant holds the vectors. Back up both, at
the same time:

```bash
mkdir -p backup
$C exec -T postgres pg_dump -U memcore -d memcore -Fc > backup/memcore-$(date +%F).dump
$C exec -T memcore curl -s -X POST http://qdrant:6333/collections/memcore_vectors/snapshots
$C exec -T memcore curl -s -X POST http://qdrant:6333/collections/memcore_vectors_episodes/snapshots
$C cp qdrant:/qdrant/snapshots backup/qdrant-snapshots-$(date +%F)
```

Copy the dump and snapshots off the host and encrypt them: they contain user memories. Do not archive the Qdrant
volume with plain `tar` (its preallocated sparse files expand to gigabytes); use snapshots.

### Restore

```bash
$C stop memcore
$C exec -T postgres psql -U memcore -d postgres \
  -c "DROP DATABASE memcore WITH (FORCE)" -c "CREATE DATABASE memcore OWNER memcore"
$C exec -T postgres pg_restore -U memcore -d memcore --no-owner < backup/memcore-YYYY-MM-DD.dump
# Vectors: restore each collection from its snapshot (Qdrant API:
# POST /collections/<name>/snapshots/upload with the .snapshot file), then:
$C start memcore
```

Check a restored user's vectors against PostgreSQL, and repair them if needed (a repair re-embeds, which costs
provider calls):

```bash
$C exec memcore memcore-maintenance verify-vectors --org-id my-org --user-id <user>
$C exec memcore memcore-maintenance verify-vectors --org-id my-org --user-id <user> \
  --repair --confirm verify-vectors:my-org:<user>
```

Test a restore before you rely on it.

## Export and import

Per-user JSON export and import move one user's memories between deployments or organizations.

```bash
# export (format memcore.user_export.v1)
curl -s "$MEMQUIRA_URL/api/v1/users/alice/export?include_events=true" \
  -H "Authorization: Bearer $KEY" -H "X-Organization-ID: $ORG" > alice.json

# import: the body wraps the export; dry_run validates without writing
jq '{export: ., mode: "append", restore_events: false, dry_run: true}' alice.json |
  curl -s -X POST "$MEMQUIRA_URL/api/v1/users/alice/import" \
    -H "Authorization: Bearer $KEY" -H "X-Organization-ID: $ORG" -H "Content-Type: application/json" -d @-
```

Run it again with `"dry_run": false` to import. The response `summary` reports imported facts, events and
episodes and the validation result.

| Mode | Behaviour |
|---|---|
| `append` | Adds the exported facts to the user's existing data |
| `replace` | Deletes the user's existing facts, vectors and episodes, then imports the export exactly |

Imported facts are re-embedded with the deployment's own embedding model (provider cost). Before its first write an
append import checks every fact id it would use:

| Found for this user | Result |
|---|---|
| a fact with **identical content** under a planned id | `409 IMPORT_ALREADY_APPLIED`; nothing is imported |
| otherwise, a **deleted** fact under a planned id | `409 IMPORT_ID_CONFLICT`; nothing is imported |
| a fact with different content under a planned id | the imported fact gets a new id |

So retrying a failed append import, or applying the same export twice, is rejected instead of duplicating facts.
To recover, use `mode=replace` with the same export, and only when the export is the complete intended state of the
user. The check is a read before the writes, not a transaction: two concurrent imports of the same export are not
serialised.

**Limitation (exports without facts):** the check uses fact ids only. An export with no facts is not checked;
re-applying it in append mode inserts its episodes that have no `message_id` again (episodes with a `message_id`
are skipped as duplicates) and, with `restore_events=true`, restores its events again. Append import is therefore
not generally idempotent.

Other limits: export and import are per user (no organization-wide restore), JSON bodies only, unencrypted, and
exports are capped by fixed fact and event limits unless the opt-in complete export is used
(`complete=true`, with paginated reads of `page_size`).

## Migrations and upgrades

The server applies its schema migrations at startup (`MEMCORE_DATABASE_MIGRATION_MODE=auto`;
`validate_only` checks without applying). Applied migrations are checksummed; a database whose history differs from
the binary's is refused while `MEMCORE_DATABASE_REQUIRE_CLEAN_MIGRATIONS=true`.

Upgrade procedure:

1. Back up PostgreSQL and Qdrant (above).
2. Build or pull the new image and restart the stack; watch the logs for the applied migrations.
3. Check `GET /health/ready` and a search for a known user.

An older binary refuses to start on a newer schema. **Rolling back across a migration means restoring the
pre-upgrade backup**; data written after the backup is lost. There are no down-migrations.

`memcore-maintenance rollout-preflight --assessment first-c-plus-rollout|later-c-plus-upgrade` reports, read-only,
whether a store is ready for an upgrade that changes vector write handling.

## Maintenance CLI

`memcore-maintenance` ships in the image. Every command prints a JSON result; destructive commands need an exact
`--confirm` value.

| Command | Purpose |
|---|---|
| `forget-user --org-id <org> --user-id <user> --confirm forget-user:<org>:<user> [--dry-run]` | Delete a user's data directly in the stores, also when the API cannot start |
| `verify-vectors --org-id <org> --user-id <user> [--repair --confirm verify-vectors:<org>:<user>]` | Compare a user's vectors with PostgreSQL; repair missing or stale points |
| `rollout-preflight --assessment …` | Read-only upgrade readiness check |

`MEMCORE_PROTECTED_COLLECTIONS` lists extra Qdrant collections the CLI must never modify.

## Retention

With `MEMCORE_RETENTION_ENABLED=true`, `POST /api/v1/users/{user_id}/retention/apply` (dry run by default) removes
expired facts and audit events of one user. In-process background jobs (`MEMCORE_BACKGROUND_JOBS_ENABLED`) can run
retention and usage snapshots on a schedule for the organizations in `MEMCORE_BACKGROUND_JOB_ORG_IDS`.

## Monitoring

- **Probes:** `GET /health/live` for liveness, `GET /health/ready` for readiness (the Compose healthcheck).
- **Metrics:** `GET /metrics` in Prometheus format when `MEMCORE_METRICS_ENABLED=true`, authenticated by default.
  Useful series: request rate and latency by route, admission rejections, provider calls and errors by bounded
  labels, provider token counts, and ingest outcomes (stored, discarded, skipped).
- **Logs:** JSON lines (`MEMCORE_LOG_FORMAT=json`) with a `request_id` per request. Message content is not logged.
- **Provider usage:** with `MEMCORE_PROVIDER_USAGE_PERSISTENCE_ENABLED=true`,
  `GET /api/v1/admin/org/provider-usage` reports calls and tokens per organization.
- **Disk:** PostgreSQL and Qdrant volumes grow with stored memories; keep Qdrant on a Docker named volume or a Linux
  filesystem.
