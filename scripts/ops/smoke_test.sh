#!/usr/bin/env bash
# Safe smoke test against a running memcore API.
#
# Unauthenticated (default — read-only operational checks):
#   ./scripts/ops/smoke_test.sh http://localhost:8080
#
# Authenticated write/read/delete (requires explicit flag):
#   MEMCORE_SMOKE_TEST_API_KEY=... \
#   MEMCORE_SMOKE_TEST_ORG_ID=org_smoke \
#   MEMCORE_SMOKE_TEST_USER_ID=smoke-test-user \
#   ./scripts/ops/smoke_test.sh http://localhost:8080 --authenticated
#
# Never prints the API key. Requires curl.
# Authenticated cleanup requires jq or python3 to parse the created memory id.
# Do not run destructive broad cleanup. Authenticated mode only touches the smoke-test user.

set -euo pipefail

BASE_URL=""
AUTHENTICATED=0

usage() {
  echo "usage: $0 <base-url> [--authenticated]" >&2
  echo "  Unauthenticated: GET /health /ready /api/v1/version" >&2
  echo "  --authenticated: also create/search/context/delete a smoke-test memory" >&2
  echo "  Requires MEMCORE_SMOKE_TEST_API_KEY (and preferably MEMCORE_SMOKE_TEST_ORG_ID)" >&2
  echo "  Authenticated mode requires jq or python3 for reliable delete cleanup" >&2
  exit 1
}

for arg in "$@"; do
  case "$arg" in
    --authenticated) AUTHENTICATED=1 ;;
    -h|--help) usage ;;
    http://*|https://*) BASE_URL="$arg" ;;
    *)
      if [[ -z "$BASE_URL" ]]; then
        BASE_URL="$arg"
      else
        echo "error: unexpected argument: $arg" >&2
        usage
      fi
      ;;
  esac
done

if [[ -z "$BASE_URL" ]]; then
  usage
fi

# Trim trailing slash
BASE_URL="${BASE_URL%/}"

if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required" >&2
  exit 1
fi

API_KEY="${MEMCORE_SMOKE_TEST_API_KEY:-}"
ORG_ID="${MEMCORE_SMOKE_TEST_ORG_ID:-org_smoke}"
USER_ID="${MEMCORE_SMOKE_TEST_USER_ID:-smoke-test-user}"
EXPECT_REAL_PROVIDER="${MEMCORE_SMOKE_TEST_EXPECT_REAL_PROVIDER:-false}"

fail() {
  echo "error: $*" >&2
  exit 1
}

# Prefer python3, then python (Windows), for JSON helpers.
json_python() {
  if command -v python3 >/dev/null 2>&1; then
    echo "python3"
  elif command -v python >/dev/null 2>&1; then
    echo "python"
  else
    echo ""
  fi
}

require_json_tool_for_cleanup() {
  if command -v jq >/dev/null 2>&1; then
    return 0
  fi
  if [[ -n "$(json_python)" ]]; then
    return 0
  fi
  fail "authenticated smoke requires jq or python3 to parse memory id for delete cleanup"
}

# Parse AddMemoryResponse for cleanup.
# Shape: { "status":"success", "summary":{...}, "memories":[{"id":"..."}] }
# Sets MEMORY_ID when a created/updated memory exists.
# Returns 0 when cleanup should run, 2 when no memory was created (noop/empty),
# 1 on parse failure. Never prints memory content or secrets.
parse_created_memory_response() {
  local file="$1"
  MEMORY_ID=""
  local py
  py="$(json_python)"

  if command -v jq >/dev/null 2>&1; then
    MEMORY_ID="$(jq -r '.memories[0].id // .id // empty' < "$file" 2>/dev/null || true)"
    local added noop mem_len
    added="$(jq -r '.summary.added // 0' < "$file" 2>/dev/null || echo 0)"
    noop="$(jq -r '.summary.noop // 0' < "$file" 2>/dev/null || echo 0)"
    mem_len="$(jq -r '.memories | length' < "$file" 2>/dev/null || echo 0)"
    if [[ -n "$MEMORY_ID" && "$MEMORY_ID" != "null" ]]; then
      return 0
    fi
    if [[ "$mem_len" == "0" && ( "$added" == "0" || "$noop" != "0" ) ]]; then
      return 2
    fi
    return 1
  fi

  [[ -n "$py" ]] || return 1
  local parsed
  parsed="$("$py" - "$file" <<'PY' 2>/dev/null || true
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as f:
    data = json.load(f)
if not isinstance(data, dict):
    print("FAIL")
    raise SystemExit(0)
memories = data.get("memories") if isinstance(data.get("memories"), list) else []
summary = data.get("summary") if isinstance(data.get("summary"), dict) else {}
added = int(summary.get("added") or 0)
noop = int(summary.get("noop") or 0)
mid = None
if memories and isinstance(memories[0], dict):
    mid = memories[0].get("id")
if not mid:
    mid = data.get("id")
if mid:
    print(f"ID:{mid}")
elif len(memories) == 0 and (added == 0 or noop > 0):
    print("NOOP")
else:
    print("FAIL")
PY
)"
  case "$parsed" in
    ID:*)
      MEMORY_ID="${parsed#ID:}"
      return 0
      ;;
    NOOP)
      return 2
      ;;
    *)
      return 1
      ;;
  esac
}

