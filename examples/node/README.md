# Node.js example client

Uses built-in `fetch` (Node 18+). **Not** a published npm package.

## Setup

```bash
export MEMCORE_BASE_URL="http://localhost:8080"
export MEMCORE_API_KEY="REPLACE_WITH_API_KEY"
export MEMCORE_ORG_ID="org_demo"
export MEMCORE_USER_ID="user_demo"
```

No install required for the example itself (`package.json` marks this as private).

## Run

```bash
node examples/node/full-flow.js
```

## Safety

- Never log `MEMCORE_API_KEY`.
- Synthetic memory text only.
- Requests use a timeout via `AbortSignal.timeout`.
