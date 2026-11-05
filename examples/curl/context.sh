#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_common.sh
source "${SCRIPT_DIR}/_common.sh"
require_common_env

QUERY="${1:-How should replies be written?}"

curl -sS -X POST "${MEMCORE_BASE_URL}/api/v1/context" \
  -H "Authorization: Bearer ${MEMCORE_API_KEY}" \
  -H "X-Organization-ID: ${MEMCORE_ORG_ID}" \
  -H "Content-Type: application/json" \
  -d "{
    \"user_id\": \"${MEMCORE_USER_ID}\",
    \"query\": \"${QUERY}\",
    \"max_tokens\": 1000
  }"
echo
