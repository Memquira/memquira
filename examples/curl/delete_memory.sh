#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_common.sh
source "${SCRIPT_DIR}/_common.sh"
require_common_env
require_env MEMORY_ID

curl -sS -X DELETE \
  "${MEMQUIRA_BASE_URL}/api/v1/users/${MEMQUIRA_USER_ID}/memories/${MEMORY_ID}" \
  -H "Authorization: Bearer ${MEMQUIRA_API_KEY}" \
  -H "X-Organization-ID: ${MEMQUIRA_ORG_ID}"
echo
