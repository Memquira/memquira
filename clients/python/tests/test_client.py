from __future__ import annotations

import httpx
import pytest
from conftest import BASE, KEY, ORG, err, ok

import memquira
from memquira import (
    AuthenticationError,
    ConflictError,
    Memquira,
    Message,
    NotFoundError,
    RateLimitError,
    ServiceUnavailableError,
    TransportError,
)
from memquira._base import parse_retry_after

MEMORY = {
    "id": "f1",
    "content": "Alice is vegetarian",
    "memory_type": "Preference",
    "importance": 0.8,
    "confidence": 0.9,
}
ADD_RESPONSE = {
    "status": "ok",
    "memories": [MEMORY],
    "summary": {"extracted": 1, "stored": 1, "added": 1, "updated": 0, "deleted": 0, "noop": 0, "future_field": 3},
}
SEARCH_RESPONSE = {
    "status": "ok",
    "results": [
        {
            "fact_id": "f1",
            "content": "Alice is vegetarian",
            "memory_type": "Preference",
            "score": 0.42,
            "vector_score": 0.91,
            "metadata": {},
        },
        {
            "fact_id": "f0",
            "content": "Alice eats meat",
            "memory_type": "Preference",
            "score": 0.2,
            "metadata": None,
            "lifecycle": {"status": "superseded", "superseded_at": "2026-11-01T00:00:00Z", "superseded_by": "f1"},
        },
    ],
}
CONTEXT_RESPONSE = {
    "status": "ok",
    "text": "- Alice is vegetarian",
    "token_count": 6,
    "token_budget": 500,
    "selected_count": 1,
    "candidates_considered": 3,
    "budget_bound": False,
    "render_mode": "lines",
    "source": "memories",
    "items": [
        {
            "id": "f1",
            "kind": "fact",
            "text": "Alice is vegetarian",
            "line": "- Alice is vegetarian",
            "timestamp": "2026-11-18T00:00:00Z",
            "tokens": 6,
            "vector_score": 0.9,
            "source_message_ids": ["m-1"],
        }
    ],
}


def test_headers_and_add_body(make_client):
    client, rec = make_client([ok(ADD_RESPONSE)])
    result = client.add("alice", "I am vegetarian.", session_id="chat-1", metadata={"source": "test"})

    req = rec.requests[0]
    assert req.method == "POST"
    assert str(req.url) == f"{BASE}/api/v1/memories"
    assert req.headers["Authorization"] == f"Bearer {KEY}"
    assert req.headers["X-Organization-ID"] == ORG
    assert req.headers["User-Agent"] == f"memquira-python/{memquira.__version__}"
    body = rec.body()
    assert body["user_id"] == "alice"
    assert body["session_id"] == "chat-1"
    assert body["metadata"] == {"source": "test"}
    [message] = body["messages"]
    assert message["role"] == "user"
    assert message["content"] == "I am vegetarian."
    assert message["message_id"].startswith("msg_")

    assert result.status == "ok"
    assert result.memories[0].id == "f1"
    assert result.memories[0].memory_type == "Preference"
    assert result.summary.added == 1
    assert result.summary.invalidated == 0
    assert result.summary.raw["future_field"] == 3


def test_add_accepts_message_objects_and_dicts(make_client):
    client, rec = make_client([ok(ADD_RESPONSE)])
    first = Message("user", "hello", name="Alice", timestamp="2026-11-18T10:00:00Z")
    client.add("alice", [first, {"role": "assistant", "content": "hi", "message_id": "a-1"}])
    messages = rec.body()["messages"]
    assert messages[0] == {
        "role": "user",
        "content": "hello",
        "message_id": first.message_id,
        "name": "Alice",
        "timestamp": "2026-11-18T10:00:00Z",
    }
    assert messages[1] == {"role": "assistant", "content": "hi", "message_id": "a-1"}


def test_message_ids_are_unique_and_stable():
    a, b = Message("user", "same"), Message("user", "same")
    assert a.message_id != b.message_id
    assert a.to_dict() == a.to_dict()


@pytest.mark.parametrize(
    "bad",
    [[], "", "   ", [{"role": "user"}], [{"content": "x"}], [42]],
)
def test_add_rejects_invalid_messages(make_client, bad):
    client, rec = make_client([])
    with pytest.raises((ValueError, TypeError)):
        client.add("alice", bad)
    assert rec.requests == []


