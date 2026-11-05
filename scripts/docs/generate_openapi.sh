#!/usr/bin/env bash
# Generate OpenAPI JSON without starting the HTTP server.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${1:-${ROOT}/openapi/memcore.openapi.json}"

cd "${ROOT}"
cargo run -q -p memcore-api --bin export_openapi -- "${OUT}"

if command -v python >/dev/null 2>&1; then
  python -m json.tool "${OUT}" >/dev/null
  echo "validated JSON: ${OUT}"
elif command -v python3 >/dev/null 2>&1; then
  python3 -m json.tool "${OUT}" >/dev/null
  echo "validated JSON: ${OUT}"
else
  echo "warning: python not found; skipped json.tool validation" >&2
fi
