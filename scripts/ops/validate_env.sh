#!/usr/bin/env bash
# Heuristic env-file validation for memquira (does not print secret values).
#
# Usage:
#   ./scripts/ops/validate_env.sh .env.production
#   ./scripts/ops/validate_env.sh .env.staging staging
#   ./scripts/ops/validate_env.sh .env.staging staging --live
#   ./scripts/ops/validate_env.sh .env.local local
#
# Modes: local | staging | production (optional second arg; inferred from MEMQUIRA_ENV / filename)
# --live: also check smoke/metrics/base URL readiness (file or process environment)
# Exit codes: 0 = ok (warnings allowed), 1 = usage/file error, 2 = hard failures

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <env-file> [local|staging|production] [--live]" >&2
  exit 1
fi

ENV_FILE="$1"
shift
MODE_ARG=""
LIVE=0
for arg in "$@"; do
  case "$arg" in
    --live) LIVE=1 ;;
    local|staging|production|stage|prod) MODE_ARG="$arg" ;;
    *)
      echo "error: unexpected argument: $arg" >&2
      echo "usage: $0 <env-file> [local|staging|production] [--live]" >&2
      exit 1
      ;;
  esac
done

if [[ ! -f "$ENV_FILE" ]]; then
  echo "error: file not found: $ENV_FILE" >&2
  exit 1
fi

warnings=0
failures=0

warn() {
  echo "warning: $*" >&2
  warnings=$((warnings + 1))
}

fail() {
  echo "error: $*" >&2
  failures=$((failures + 1))
}

# Read KEY=VALUE lines (no export, no multiline). Does not echo values.
get_var() {
  local key="$1"
  local line
  line="$(grep -E "^[[:space:]]*${key}=" "$ENV_FILE" | tail -n1 || true)"
  if [[ -z "$line" && "$key" == MEMQUIRA_* ]]; then
    # Pre-0.3 name, still accepted by the server.
    line="$(grep -E "^[[:space:]]*MEMCORE_${key#MEMQUIRA_}=" "$ENV_FILE" | tail -n1 || true)"
  fi
  if [[ -z "$line" ]]; then
    echo ""
    return
  fi
  echo "${line#*=}" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//;s/^"//;s/"$//;s/^'"'"'//;s/'"'"'$//'
}

# Prefer file value; fall back to process environment. Never prints.
get_var_or_env() {
  local key="$1"
  local from_file
  from_file="$(get_var "$key")"
  if [[ -n "$from_file" ]]; then
    echo "$from_file"
    return
  fi
  local legacy="MEMCORE_${key#MEMQUIRA_}"
  if [[ -z "${!key-}" && "$key" == MEMQUIRA_* ]]; then echo "${!legacy-}"; return; fi
  echo "${!key-}"
}

has_placeholder() {
  local value="$1"
  [[ "$value" == *CHANGE_ME* ]] \
    || [[ "$value" == *your-openai-api-key* ]] \
    || [[ "$value" == *your_openai_api_key* ]] \
    || [[ "$value" == *change_this* ]]
}

# Pre-0.3 MEMCORE_* names: accepted (deprecated); a legacy and a current name with different values is refused.
while IFS= read -r legacy_key; do
  [[ -n "$legacy_key" ]] || continue
  current_key="MEMQUIRA_${legacy_key#MEMCORE_}"
  legacy_line="$(grep -E "^[[:space:]]*${legacy_key}=" "$ENV_FILE" | tail -n1)"
  current_line="$(grep -E "^[[:space:]]*${current_key}=" "$ENV_FILE" | tail -n1 || true)"
  if [[ -n "$current_line" && "${current_line#*=}" != "${legacy_line#*=}" ]]; then
    fail "$legacy_key and $current_key are both set to different values; remove $legacy_key"
  else
    warn "$legacy_key is deprecated; rename it to $current_key"
  fi
done < <(grep -oE '^[[:space:]]*MEMCORE_[A-Z0-9_]+=' "$ENV_FILE" | sed 's/^[[:space:]]*//;s/=$//' | sort -u || true)

is_example_file=0
case "$ENV_FILE" in
  *.example|*.example.*) is_example_file=1 ;;
esac