def test_search(make_client):
    client, rec = make_client([ok(SEARCH_RESPONSE)])
    hits = client.search(
        "alice", "diet", limit=5, memory_types=["Preference"], include_history=True, retrieval_mode="hybrid"
    )
    assert rec.body() == {
        "user_id": "alice",
        "query": "diet",
        "limit": 5,
        "filters": {"memory_type": ["Preference"]},
        "include_history": True,
        "retrieval_mode": "hybrid",
    }
    assert [h.id for h in hits] == ["f1", "f0"]
    assert hits[0].vector_score == 0.91 and hits[0].lifecycle is None
    assert hits[1].vector_score is None
    assert hits[1].lifecycle is not None and hits[1].lifecycle.superseded_by == "f1"


def test_search_sends_only_given_options(make_client):
    client, rec = make_client([ok({"status": "ok", "results": []})])
    assert client.search("alice", "x") == []
    assert rec.body() == {"user_id": "alice", "query": "x"}


def test_context(make_client):
    client, rec = make_client([ok(CONTEXT_RESPONSE)])
    ctx = client.context("alice", "dinner", token_budget=500, include_items=True, exclude_message_ids=["m-0"])
    assert rec.body() == {
        "user_id": "alice",
        "query": "dinner",
        "token_budget": 500,
        "include_items": True,
        "exclude_message_ids": ["m-0"],
    }
    assert ctx.text == "- Alice is vegetarian"
    assert ctx.token_count == 6 and ctx.selected_count == 1
    assert ctx.items is not None and ctx.items[0].source_message_ids == ["m-1"]


def test_list_and_iterate_pages(make_client):
    page1 = {"status": "ok", "memories": [MEMORY], "next_cursor": "c2"}
    page2 = {"status": "ok", "memories": [dict(MEMORY, id="f2")], "next_cursor": None}
    client, rec = make_client([ok(page1), ok(page2)])
    ids = [m.id for m in client.iter_memories("alice", page_size=1, memory_type="Preference")]
    assert ids == ["f1", "f2"]
    assert rec.requests[0].url.params["limit"] == "1"
    assert rec.requests[0].url.params["memory_type"] == "Preference"
    assert "cursor" not in rec.requests[0].url.params
    assert rec.requests[1].url.params["cursor"] == "c2"


def test_path_segments_are_encoded(make_client):
    client, rec = make_client([ok({"status": "ok", "deleted": True})])
    assert client.delete("team/alice smith", "f/1") is True
    assert rec.requests[0].url.raw_path == b"/api/v1/users/team%2Falice%20smith/memories/f%2F1"


def test_forget_user(make_client):
    client, rec = make_client([ok({"status": "ok", "deleted": True})])
    assert client.forget_user("alice") is True
    assert rec.requests[0].method == "DELETE"
    assert rec.requests[0].url.path == "/api/v1/users/alice"


def test_version_and_ready(make_client):
    version = {
        "status": "ok",
        "version": {
            "package_version": "0.2.0",
            "git_sha": "abc",
            "profile": "release",
            "build_timestamp": "t",
            "rustc_version": "1.89.0",
        },
    }
    not_ready = {"status": "not_ready", "draining": True, "quality": {"tier": "reference"}, "checks": {}}
    client, _ = make_client([ok(version), ok(not_ready, status=503)])
    assert client.version().package_version == "0.2.0"
    readiness = client.ready()
    assert readiness.ready is False and readiness.draining is True and readiness.quality_tier == "reference"


def test_error_mapping(make_client):
    client, _ = make_client(
        [err(401, "UNAUTHORIZED", "bad key"), err(409, "MESSAGE_ID_CONFLICT"), err(404, "NOT_FOUND")]
    )
    with pytest.raises(AuthenticationError) as info:
        client.search("alice", "x")
    assert info.value.code == "UNAUTHORIZED"
    assert info.value.request_id == "req-1"
    assert info.value.status_code == 401
    assert "bad key" in str(info.value)
    with pytest.raises(ConflictError):
        client.add("alice", "x")
    with pytest.raises(NotFoundError):
        client.delete("alice", "missing")


def test_non_json_error_body(make_client):
    client, _ = make_client([httpx.Response(502, text="Bad gateway")])
    with pytest.raises(memquira.ServerError) as info:
        client.search("alice", "x")
    assert info.value.code is None and info.value.message == "Bad gateway"


def test_rate_limit_is_retried_after_retry_after(make_client):
    client, rec = make_client([err(429, "RATE_LIMITED", headers={"Retry-After": "2"}), ok(SEARCH_RESPONSE)])
    assert len(client.search("alice", "x")) == 2
    assert rec.sleeps == [2.0]
    assert len(rec.requests) == 2


