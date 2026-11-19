#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_common.sh
source "${SCRIPT_DIR}/_common.sh"
require_common_env

# An explicit session and a stable id per message, created once. To retry this exact request
# (e.g. after 503 REQUEST_DEADLINE_EXCEEDED), run it again with the same three values exported;
# new values make it a new message. Reusing a message id for different content returns 409
# MESSAGE_ID_CONFLICT. Do not run the same request concurrently.
MEMQUIRA_SESSION_ID="${MEMQUIRA_SESSION_ID:-$(new_id session)}"
MESSAGE_ID_1="${MESSAGE_ID_1:-$(new_id msg)}"
MESSAGE_ID_2="${MESSAGE_ID_2:-$(new_id msg)}"
echo "MEMQUIRA_SESSION_ID=${MEMQUIRA_SESSION_ID} MESSAGE_ID_1=${MESSAGE_ID_1} MESSAGE_ID_2=${MESSAGE_ID_2}" >&2

curl -sS -X POST "${MEMQUIRA_BASE_URL}/api/v1/memories" \
  -H "Authorization: Bearer ${MEMQUIRA_API_KEY}" \
  -H "X-Organization-ID: ${MEMQUIRA_ORG_ID}" \
  -H "Content-Type: application/json" \
  -d "{
    \"user_id\": \"${MEMQUIRA_USER_ID}\",
    \"session_id\": \"${MEMQUIRA_SESSION_ID}\",
    \"messages\": [
      {\"role\": \"user\", \"message_id\": \"${MESSAGE_ID_1}\", \"content\": \"User prefers concise technical summaries.\"},
      {\"role\": \"assistant\", \"message_id\": \"${MESSAGE_ID_2}\", \"content\": \"Noted.\"}
    ],
    \"metadata\": {\"source\": \"examples/curl\"}
  }"
echo
