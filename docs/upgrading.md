# Upgrading

## From 0.2.x: the Memcore → Memquira rename

Releases up to 0.2.x used the engine's earlier name, Memcore, in package names, binaries and configuration. This
release uses **Memquira** throughout.

Your data needs no migration:
- Nothing in the database schema, the stored vectors or the API routes changes.
- Existing configuration keeps working: every old `MEMCORE_*` setting is still accepted as an alias.

### What changed

| Before (0.2.x) | Now | Old name still works? |
|---|---|---|
| `MEMCORE_*` environment variables | `MEMQUIRA_*` | Yes, as a deprecated alias (see below) |
| `memcore-api`, `memcore-maintenance` binaries | `memquira-api`, `memquira-maintenance` | In the Docker image, as symlinks to the new binaries |
| Cargo packages `memcore-*` (`crates/memcore-*`) | `memquira-*` (`crates/memquira-*`) | No. Update `cargo -p` arguments and paths |
| `X-Memcore-Test-Source` request header | `X-Memquira-Test-Source` | Yes |
| Compose service `memcore` | `memquira` | No. See "Docker Compose" below |
| `openapi/memcore.openapi.json`, title `memcore API` | `openapi/memquira.openapi.json`, title `Memquira API` | No. The served `GET /openapi.json` is unchanged |
| `"service": "memcore"` in `/health` and in the startup log | `"service": "memquira"` | No |
| `MEMCORE_BUILD_*` build-time variables | `MEMQUIRA_BUILD_*` | Yes |
| Image user `memcore`; notices in `/usr/share/doc/memcore` | User `memquira` (same uid/gid 10001); `/usr/share/doc/memquira` | Ownership of existing volumes is unchanged |
| `examples/python/memcore_client.py`, `examples/node/memcore-client.js` | `memquira_client.py`, `memquira-client.js` | No |

### What stays the same on purpose

These names identify stored data, existing dashboards or credentials. Changing them would break existing
installations, so they keep their current values:

- **Prometheus metric names** (`memcore_*`). Dashboards and alerts keep working.
- **Storage defaults:**
  - the Qdrant collections `memcore_vectors` and `memcore_vectors_episodes`;
  - the SQLite file `./data/memcore.db`;
  - the PostgreSQL role and database `memcore` in the Compose files;
  - the backup file prefix `memcore-sqlite-`.

  Set them explicitly if you want different names for a new installation.
- **Compose project names** (`memcore-reference`, `memcore-local-starter`). Docker derives volume names from them.
- **The default development keys** (`memcore_dev_key`, `memcore_local_starter_key`). Set your own with
  `MEMQUIRA_DEV_API_KEY` in any shared environment.
- **The API key format** (`mc_live_…`). Existing keys keep working, as long as the pepper keeps its value. When
  you rename `MEMCORE_API_KEY_PEPPER` to `MEMQUIRA_API_KEY_PEPPER`, copy the value exactly. With a different
  value, every stored key stops verifying. If the old name is still set too, startup refuses to start instead.
- **Export document versions** (`memcore.user_export.v1`–`v3`). Old exports import as before.
- **Database migrations.** The migration files are byte-identical to 0.2.x, so their recorded checksums still
  match and an existing database starts without a migration step.

### Configuration aliases

Each `MEMQUIRA_X` setting also accepts its old name `MEMCORE_X`:

- **Only the old name set:** it is used, and startup logs `MEMCORE_X is deprecated; rename it to MEMQUIRA_X`.
  `memquira-maintenance` prints the same warning to stderr.
- **Both set to the same value:** accepted, with the same warning.
- **Both set to different values:** startup fails with a configuration error that names both variables. The error
  never prints the values.

Rename at your own pace. On Linux or macOS, this rewrites an environment file:

```bash
sed -i.bak 's/^MEMCORE_/MEMQUIRA_/' .env
```

The aliases will be removed only in a later release. The changelog will announce the removal at least one minor
release ahead.

### Docker Compose

1. Update your checkout. Keep your `.env` file: Compose also reads the old variable names
   (`MEMCORE_HOST_PORT`, `MEMCORE_IMAGE`, `MEMCORE_DEV_API_KEY` and the others) when the new ones are unset.
2. Recreate the stack, removing the container that still uses the old service name:

   ```bash
   docker compose -f docker/docker-compose.prod.yml up -d --remove-orphans
   ```

   Without `--remove-orphans`, the old `memcore` container keeps running and holds the API port.
3. Volumes are untouched. Check with `docker volume ls` before and after: the same volumes are mounted.
4. Commands that ran the old binaries inside the container keep working, for example
   `docker compose exec memquira memcore-maintenance …`. Prefer the new names in new scripts.

### Building from source

```bash
cargo build --release -p memquira-api        # was -p memcore-api
cargo run -p memquira-api --bin memquira-maintenance   # prints the subcommands
```

Test helpers read `MEMQUIRA_TEST_POSTGRES_URL` and `MEMQUIRA_TEST_QDRANT_URL`. The old `MEMCORE_TEST_*` names still
work.

### Rolling back

The rename changes no stored data and adds no migration, so a 0.2.x binary can run against the same data again,
including memories, history and deletions written by this release. To roll back:

1. If you renamed your environment variables, rename them back: 0.2.x reads only `MEMCORE_*`.
2. Restore the `memcore` Compose service from your previous checkout, and recreate the stack with
   `--remove-orphans`.
3. Run `memcore-maintenance verify-vectors` once the old version is up.

No backup restore is needed for this upgrade alone. If you upgrade across a release that adds a schema migration,
back up first: an older binary refuses to start on a newer schema, and rolling back then means restoring that
backup.
