from __future__ import annotations

import json
from collections.abc import Callable
from typing import Any

import httpx
import pytest

from memquira import AsyncMemquira, Memquira

BASE = "http://memquira.test"
KEY = "test-key-not-a-secret"
ORG = "org-1"


class Recorder:
    """A scripted transport: each request takes the next response; requests and sleeps are recorded."""

    def __init__(self, responses: list[Any]) -> None:
        self.responses = list(responses)
        self.requests: list[httpx.Request] = []
        self.sleeps: list[float] = []

    def handler(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        if not self.responses:
            raise AssertionError(f"unexpected request {request.method} {request.url}")
        nxt = self.responses.pop(0)
        if isinstance(nxt, Exception):
            raise nxt
        if callable(nxt):
            result: httpx.Response = nxt(request)
            return result
        return nxt

    def body(self, index: int = -1) -> Any:
        return json.loads(self.requests[index].content)


def ok(data: dict[str, Any], status: int = 200, headers: dict[str, str] | None = None) -> httpx.Response:
    return httpx.Response(status, json=data, headers=headers)


def err(status: int, code: str, message: str = "msg", headers: dict[str, str] | None = None) -> httpx.Response:
    return httpx.Response(
        status, json={"error": {"code": code, "message": message, "request_id": "req-1"}}, headers=headers
    )


@pytest.fixture
def make_client() -> Callable[..., tuple[Memquira, Recorder]]:
    def make(responses: list[Any], **kwargs: Any) -> tuple[Memquira, Recorder]:
        rec = Recorder(responses)
        client = Memquira(
            BASE, KEY, ORG, http_client=httpx.Client(transport=httpx.MockTransport(rec.handler)), **kwargs
        )
        client._sleep = rec.sleeps.append
        return client, rec

    return make


@pytest.fixture
def make_async_client() -> Callable[..., tuple[AsyncMemquira, Recorder]]:
    def make(responses: list[Any], **kwargs: Any) -> tuple[AsyncMemquira, Recorder]:
        rec = Recorder(responses)
        client = AsyncMemquira(
            BASE, KEY, ORG, http_client=httpx.AsyncClient(transport=httpx.MockTransport(rec.handler)), **kwargs
        )

        async def sleep(seconds: float) -> None:
            rec.sleeps.append(seconds)

        client._sleep = sleep
        return client, rec

    return make
