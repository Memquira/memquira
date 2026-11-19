# Python example client

Lightweight example client using `requests`. **Not** a published PyPI package.

## Setup

```bash
python -m venv .venv
# Windows: .venv\Scripts\activate
source .venv/bin/activate
pip install -r examples/python/requirements.txt
```

```bash
export MEMQUIRA_BASE_URL="http://localhost:8080"
export MEMQUIRA_API_KEY="REPLACE_WITH_API_KEY"
export MEMQUIRA_ORG_ID="org_demo"
export MEMQUIRA_USER_ID="user_demo"
```

## Run

```bash
python examples/python/full_flow.py
```

## Safety

- Do not log `MEMQUIRA_API_KEY`.
- Synthetic memory text only.
- Timeouts are enabled on every request.
