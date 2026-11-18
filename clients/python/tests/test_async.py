from __future__ import annotations

import asyncio

import httpx
import pytest
from conftest import err, ok
from test_client import ADD_RESPONSE, CONTEXT_RESPONSE, MEMORY, SEARCH_RESPONSE

from memquira import ConflictError, ServiceUnavailableError, TransportError


def run(coro):
    return asyncio.run(coro)


def test_async_round_trip(make_async_client):
    async def scenario():
        client, rec = make_async_client([ok(ADD_RESPONSE), ok(SEARCH_RESPONSE), ok(CONTEXT_RESPONSE)])
        async with client:
            alice = client.for_user("alice")
            added = await alice.add("I am vegetarian.")
            hits = await alice.search("diet", limit=3)
            ctx = await alice.context("dinner", token_budget=500)
        return rec, added, hits, ctx

    rec, added, hits, ctx = run(scenario())
    assert added.summary.added == 1
    assert [h.id for h in hits] == ["f1", "f0"]
    assert ctx.text == "- Alice is vegetarian"
    assert [r.url.path for r in rec.requests] == [
        "/api/v1/memories",
        "/api/v1/memories/search",
        "/api/v1/memories/context",
    ]
    assert all(rec.body(i)["user_id"] == "alice" for i in range(3))


def test_async_iter_memories(make_async_client):
    async def scenario():
        page1 = {"status": "ok", "memories": [MEMORY], "next_cursor": "c2"}
        page2 = {"status": "ok", "memories": [dict(MEMORY, id="f2")]}
        client, rec = make_async_client([ok(page1), ok(page2)])
        ids = [m.id async for m in client.iter_memories("alice")]
        await client.aclose()
        return ids, rec

    ids, rec = run(scenario())
    assert ids == ["f1", "f2"]
    assert rec.requests[1].url.params["cursor"] == "c2"


def test_async_retry_and_errors(make_async_client):
    async def scenario():
        client, rec = make_async_client(
            [
                err(429, "RATE_LIMITED", headers={"Retry-After": "1"}),
                ok(SEARCH_RESPONSE),
                err(409, "MESSAGE_ID_CONFLICT"),
                err(503, "OVERLOADED"),
                httpx.ReadTimeout("slow"),
            ]
        )
        await client.search("alice", "x")
        with pytest.raises(ConflictError):
            await client.add("alice", "x")
        with pytest.raises(ServiceUnavailableError):
            await client.add("alice", [{"role": "user", "content": "no id"}])
        with pytest.raises(TransportError):
            await client.add("alice", [{"role": "user", "content": "no id"}])
        return rec

    rec = run(scenario())
    assert rec.sleeps == [1.0]
    assert len(rec.requests) == 5
