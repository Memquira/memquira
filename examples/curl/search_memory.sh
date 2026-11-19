#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_common.sh
source "${SCRIPT_DIR}/_common.sh"
require_common_env

QUERY="${1:-technical summaries}"

curl -sS -X POST "${MEMQUIRA_BASE_URL}/api/v1/memories/search" \
  -H "Authorization: Bearer ${MEMQUIRA_API_KEY}" \
  -H "X-Organization-ID: ${MEMQUIRA_ORG_ID}" \
  -H "Content-Type: application/json" \
  -d "{
    \"user_id\": \"${MEMQUIRA_USER_ID}\",
    \"query\": \"${QUERY}\",
    \"limit\": 5
  }"
echo