# Never echo API_KEY / Authorization values.
check_get() {
  local path="$1"
  local url="${BASE_URL}${path}"
  local code
  local body
  body="$(mktemp)"
  code="$(curl -sS -o "$body" -w '%{http_code}' "$url" || true)"
  if [[ "$code" != 2* ]]; then
    echo "error: GET $path returned HTTP $code" >&2
    head -c 500 "$body" >&2 || true
    echo >&2
    rm -f "$body"
    exit 1
  fi
  echo "ok: GET $path -> $code"
  if command -v jq >/dev/null 2>&1; then
    jq -e . >/dev/null 2>&1 < "$body" \
      || echo "warning: response was not JSON for $path" >&2
  fi
  rm -f "$body"
}

auth_curl() {
  # Args: method path [json_body]
  local method="$1"
  local path="$2"
  local json_body="${3:-}"
  local url="${BASE_URL}${path}"
  local body code
  body="$(mktemp)"

  if [[ -n "$json_body" ]]; then
    code="$(curl -sS -o "$body" -w '%{http_code}' \
      -X "$method" \
      -H "Authorization: Bearer ${API_KEY}" \
      -H "X-Organization-ID: ${ORG_ID}" \
      -H "X-Memcore-Test-Source: smoke-test" \
      -H "Content-Type: application/json" \
      -d "$json_body" \
      "$url" || true)"
  else
    code="$(curl -sS -o "$body" -w '%{http_code}' \
      -X "$method" \
      -H "Authorization: Bearer ${API_KEY}" \
      -H "X-Organization-ID: ${ORG_ID}" \
      -H "X-Memcore-Test-Source: smoke-test" \
      "$url" || true)"
  fi

  if [[ "$code" != 2* ]]; then
    echo "error: ${method} ${path} returned HTTP ${code}" >&2
    head -c 500 "$body" >&2 || true
    echo >&2
    rm -f "$body"
    exit 1
  fi

  echo "ok: ${method} ${path} -> ${code}"
  # shellcheck disable=SC2034
  AUTH_CURL_BODY="$body"
  AUTH_CURL_CODE="$code"
}

echo "smoke_test: base=$BASE_URL authenticated=$AUTHENTICATED"

check_get /health
check_get /ready
check_get /api/v1/version

if [[ "$AUTHENTICATED" -eq 1 ]]; then
  if [[ -z "$API_KEY" ]]; then
    fail "MEMCORE_SMOKE_TEST_API_KEY is required with --authenticated"
  fi
  require_json_tool_for_cleanup

  echo "ok: using smoke-test user_id=${USER_ID} org_id=${ORG_ID} (API key not printed)"

  # Create a small synthetic memory for the dedicated smoke-test user only.
  CREATE_BODY=$(cat <<EOF
{"user_id":"${USER_ID}","messages":[{"role":"user","content":"Smoke test memory: user likes green tea."}],"metadata":{"source":"smoke_test"}}
EOF
)

  AUTH_CURL_BODY=""
  auth_curl POST /api/v1/memories "$CREATE_BODY"
  create_body_file="$AUTH_CURL_BODY"

  MEMORY_ID=""
  parse_rc=0
  parse_created_memory_response "$create_body_file" || parse_rc=$?
  rm -f "$create_body_file"
  if [[ "$parse_rc" -eq 1 ]]; then
    fail "could not parse created memory response for cleanup (unexpected response shape)"
  fi
  if [[ "$parse_rc" -eq 2 ]]; then
    fail "create memory returned success but no memory id (noop/empty facts); cannot validate LLM write path"
  fi

  SEARCH_BODY=$(cat <<EOF
{"user_id":"${USER_ID}","query":"green tea"}
EOF
)
  auth_curl POST /api/v1/memories/search "$SEARCH_BODY"
  rm -f "$AUTH_CURL_BODY"

  CONTEXT_BODY=$(cat <<EOF
{"user_id":"${USER_ID}","query":"green tea","max_memories":5}
EOF
)
  auth_curl POST /api/v1/context "$CONTEXT_BODY"
  rm -f "$AUTH_CURL_BODY"

  auth_curl GET "/api/v1/users/${USER_ID}/memories?limit=20"
  rm -f "$AUTH_CURL_BODY"

  auth_curl DELETE "/api/v1/users/${USER_ID}/memories/${MEMORY_ID}"
  rm -f "$AUTH_CURL_BODY"
  echo "ok: cleaned up smoke-test memory id=${MEMORY_ID}"

  case "$(echo "$EXPECT_REAL_PROVIDER" | tr '[:upper:]' '[:lower:]')" in
    true|1|yes)
      echo "note: MEMCORE_SMOKE_TEST_EXPECT_REAL_PROVIDER=true — ensure API was started with real providers enabled and check admin guardrails/usage separately (keys not printed)"
      ;;
  esac
elif [[ -n "$API_KEY" ]]; then
  # Backward-compatible optional read-only admin probe when key is set without --authenticated.
  url="${BASE_URL}/api/v1/admin/org/summary"
  body="$(mktemp)"
  code="$(curl -sS -o "$body" -w '%{http_code}' \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "X-Organization-ID: ${ORG_ID}" \
    -H "X-Memcore-Test-Source: smoke-test" \
    "$url" || true)"
  if [[ "$code" != 2* ]]; then
    echo "error: authenticated GET /api/v1/admin/org/summary returned HTTP $code" >&2
    head -c 500 "$body" >&2 || true
    echo >&2
    rm -f "$body"
    exit 1
  fi
  echo "ok: authenticated GET /api/v1/admin/org/summary -> $code"
  echo "note: pass --authenticated for create/search/context/delete smoke flow"
  rm -f "$body"
else
  echo "skip: authenticated checks (set MEMCORE_SMOKE_TEST_API_KEY and optionally --authenticated)"
fi

echo "smoke_test: passed against $BASE_URL"
