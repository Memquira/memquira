"""End to end against a hosted Memquira developer API (developer tokens), over stdio.

Skipped unless MEMQUIRA_E2E_HOSTED_URL and MEMQUIRA_E2E_HOSTED_TOKEN are set. Run it only against a local or staging
deployment with mock models: it stores and deletes memories of the token's account.

Optional:
- MEMQUIRA_E2E_HOSTED_READ_TOKEN: a token of the same account with only the read scope;
- MEMQUIRA_E2E_HOSTED_OTHER_TOKEN: a read-write token of a second account (isolation);
- MEMQUIRA_E2E_HOSTED_REVOKED_TOKEN: a token that has been revoked.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import uuid
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Any

import pytest
from mcp import Client
from mcp.client.stdio import StdioServerParameters

URL = os.environ.get("MEMQUIRA_E2E_HOSTED_URL")
TOKEN = os.environ.get("MEMQUIRA_E2E_HOSTED_TOKEN")
READ_TOKEN = os.environ.get("MEMQUIRA_E2E_HOSTED_READ_TOKEN")
OTHER_TOKEN = os.environ.get("MEMQUIRA_E2E_HOSTED_OTHER_TOKEN")
REVOKED_TOKEN = os.environ.get("MEMQUIRA_E2E_HOSTED_REVOKED_TOKEN")

pytestmark = [
    pytest.mark.anyio,
    pytest.mark.e2e,
    pytest.mark.skipif(not (URL and TOKEN), reason="MEMQUIRA_E2E_HOSTED_* not set"),
]
RUN = uuid.uuid4().hex[:8]


def _env(token: str, **extra: str) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("MEMQUIRA_")}
    env.update(MEMQUIRA_URL=str(URL), MEMQUIRA_API_KEY=token)
    env.update(extra)
    return env


@asynccontextmanager
async def hosted(token: str) -> AsyncIterator[Client]:
    params = StdioServerParameters(command=sys.executable, args=["-m", "memquira_mcp"], env=_env(token))
    async with Client(params) as client:
        yield client


async def call(client: Client, tool: str, args: dict[str, Any]) -> tuple[bool, dict[str, Any]]:
    result = await client.call_tool(tool, args)
    return result.is_error, json.loads(result.content[0].text)


def test_hosted_mode_refuses_a_local_identity() -> None:
    for name in ("MEMQUIRA_USER_ID", "MEMQUIRA_ORG_ID"):
        proc = subprocess.run(
            [sys.executable, "-m", "memquira_mcp", "check"],
            capture_output=True,
            text=True,
            env=_env(str(TOKEN), **{name: "someone-else"}),
            timeout=60,
        )
        assert proc.returncode == 2 and f"{name} is not used with a hosted developer token" in proc.stderr
        assert str(TOKEN) not in proc.stdout + proc.stderr


def test_check_command() -> None:
    proc = subprocess.run(
        [sys.executable, "-m", "memquira_mcp", "check"],
        capture_output=True,
        text=True,
        env=_env(str(TOKEN)),
        timeout=60,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "hosted" in proc.stdout and "credentials: accepted" in proc.stdout and str(TOKEN) not in proc.stdout


async def test_lifecycle_through_the_hosted_api() -> None:
    fact = f"Hosted project {RUN} uses PostgreSQL, Rust and Axum; integration tests over mocks."
    async with hosted(str(TOKEN)) as client:
        err, stored = await call(client, "remember", {"content": fact, "session_id": f"e2e-{RUN}"})
        assert not err, stored
        assert stored["stored"] >= 1
        ids = [m["id"] for m in stored["memories"]]
        err, again = await call(
            client, "remember", {"content": fact, "session_id": f"e2e-{RUN}", "request_id": stored["request_id"]}
        )
        assert not err and [m["id"] for m in again["memories"]] == ids  # replayed, not stored twice
    async with hosted(str(TOKEN)) as client:
        err, found = await call(client, "search_memory", {"query": f"Hosted project {RUN} PostgreSQL Axum", "limit": 5})
        assert not err and any(r["id"] in ids for r in found["results"])
        err, ctx = await call(
            client, "get_context", {"query": f"What does hosted project {RUN} use?", "token_budget": 300}
        )
        assert not err and "PostgreSQL" in ctx["context"] and ctx["token_count"] <= 300
        err, listed = await call(client, "list_memories", {"limit": 50})
        assert not err and set(ids) <= {m["id"] for m in listed["memories"]}
        for mid in ids:
            err, deleted = await call(client, "delete_memory", {"memory_id": mid})
            assert not err and deleted["deleted"]
        err, listed = await call(client, "list_memories", {"limit": 50})
        assert not set(ids) & {m["id"] for m in listed["memories"]}


async def test_identity_injection_is_refused() -> None:
    async with hosted(str(TOKEN)) as client:
        for tool, args in [
            ("search_memory", {"query": "x", "user_id": "u_someone"}),
            ("list_memories", {"user_id": "me"}),
            ("remember", {"content": "x", "org_id": "org"}),
        ]:
            err, out = await call(client, tool, args)
            assert err and out["error"]["code"] == "INVALID_ARGUMENTS"


@pytest.mark.skipif(not READ_TOKEN, reason="MEMQUIRA_E2E_HOSTED_READ_TOKEN not set")
async def test_read_only_token() -> None:
    async with hosted(str(READ_TOKEN)) as client:
        err, out = await call(client, "remember", {"content": f"Must not be stored {RUN}."})
        assert err and out["error"]["status"] == 403 and out["error"]["code"] == "INSUFFICIENT_SCOPE"
        assert out["error"]["message"].startswith("Nothing was stored")
        err, out = await call(client, "delete_memory", {"memory_id": str(uuid.uuid4())})
        assert err and out["error"]["status"] == 403
        err, _ = await call(client, "list_memories", {})
        assert not err


@pytest.mark.skipif(not OTHER_TOKEN, reason="MEMQUIRA_E2E_HOSTED_OTHER_TOKEN not set")
async def test_accounts_are_isolated() -> None:
    async with hosted(str(TOKEN)) as a:
        _, stored = await call(a, "remember", {"content": f"Account A's codename is Plover-{RUN}."})
    mine = stored["memories"][0]["id"]
    try:
        async with hosted(str(OTHER_TOKEN)) as b:
            _, found = await call(b, "search_memory", {"query": f"codename Plover-{RUN}"})
            _, listed = await call(b, "list_memories", {"limit": 50})
            err, deleted = await call(b, "delete_memory", {"memory_id": mine})
        assert found["count"] == 0 and mine not in {m["id"] for m in listed["memories"]}
        assert err and deleted["error"]["status"] == 404
    finally:
        async with hosted(str(TOKEN)) as a:
            await call(a, "delete_memory", {"memory_id": mine})


@pytest.mark.skipif(not REVOKED_TOKEN, reason="MEMQUIRA_E2E_HOSTED_REVOKED_TOKEN not set")
async def test_revoked_token() -> None:
    async with hosted(str(REVOKED_TOKEN)) as client:
        err, out = await call(client, "list_memories", {})
    assert err and out["error"]["status"] == 401 and "rejected" in out["error"]["message"]