def test_long_retry_after_is_not_waited_for(make_client):
    client, rec = make_client([err(429, "QUOTA_EXCEEDED", headers={"Retry-After": "3600"})])
    with pytest.raises(RateLimitError) as info:
        client.search("alice", "x")
    assert info.value.retry_after == 3600.0
    assert rec.sleeps == []


def test_retries_stop_after_max_retries(make_client):
    client, rec = make_client([err(429, "RATE_LIMITED", headers={"Retry-After": "0"})] * 3, max_retries=2)
    with pytest.raises(RateLimitError):
        client.search("alice", "x")
    assert len(rec.requests) == 3


def test_503_retried_for_add_with_message_ids(make_client):
    client, rec = make_client([err(503, "REQUEST_DEADLINE_EXCEEDED"), ok(ADD_RESPONSE)])
    client.add("alice", "x")
    assert len(rec.requests) == 2
    assert rec.body(0)["messages"] == rec.body(1)["messages"]


def test_503_not_retried_for_add_without_message_ids(make_client):
    client, rec = make_client([err(503, "OVERLOADED")])
    with pytest.raises(ServiceUnavailableError):
        client.add("alice", [{"role": "user", "content": "x"}])
    assert len(rec.requests) == 1


def test_connect_error_is_retried_for_any_request(make_client):
    client, rec = make_client([httpx.ConnectError("refused"), ok({"status": "ok", "deleted": True})])
    assert client.delete("alice", "f1") is True
    assert len(rec.requests) == 2


def test_read_timeout_retried_only_when_idempotent(make_client):
    client, rec = make_client([httpx.ReadTimeout("slow"), ok(SEARCH_RESPONSE)])
    client.search("alice", "x")
    assert len(rec.requests) == 2

    client, rec = make_client([httpx.ReadTimeout("slow")])
    with pytest.raises(TransportError):
        client.add("alice", [{"role": "user", "content": "x"}])
    assert len(rec.requests) == 1

    client, rec = make_client([httpx.ReadTimeout("slow"), ok({"status": "success", "deleted": True})])
    assert client.delete("alice", "f1") is True
    assert len(rec.requests) == 2


def test_max_retries_zero_disables_retries(make_client):
    client, rec = make_client([httpx.ConnectError("refused")], max_retries=0)
    with pytest.raises(TransportError):
        client.search("alice", "x")
    assert len(rec.requests) == 1


def test_for_user_binds_user(make_client):
    client, rec = make_client([ok(SEARCH_RESPONSE), ok(ADD_RESPONSE)])
    bob = client.for_user("bob")
    bob.search("x")
    bob.add("hello")
    assert rec.body(0)["user_id"] == "bob"
    assert rec.body(1)["user_id"] == "bob"
    assert not hasattr(bob, "forget_user")


def test_repr_hides_api_key(make_client):
    client, _ = make_client([])
    assert KEY not in repr(client)
    assert KEY not in repr(client.for_user("alice"))


def test_settings_validation_and_env(monkeypatch):
    for var in ("MEMQUIRA_URL", "MEMQUIRA_API_KEY", "MEMQUIRA_ORG_ID"):
        monkeypatch.delenv(var, raising=False)
    with pytest.raises(ValueError, match="MEMQUIRA_URL"):
        Memquira()
    with pytest.raises(ValueError, match="http"):
        Memquira("memquira.test", KEY, ORG)
    monkeypatch.setenv("MEMQUIRA_URL", "http://localhost:8080/")
    monkeypatch.setenv("MEMQUIRA_API_KEY", KEY)
    monkeypatch.setenv("MEMQUIRA_ORG_ID", ORG)
    with Memquira() as client:
        assert client.base_url == "http://localhost:8080"
        assert client.org_id == ORG


@pytest.mark.parametrize("bad", ["", "  ", None])
def test_user_id_required(make_client, bad):
    client, rec = make_client([])
    with pytest.raises(ValueError, match="user_id"):
        client.search(bad, "x")
    assert rec.requests == []


def test_parse_retry_after():
    assert parse_retry_after(None) is None
    assert parse_retry_after("5") == 5.0
    assert parse_retry_after("-3") == 0.0
    assert parse_retry_after("soon") is None
    assert parse_retry_after("Wed, 18 Nov 2026 10:00:30 GMT", now=1_794_996_000.0) == 30.0