memquira_env="$(get_var MEMQUIRA_ENV)"
auth_mode="$(get_var MEMQUIRA_AUTH_MODE)"
fact_backend="$(get_var MEMQUIRA_FACT_BACKEND)"
event_backend="$(get_var MEMQUIRA_EVENT_BACKEND)"
vector_backend="$(get_var MEMQUIRA_VECTOR_BACKEND)"
migration_mode="$(get_var MEMQUIRA_DATABASE_MIGRATION_MODE)"
migrations_enabled="$(get_var MEMQUIRA_DATABASE_MIGRATIONS_ENABLED)"
cors_enabled="$(get_var MEMQUIRA_CORS_ENABLED)"
cors_origins="$(get_var MEMQUIRA_CORS_ALLOWED_ORIGINS)"
cors_creds="$(get_var MEMQUIRA_CORS_ALLOW_CREDENTIALS)"
backup_enabled="$(get_var MEMQUIRA_BACKUP_ENABLED)"
backup_dir="$(get_var MEMQUIRA_BACKUP_DIR)"
restore_enabled="$(get_var MEMQUIRA_RESTORE_ENABLED)"
postgres_url="$(get_var MEMQUIRA_POSTGRES_URL)"
# Alias: MEMQUIRA_DATABASE_URL in file
if [[ -z "$postgres_url" ]]; then
  postgres_url="$(get_var MEMQUIRA_DATABASE_URL)"
fi
dev_key="$(get_var MEMQUIRA_DEV_API_KEY)"
pepper="$(get_var MEMQUIRA_API_KEY_PEPPER)"
openai_key="$(get_var OPENAI_API_KEY)"
redis_url="$(get_var MEMQUIRA_REDIS_URL)"
cache_backend="$(get_var MEMQUIRA_CONTEXT_CACHE_BACKEND)"
llm_provider="$(get_var MEMQUIRA_LLM_PROVIDER)"
embed_provider="$(get_var MEMQUIRA_EMBEDDING_PROVIDER)"
postgres_password="$(get_var POSTGRES_PASSWORD)"
metrics_enabled="$(get_var MEMQUIRA_METRICS_ENABLED)"
metrics_require_auth="$(get_var MEMQUIRA_METRICS_REQUIRE_AUTH)"
metrics_path="$(get_var MEMQUIRA_METRICS_PATH)"
qdrant_url="$(get_var MEMQUIRA_QDRANT_URL)"

env_normalized="$(printf '%s' "$memquira_env" | tr '[:upper:]' '[:lower:]')"
mode="$(printf '%s' "$MODE_ARG" | tr '[:upper:]' '[:lower:]')"
case "$mode" in
  stage) mode=staging ;;
  prod) mode=production ;;
esac
if [[ -z "$mode" ]]; then
  case "$env_normalized" in
    staging|stage) mode=staging ;;
    production|prod) mode=production ;;
    *)
      case "$ENV_FILE" in
        *.staging*|*.staging) mode=staging ;;
        *.production*|*.prod*) mode=production ;;
        *.local*) mode=local ;;
        *) mode=local ;;
      esac
      ;;
  esac
fi

case "$mode" in
  local|staging|production) ;;
  *)
    echo "error: unknown mode '$MODE_ARG' (expected local|staging|production)" >&2
    exit 1
    ;;
esac

is_staging=0
is_production=0
case "$mode" in
  staging) is_staging=1 ;;
  production) is_production=1 ;;
esac

# Placeholder scan for known sensitive keys (warn on examples; fail for live staging/prod files).
for key in MEMQUIRA_POSTGRES_URL MEMQUIRA_DATABASE_URL MEMQUIRA_DEV_API_KEY MEMQUIRA_API_KEY_PEPPER \
  MEMQUIRA_REDIS_URL OPENAI_API_KEY POSTGRES_PASSWORD MEMQUIRA_SMOKE_TEST_API_KEY MEMQUIRA_METRICS_API_KEY; do
  val="$(get_var "$key")"
  if [[ -n "$val" ]] && has_placeholder "$val"; then
    if [[ "$is_example_file" -eq 1 ]]; then
      warn "$key still contains a placeholder token (expected in *.example files)"
    elif [[ "$is_staging" -eq 1 || "$is_production" -eq 1 ]]; then
      fail "$key still contains a placeholder token — replace before staging/production use"
    else
      warn "$key still contains a placeholder token"
    fi
  fi
done

if [[ -z "$vector_backend" ]]; then
  warn "MEMQUIRA_VECTOR_BACKEND is unset (code may default to LanceDB); set mock/qdrant/lancedb explicitly"
fi

if [[ "${restore_enabled}" == "true" ]]; then
  if [[ "$is_staging" -eq 1 || "$is_production" -eq 1 ]]; then
    fail "MEMQUIRA_RESTORE_ENABLED=true is unsafe for shared staging/production hosts"
  else
    warn "MEMQUIRA_RESTORE_ENABLED=true — destructive; keep false unless intentionally testing restore tooling"
  fi
fi

if [[ "${cors_enabled}" == "true" && "${cors_creds}" == "true" ]]; then
  if [[ "$cors_origins" == "*" || "$cors_origins" == *",*"* || "$cors_origins" == *"*,"* ]]; then
    fail "CORS credentials cannot be combined with wildcard origin *"
  fi
