# curl examples

Shell scripts for common Memquira flows. No API key is printed.

## Requirements

- `bash`, `curl`, and preferably `jq` (for `full_flow.sh` ID extraction)

## Environment

```bash
export MEMCORE_BASE_URL="http://localhost:8080"
export MEMCORE_API_KEY="REPLACE_WITH_API_KEY"
export MEMCORE_ORG_ID="org_demo"
export MEMCORE_USER_ID="user_demo"   # optional; defaults to user_demo
```

## Scripts

| Script | Purpose |
|--------|---------|
| `create_memory.sh` | Create a synthetic memory |
| `search_memory.sh` | Search memories |
| `context.sh` | Build context |
| `delete_memory.sh` | Soft-delete one memory (`MEMORY_ID` required) |
| `full_flow.sh` | Create → search → context → delete |

```bash
chmod +x examples/curl/*.sh
./examples/curl/full_flow.sh
```

## Safety

- Scripts exit if required env vars are missing.
- Synthetic content only.
- Do not `echo` `$MEMCORE_API_KEY`.
