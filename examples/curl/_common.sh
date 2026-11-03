#!/usr/bin/env bash
# Shared helpers for memcore curl examples. Sourced by other scripts.
# Does not print API keys or bearer tokens.

set -euo pipefail

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "error: missing required environment variable: ${name}" >&2
    exit 1
  fi
}

require_common_env() {
  require_env MEMCORE_BASE_URL
  require_env MEMCORE_API_KEY
  require_env MEMCORE_ORG_ID
  MEMCORE_USER_ID="${MEMCORE_USER_ID:-user_demo}"
}

# A random id for a new session or message (no external tools needed).
new_id() {
  printf '%s_%s\n' "$1" "$(od -An -tx1 -N16 /dev/urandom | tr -d ' \n')"
}

auth_headers() {
  # Intentionally expand into curl -H args without echoing the key.
  printf '%s\n' \
    "Authorization: Bearer ${MEMCORE_API_KEY}" \
    "X-Organization-ID: ${MEMCORE_ORG_ID}"
}