fi

if [[ "${metrics_enabled}" == "true" && "${metrics_require_auth}" == "false" ]]; then
  if [[ "$is_staging" -eq 1 || "$is_production" -eq 1 ]]; then
    fail "MEMQUIRA_METRICS_ENABLED=true with MEMQUIRA_METRICS_REQUIRE_AUTH=false — require auth or private-network-only exception documented elsewhere"
  else
    warn "metrics enabled without auth — use only on private loopback scrapes"
  fi
fi

if [[ "$is_staging" -eq 1 || "$is_production" -eq 1 ]]; then
  if [[ "${migrations_enabled}" == "false" ]]; then
    fail "MEMQUIRA_DATABASE_MIGRATIONS_ENABLED=false is not allowed for ${mode} validation"
  fi
  if [[ "${migration_mode:-auto}" == "disabled" ]]; then
    fail "MEMQUIRA_DATABASE_MIGRATION_MODE=disabled is not allowed for ${mode} validation"
  fi

  if [[ "${fact_backend}" == "sqlite" || "${event_backend}" == "sqlite" ]]; then
    fail "sqlite backends are not allowed for controlled ${mode} shape (use postgres)"
  fi

  if [[ "${fact_backend}" == "postgres" || "${event_backend}" == "postgres" ]]; then
    if [[ -z "$postgres_url" ]]; then
      fail "MEMQUIRA_POSTGRES_URL required when using postgres backends"
    fi
  else
    if [[ "$is_staging" -eq 1 ]]; then
      warn "controlled staging normally uses MEMQUIRA_FACT_BACKEND=postgres"
    fi
  fi

  if [[ "${vector_backend}" == "qdrant" ]]; then
    if [[ -z "$qdrant_url" ]]; then
      fail "MEMQUIRA_QDRANT_URL required when MEMQUIRA_VECTOR_BACKEND=qdrant"
    fi
  elif [[ "$is_staging" -eq 1 && "${vector_backend}" != "qdrant" ]]; then
    warn "controlled staging example uses qdrant; current MEMQUIRA_VECTOR_BACKEND=${vector_backend:-unset}"
  fi

  if [[ "${auth_mode:-dev}" == "database" && -z "$pepper" ]]; then
    fail "MEMQUIRA_API_KEY_PEPPER required when MEMQUIRA_AUTH_MODE=database"
  fi

  if [[ "${auth_mode:-dev}" == "dev" ]]; then
    if [[ "$is_production" -eq 1 ]]; then
      warn "MEMQUIRA_AUTH_MODE=dev in production-shaped env — prefer database auth"
    else
      warn "MEMQUIRA_AUTH_MODE=dev in staging — prefer database auth for shared staging"
    fi
  fi

  if [[ -n "$metrics_path" && "$metrics_path" != /* ]]; then
    fail "MEMQUIRA_METRICS_PATH must start with /"
  fi
fi

if [[ "${backup_enabled}" == "true" ]]; then
  dir="${backup_dir:-./backups}"
  if [[ -z "$backup_dir" ]]; then
    warn "MEMQUIRA_BACKUP_ENABLED=true but MEMQUIRA_BACKUP_DIR is empty"
  elif [[ ! -d "$dir" && "$is_example_file" -eq 0 ]]; then
    warn "MEMQUIRA_BACKUP_ENABLED=true but backup directory does not exist yet: (path omitted)"
  fi
fi

# Context cache: memquira refuses redis (always) and memory (outside local single-process
# development) at startup; a deployment env must use disabled.
if [[ -n "${cache_backend}" && "${cache_backend}" != "disabled" ]]; then
  fail "MEMQUIRA_CONTEXT_CACHE_BACKEND must be disabled (memquira refuses ${cache_backend} at startup)"
fi

if [[ "${llm_provider}" == "openai" || "${embed_provider}" == "openai" ]]; then
  if [[ -z "$openai_key" ]]; then
    fail "OPENAI_API_KEY required when OpenAI providers are selected"
  fi
fi

# Redacted readiness summary for staging/production (set/missing only).
if [[ "$is_staging" -eq 1 || "$is_production" -eq 1 ]]; then
  echo "validate_env: readiness (redacted):"
  for key in MEMQUIRA_ENV MEMQUIRA_FACT_BACKEND MEMQUIRA_POSTGRES_URL POSTGRES_PASSWORD \
    MEMQUIRA_VECTOR_BACKEND MEMQUIRA_QDRANT_URL MEMQUIRA_AUTH_MODE MEMQUIRA_API_KEY_PEPPER \
    MEMQUIRA_DEV_API_KEY MEMQUIRA_LLM_PROVIDER OPENAI_API_KEY MEMQUIRA_REDIS_URL \
    MEMQUIRA_METRICS_ENABLED MEMQUIRA_METRICS_REQUIRE_AUTH MEMQUIRA_RESTORE_ENABLED; do
    val="$(get_var "$key")"
    if [[ -n "$val" ]]; then
      if has_placeholder "$val"; then
        echo "  $key=placeholder"
      else
        echo "  $key=set"
      fi
    else
      echo "  $key=missing"
    fi
  done
fi

# --live: smoke / metrics / base URL readiness (file or process env). Never print values.
if [[ "$LIVE" -eq 1 ]]; then
  if [[ "$is_staging" -ne 1 && "$is_production" -ne 1 ]]; then
    warn "--live is intended for staging/production; continuing with live checks anyway"
  fi
  smoke_key="$(get_var_or_env MEMQUIRA_SMOKE_TEST_API_KEY)"
  smoke_org="$(get_var_or_env MEMQUIRA_SMOKE_TEST_ORG_ID)"
  smoke_user="$(get_var_or_env MEMQUIRA_SMOKE_TEST_USER_ID)"
  metrics_key="$(get_var_or_env MEMQUIRA_METRICS_API_KEY)"
  base_url="$(get_var_or_env MEMQUIRA_STAGING_BASE_URL)"
  # Dev key can serve as smoke/metrics for short-lived personal stacks
  if [[ -z "$smoke_key" ]]; then
    smoke_key="$(get_var_or_env MEMQUIRA_DEV_API_KEY)"
  fi
  if [[ -z "$metrics_key" ]]; then
    metrics_key="$(get_var_or_env MEMQUIRA_SMOKE_TEST_API_KEY)"
    if [[ -z "$metrics_key" ]]; then
      metrics_key="$(get_var_or_env MEMQUIRA_DEV_API_KEY)"
    fi
  fi

  echo "validate_env: live readiness (redacted):"
  if [[ -n "$smoke_key" ]]; then
    if has_placeholder "$smoke_key"; then
      echo "  MEMQUIRA_SMOKE_TEST_API_KEY=placeholder"
      fail "smoke API key still contains a placeholder"
    else
      echo "  MEMQUIRA_SMOKE_TEST_API_KEY=set"
    fi
  else
    echo "  MEMQUIRA_SMOKE_TEST_API_KEY=missing"
    fail "live check requires MEMQUIRA_SMOKE_TEST_API_KEY (file or shell) or MEMQUIRA_DEV_API_KEY"
  fi

  if [[ -n "$smoke_org" ]]; then
    echo "  MEMQUIRA_SMOKE_TEST_ORG_ID=set"
  else
    echo "  MEMQUIRA_SMOKE_TEST_ORG_ID=missing"
    fail "live check requires MEMQUIRA_SMOKE_TEST_ORG_ID"
  fi

  if [[ -n "$smoke_user" ]]; then
    echo "  MEMQUIRA_SMOKE_TEST_USER_ID=set"
  else
    echo "  MEMQUIRA_SMOKE_TEST_USER_ID=missing"
    warn "MEMQUIRA_SMOKE_TEST_USER_ID unset — smoke_test.sh defaults to smoke-test-user"
  fi

  if [[ -n "$base_url" ]]; then
    echo "  MEMQUIRA_STAGING_BASE_URL=set"
  else
    echo "  MEMQUIRA_STAGING_BASE_URL=missing"
    fail "live check requires MEMQUIRA_STAGING_BASE_URL"
  fi

  metrics_on="$(get_var MEMQUIRA_METRICS_ENABLED)"
  metrics_auth="$(get_var MEMQUIRA_METRICS_REQUIRE_AUTH)"
  if [[ "${metrics_on}" == "true" && "${metrics_auth}" != "false" ]]; then
    if [[ -n "$metrics_key" ]]; then
      if has_placeholder "$metrics_key"; then
        echo "  MEMQUIRA_METRICS_API_KEY=placeholder"
        fail "metrics API key still contains a placeholder"
      else
        echo "  MEMQUIRA_METRICS_API_KEY=set"
      fi
    else
      echo "  MEMQUIRA_METRICS_API_KEY=missing"
      fail "live check requires MEMQUIRA_METRICS_API_KEY when metrics auth is required"
    fi
  else
    echo "  MEMQUIRA_METRICS_API_KEY=skipped (metrics disabled or auth not required)"
  fi
fi

# Avoid echoing any secret content.
echo "validate_env: checked $ENV_FILE mode=$mode live=$LIVE (failures=$failures warnings=$warnings)"
if [[ "$is_example_file" -eq 1 ]]; then
  echo "note: example env files are expected to warn on CHANGE_ME placeholders"
fi
if [[ "$failures" -gt 0 ]]; then
  exit 2
fi
exit 0
