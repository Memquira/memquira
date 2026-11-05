#!/usr/bin/env bash
# Validate example clients/scripts without a live memcore server.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

fail=0

echo "==> bash -n curl scripts"
for script in examples/curl/*.sh; do
  if ! bash -n "${script}"; then
    echo "FAIL: bash -n ${script}" >&2
    fail=1
  fi
done

if [[ -f scripts/examples/validate_examples.sh ]]; then
  bash -n scripts/examples/validate_examples.sh
fi
if [[ -f scripts/docs/generate_openapi.sh ]]; then
  bash -n scripts/docs/generate_openapi.sh
fi

echo "==> python syntax"
if command -v python >/dev/null 2>&1; then
  PY=python
elif command -v python3 >/dev/null 2>&1; then
  PY=python3
else
  PY=""
fi

if [[ -n "${PY}" ]]; then
  if ! "${PY}" -m py_compile examples/python/memcore_client.py examples/python/full_flow.py; then
    echo "FAIL: python syntax" >&2
    fail=1
  fi
else
  echo "skip: python not installed"
fi

echo "==> node syntax"
if command -v node >/dev/null 2>&1; then
  if ! node --check examples/node/memcore-client.js; then
    echo "FAIL: node --check memcore-client.js" >&2
    fail=1
  fi
  if ! node --check examples/node/full-flow.js; then
    echo "FAIL: node --check full-flow.js" >&2
    fail=1
  fi
else
  echo "skip: node not installed"
fi

echo "==> secret / unsafe pattern scan (heuristic)"
# Avoid matching documentation placeholders like REPLACE_WITH_API_KEY.
scan_hit=0
if command -v rg >/dev/null 2>&1; then
  if rg -n --glob '!**/README.md' \
    -e 'sk-live-[A-Za-z0-9]+' \
    -e 'sk_test_[A-Za-z0-9]+' \
    -e 'postgres://[^[:space:]]+:[^[:space:]]+@' \
    -e 'redis://[^[:space:]]+:[^[:space:]]+@' \
    -e 'Bearer memcore_[A-Za-z0-9_]+' \
    -e 'echo .*MEMCORE_API_KEY' \
    -e 'console\.log\(.*apiKey' \
    -e 'print\(.*api_key' \
    examples 2>/dev/null; then
    scan_hit=1
  fi
elif [[ -n "${PY}" ]]; then
  if "${PY}" - <<'PY'
import pathlib, re, sys
roots = [pathlib.Path("examples")]
patterns = [
    re.compile(r"sk-live-[A-Za-z0-9]+"),
    re.compile(r"sk_test_[A-Za-z0-9]+"),
    re.compile(r"postgres://[^\s:]+:[^\s]+@"),
    re.compile(r"redis://[^\s:]+:[^\s]+@"),
    re.compile(r"Bearer memcore_[A-Za-z0-9_]+"),
    re.compile(r"echo .*MEMCORE_API_KEY"),
    re.compile(r"console\.log\(.*apiKey"),
    re.compile(r"print\(.*api_key"),
]
hits = []
for root in roots:
    if not root.exists():
        continue
    for path in root.rglob("*"):
        if not path.is_file() or path.name == "README.md":
            continue
        if path.suffix.lower() not in {".sh", ".py", ".js", ".json", ".txt", ".md", ""}:
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for pat in patterns:
            if pat.search(text):
                hits.append(f"{path}:{pat.pattern}")
if hits:
    print("\n".join(hits))
    sys.exit(1)
sys.exit(0)
PY
  then
    :
  else
    scan_hit=1
  fi
else
  echo "skip: neither rg nor python available for secret scan"
fi

if [[ "${scan_hit}" -eq 1 ]]; then
  echo "FAIL: potential secret or key-logging pattern in examples" >&2
  fail=1
else
  if command -v rg >/dev/null 2>&1 || [[ -n "${PY}" ]]; then
    echo "ok: no obvious secret patterns"
  fi
fi

if [[ "${fail}" -ne 0 ]]; then
  echo "example validation failed" >&2
  exit 1
fi

echo "example validation passed"
