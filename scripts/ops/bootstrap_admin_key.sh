#!/usr/bin/env bash
# Create the first database-mode API key of an organization (scopes admin_read + admin_write).
#
# Database auth has no unauthenticated key endpoint: POST /api/v1/api-keys itself needs an admin key.
# This script inserts one admin key directly into PostgreSQL, hashed exactly as the server hashes keys
# (hex HMAC-SHA256 of the raw key, keyed with MEMCORE_API_KEY_PEPPER). Use it only to mint scoped keys
# through the API (docs/deployment.md), then revoke it or keep it offline.
#
# usage (from the repository root, stack running):  ./scripts/ops/bootstrap_admin_key.sh <org_id> [key_name]
# The raw key is printed once on stdout. Requires: bash, openssl, docker compose.
set -euo pipefail

ORG=${1:?usage: bootstrap_admin_key.sh <org_id> [key_name]}
NAME=${2:-bootstrap-admin}
ENV_FILE=${MEMQUIRA_ENV_FILE:-.env.production}
COMPOSE_FILE=${MEMQUIRA_COMPOSE_FILE:-docker/docker-compose.prod.yml}

[[ -f $ENV_FILE ]] || { echo "env file not found: $ENV_FILE" >&2; exit 2; }
# The Compose file reads MEMQUIRA_ENV_FILE too, but resolves relative paths from docker/. Pass an absolute
# path so the script and Compose read the same file.
ENV_FILE="$(cd "$(dirname "$ENV_FILE")" && pwd)/$(basename "$ENV_FILE")"
export MEMQUIRA_ENV_FILE=$ENV_FILE

[[ $ORG =~ ^[A-Za-z0-9_.-]{1,128}$ ]] || { echo "org_id must match [A-Za-z0-9_.-]{1,128}" >&2; exit 2; }
[[ $NAME =~ ^[A-Za-z0-9_.\ -]{1,128}$ ]] || { echo "key_name must match [A-Za-z0-9_. -]{1,128}" >&2; exit 2; }

PEPPER=$(grep -E '^MEMCORE_API_KEY_PEPPER=' "$ENV_FILE" | head -1 | cut -d= -f2-)
if [[ -z $PEPPER || $PEPPER == CHANGE_ME* ]]; then
  echo "set MEMCORE_API_KEY_PEPPER in $ENV_FILE first" >&2
  exit 2
fi

RAW="mc_live_$(openssl rand -hex 16)"
HASH=$(printf '%s' "$RAW" | openssl dgst -sha256 -hmac "$PEPPER" | awk '{print $NF}')

docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" \
  exec -T postgres psql -U memcore -d memcore -v ON_ERROR_STOP=1 -q \
  -v org="$ORG" -v name="$NAME" -v hash="$HASH" >/dev/null <<'SQL'
INSERT INTO api_keys (id, org_id, name, key_hash, scopes, created_at)
VALUES (gen_random_uuid(), :'org', :'name', :'hash', ARRAY['admin_read', 'admin_write'], now());
SQL

echo "$RAW"
