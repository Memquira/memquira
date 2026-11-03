#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=_common.sh
source "${SCRIPT_DIR}/_common.sh"
require_common_env

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required for full_flow.sh to extract memory ids" >&2
  exit 1
fi

echo "==> create"
# An explicit session and a stable id per message, created once; a retry of this request must
# resend this same body (same ids). See create_memory.sh.
SESSION_ID="$(new_id session)"
CREATE_BODY="$(jq -n \
  --arg user "${MEMCORE_USER_ID}" --arg session "${SESSION_ID}" \
  --arg m1 "$(new_id msg)" --arg m2 "$(new_id msg)" \
  '{user_id: $user, session_id: $session,
    messages: [
      {role: "user", message_id: $m1, content: "User prefers concise technical summaries."},
      {role: "assistant", message_id: $m2, content: "Noted."}
    ],
    metadata: {source: "examples/curl/full_flow"}}')"
CREATE_JSON="$(
  curl -sS -X POST "${MEMCORE_BASE_URL}/api/v1/memories" \
    -H "Authorization: Bearer ${MEMCORE_API_KEY}" \
    -H "X-Organization-ID: ${MEMCORE_ORG_ID}" \
    -H "Content-Type: application/json" \
    -d "${CREATE_BODY}"
)"
echo "${CREATE_JSON}" | jq '{status, summary, memory_ids: [.memories[].id]}'

MEMORY_ID="$(echo "${CREATE_JSON}" | jq -r '.memories[0].id // empty')"

echo "==> search"
curl -sS -X POST "${MEMCORE_BASE_URL}/api/v1/memories/search" \
  -H "Authorization: Bearer ${MEMCORE_API_KEY}" \
  -H "X-Organization-ID: ${MEMCORE_ORG_ID}" \
  -H "Content-Type: application/json" \
  -d "{
    \"user_id\": \"${MEMCORE_USER_ID}\",
    \"query\": \"technical summaries\",
    \"limit\": 5
  }" | jq '{status, result_count: (.results|length)}'

echo "==> context"
curl -sS -X POST "${MEMCORE_BASE_URL}/api/v1/context" \
  -H "Authorization: Bearer ${MEMCORE_API_KEY}" \
  -H "X-Organization-ID: ${MEMCORE_ORG_ID}" \
  -H "Content-Type: application/json" \
  -d "{
    \"user_id\": \"${MEMCORE_USER_ID}\",
    \"query\": \"How should replies be written?\",
    \"max_tokens\": 1000
  }" | jq '{status, context_chars: (.context|length), memory_count: (.memories|length)}'

if [[ -n "${MEMORY_ID}" ]]; then
  echo "==> delete ${MEMORY_ID}"
  curl -sS -X DELETE \
    "${MEMCORE_BASE_URL}/api/v1/users/${MEMCORE_USER_ID}/memories/${MEMORY_ID}" \
    -H "Authorization: Bearer ${MEMCORE_API_KEY}" \
    -H "X-Organization-ID: ${MEMCORE_ORG_ID}" | jq .
else
  echo "warning: create response had no memory id; skipping delete" >&2
fi
