"""End-to-end tests against a running Memquira server.

Skipped unless MEMQUIRA_E2E_URL, MEMQUIRA_E2E_API_KEY and MEMQUIRA_E2E_ORG_ID are set. Use a server with mock
models (for example docker/docker-compose.local.yml): every ``add`` runs the server's model. Each test writes to
its own random user and erases it afterwards, which needs a key with the UserDelete scope (dev mode has all).
"""

from __future__ import annotations

import asyncio
import os
import uuid
from collections.abc import Iterator

import pytest

from memquira import AsyncMemquira, BadRequestError, ConflictError, Memquira, Message, NotFoundError

URL = os.environ.get("MEMQUIRA_E2E_URL")
KEY = os.environ.get("MEMQUIRA_E2E_API_KEY")
ORG = os.environ.get("MEMQUIRA_E2E_ORG_ID")

pytestmark = pytest.mark.skipif(
    not (URL and KEY and ORG), reason="set MEMQUIRA_E2E_URL, MEMQUIRA_E2E_API_KEY and MEMQUIRA_E2E_ORG_ID"
)


@pytest.fixture
def client() -> Iterator[Memquira]:
    with Memquira(URL, KEY, ORG, timeout=120) as c:
        yield c


@pytest.fixture
def user_id(client: Memquira) -> Iterator[str]:
    uid = f"e2e-{uuid.uuid4().hex[:12]}"
    yield uid
    client.forget_user(uid)


def test_server_is_ready(client: Memquira) -> None:
    readiness = client.ready()
    assert readiness.ready, readiness.raw
    assert client.version().package_version


def test_store_search_context_list_delete(client: Memquira, user_id: str) -> None:
    user = client.for_user(user_id)
    turn = [
        Message("user", "I am vegetarian and I live in Lisbon."),
        Message("user", "I prefer meetings after 2pm."),
    ]
    added = user.add(turn, session_id="chat-1")
    assert added.status
    assert added.summary.extracted >= 0

    memories = list(user.iter_memories(page_size=1))
    if added.summary.stored:
        assert memories, "stored memories should be listable"

    hits = user.search("vegetarian Lisbon", limit=5)
    assert all(h.content for h in hits)

    context = user.context("Plan a team dinner with this user", token_budget=500, include_items=True)
    assert context.token_count <= context.token_budget
    assert context.items is not None

    if memories:
        target = memories[0].id
        assert user.delete(target) is True
        assert target not in {m.id for m in user.iter_memories()}
        # Deleting an already deleted memory succeeds again, so a resent delete is harmless.
        assert user.delete(target) is True

    with pytest.raises(NotFoundError):
        user.delete(str(uuid.uuid4()))
    with pytest.raises(BadRequestError):
        user.delete("not-a-memory-id")


def test_resending_the_same_messages_is_a_safe_retry(client: Memquira, user_id: str) -> None:
    turn = [Message("user", "My sister Ana works as a nurse in Porto.")]
    first = client.add(user_id, turn, session_id="chat-2")
    before = sorted(m.id for m in client.iter_memories(user_id))

    # The server recognises the message ids and answers with the first call's result instead of storing again.
    again = client.add(user_id, turn, session_id="chat-2")
    assert [m.id for m in again.memories] == [m.id for m in first.memories]
    assert sorted(m.id for m in client.iter_memories(user_id)) == before


def test_reusing_a_message_id_for_other_text_is_rejected(client: Memquira, user_id: str) -> None:
    first = Message("user", "I like green tea.")
    client.add(user_id, [first], session_id="chat-3")
    with pytest.raises(ConflictError) as info:
        client.add(user_id, [Message("user", "I like coffee.", message_id=first.message_id)], session_id="chat-3")
    assert info.value.code == "MESSAGE_ID_CONFLICT"


def test_forget_user_erases_everything(client: Memquira) -> None:
    uid = f"e2e-{uuid.uuid4().hex[:12]}"
    client.add(uid, "I play the cello.", session_id="chat-4")
    assert client.forget_user(uid) is True
    assert list(client.iter_memories(uid)) == []
    assert client.forget_user(uid) is True  # erasing again succeeds, so a resent erase is harmless


def test_async_client(user_id: str) -> None:
    async def scenario() -> None:
        async with AsyncMemquira(URL, KEY, ORG, timeout=120) as aclient:
            user = aclient.for_user(user_id)
            await user.add("I am learning Japanese.", session_id="chat-5")
            hits = await user.search("Japanese")
            assert all(h.content for h in hits)
            context = await user.context("What is this user learning?")
            assert context.token_count <= context.token_budget
            memories = [m async for m in user.iter_memories()]
            assert all(m.id and m.content for m in memories)

    asyncio.run(scenario())
