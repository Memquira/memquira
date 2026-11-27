"""A fake Memquira engine (in memory, behind httpx.MockTransport) and an in-process MCP client."""

from __future__ import annotations

import json
import uuid
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager
from dataclasses import dataclass, field
from typing import Any

import httpx
import pytest
from mcp import Client

from memquira_mcp.config import Settings
from memquira_mcp.server import build_server, make_http_client

API_KEY = "mc_live_test_secret_key_0123456789"
ORG = "org_test"
USER = "alice"


@pytest.fixture
def anyio_backend() -> str:
    return "asyncio"


def settings(**overrides: Any) -> Settings:
    values: dict[str, Any] = {
        "mode": "self-hosted",
        "base_url": "http://127.0.0.1:8080",
        "api_key": API_KEY,
        "org_id": ORG,
        "user_id": USER,
        "timeout": 5.0,
        "read_only": False,
    }
    values.update(overrides)
    return Settings(**values)


@dataclass
class FakeEngine:
    """Enough of the engine API for the five tools. Records every request."""

    requests: list[httpx.Request] = field(default_factory=list)
    memories: dict[str, dict[str, Any]] = field(default_factory=dict)
    seen_message_ids: dict[str, str] = field(default_factory=dict)
    replies: dict[str, dict[str, Any]] = field(default_factory=dict)
    # Hooks: return a Response to override the default behaviour of a request.
    override: Callable[[httpx.Request], Awaitable[httpx.Response | None]] | None = None
    add_summary: dict[str, Any] | None = None

    def bodies(self, path_suffix: str) -> list[dict[str, Any]]:
        return [json.loads(r.content) for r in self.requests if r.url.path.endswith(path_suffix) and r.content]

    def store(self, content: str, memory_type: str = "Preference", user: str = USER) -> str:
        mid = str(uuid.uuid4())
        self.memories[mid] = {"id": mid, "content": content, "memory_type": memory_type, "user": user}
        return mid

    async def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        if self.override is not None:
            forced = await self.override(request)
            if forced is not None:
                return forced
        path = request.url.path
        if path == "/api/v1/version":
            return httpx.Response(200, json={"version": {"package_version": "0.3.1"}})
        if request.headers.get("authorization") != f"Bearer {API_KEY}":
            return _error(401, "UNAUTHORIZED", "invalid api key")
        if request.headers.get("x-organization-id") != ORG:
            return _error(403, "FORBIDDEN", "organization mismatch")
        body = json.loads(request.content) if request.content else {}
        if path == "/api/v1/memories" and request.method == "POST":
            return self._add(body)
        if path == "/api/v1/memories/search":
            return self._search(body)
        if path == "/api/v1/memories/context":
            return self._context(body)
        parts = path.split("/")
        if len(parts) >= 6 and parts[3] == "users" and parts[5] == "memories":
            user = parts[4]
            if request.method == "GET" and len(parts) == 6:
                return self._list(user, request.url.params)
            if request.method == "DELETE" and len(parts) == 7:
                mem = self.memories.get(parts[6])
                if mem is None or mem["user"] != user:
                    return _error(404, "NOT_FOUND", "memory not found")
                del self.memories[parts[6]]
                return httpx.Response(200, json={"status": "success", "deleted": True})
        return _error(404, "NOT_FOUND", "no route")

    def _add(self, body: dict[str, Any]) -> httpx.Response:
        msg = body["messages"][0]
        mid = msg.get("message_id")
        # Like the engine: the same id with a different message (content or message time) is refused.
        occurrence = json.dumps([msg["content"], msg.get("timestamp")])
        if mid in self.seen_message_ids and self.seen_message_ids[mid] != occurrence:
            return _error(409, "MESSAGE_ID_CONFLICT", "message_id already names a different message")
        if mid in self.replies:
            # Like the engine: a resent message gets the first call's result, and nothing is stored again.
            return httpx.Response(200, json=self.replies[mid])
        self.seen_message_ids[mid] = occurrence
        if self.add_summary is not None:
            return httpx.Response(200, json={"status": "success", "memories": [], "summary": self.add_summary})
        new_id = self.store(msg["content"], user=body["user_id"])
        memory = {
            "id": new_id,
            "content": msg["content"],
            "memory_type": "Preference",
            "confidence": 0.9,
            "importance": 0.8,
        }
        reply = {"status": "success", "memories": [memory], "summary": _summary(extracted=1, added=1, stored=1)}
        self.replies[mid] = reply
        return httpx.Response(200, json=reply)

    def _search(self, body: dict[str, Any]) -> httpx.Response:
        words = body["query"].lower().split()
        hits = [
            {"fact_id": m["id"], "content": m["content"], "memory_type": m["memory_type"], "score": 0.5, "metadata": {}}
            for m in self.memories.values()
            if m["user"] == body["user_id"] and any(w in m["content"].lower() for w in words)
        ]
        return httpx.Response(200, json={"status": "success", "results": hits[: body.get("limit", 10)]})

    def _context(self, body: dict[str, Any]) -> httpx.Response:
        lines = [m["content"] for m in self.memories.values() if m["user"] == body["user_id"]]
        return httpx.Response(
            200,
            json={
                "status": "success",
                "text": "\n".join(lines),
                "token_count": 10 * len(lines),
                "token_budget": body.get("token_budget", 1500),
                "selected_count": len(lines),
                "candidates_considered": len(lines),
                "budget_bound": False,
            },
        )

    def _list(self, user: str, params: httpx.QueryParams) -> httpx.Response:
        mine = [m for m in self.memories.values() if m["user"] == user]
        if params.get("q"):
            mine = [m for m in mine if params["q"].lower() in m["content"].lower()]
        if params.get("memory_type"):
            mine = [m for m in mine if m["memory_type"] == params["memory_type"]]
        start = int(params.get("cursor") or 0)
        limit = int(params.get("limit") or 20)
        page = mine[start : start + limit]
        next_cursor = str(start + limit) if start + limit < len(mine) else None
        items = [
            {
                **{k: m[k] for k in ("id", "content", "memory_type")},
                "importance": 0.5,
                "confidence": 0.9,
                "metadata": {},
            }
            for m in page
        ]
        return httpx.Response(200, json={"status": "success", "memories": items, "next_cursor": next_cursor})


def _summary(**counts: int) -> dict[str, Any]:
    base = {k: 0 for k in ("extracted", "stored", "added", "updated", "deleted", "noop")}
    base.update(counts)
    base["discarded"] = {
        "total": base["noop"],
        "low_importance": 0,
        "duplicate_content": 0,
        "duplicate_embedding": 0,
        "no_op_decision": 0,
        "invalid_classification": 0,
    }
    base["skipped"] = {"total": 0, "unknown_memory_type": 0, "invalid_candidate": 0}
    return base


def _error(status: int, code: str, message: str, headers: dict[str, str] | None = None) -> httpx.Response:
    return httpx.Response(
        status, json={"error": {"code": code, "message": message, "request_id": "req-1"}}, headers=headers or {}
    )


error_response = _error
summary = _summary


@asynccontextmanager
async def connect(engine: FakeEngine, **setting_overrides: Any) -> AsyncIterator[Client]:
    s = settings(**setting_overrides)
    http = make_http_client(s, transport=httpx.MockTransport(engine))
    server = build_server(s, http_client=http)
    async with Client(server) as client:
        yield client
    await http.aclose()


def payload(result: Any) -> dict[str, Any]:
    """The JSON text of a tool result (success or error)."""
    assert len(result.content) == 1
    return json.loads(result.content[0].text)
