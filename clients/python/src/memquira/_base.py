"""Request building, retry policy and response handling shared by the sync and async clients."""

from __future__ import annotations

import email.utils
import os
import random
import time
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Generic, TypeVar
from urllib.parse import quote

import httpx

from . import errors
from ._version import __version__
from .models import (
    AddResult,
    MemoryContext,
    MemoryPage,
    Message,
    Readiness,
    SearchHit,
    VersionInfo,
)

DEFAULT_TIMEOUT = 60.0
DEFAULT_MAX_RETRIES = 2
DEFAULT_MAX_RETRY_WAIT = 30.0

MessageInput = str | Message | Mapping[str, Any]
MessagesInput = MessageInput | Sequence[MessageInput]

T = TypeVar("T")


@dataclass(frozen=True)
class RequestSpec(Generic[T]):
    method: str
    path: str
    parse: Callable[[int, Any], T]
    json: dict[str, Any] | None = None
    params: dict[str, Any] | None = None
    # Safe to send again when the response was lost (timeout, dropped connection) or the server answered 503.
    idempotent: bool = False
    # Non-2xx statuses passed to ``parse`` instead of raised (the readiness probe answers 503 when not ready).
    pass_through: frozenset[int] = frozenset()


def require_text(value: Any, name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} must be a non-empty string")
    return value


def segment(value: Any, name: str) -> str:
    return quote(require_text(value, name), safe="")


def normalize_messages(messages: MessagesInput) -> list[dict[str, Any]]:
    """Accept a string, a ``Message``, a dict, or a sequence of them. A string is one new user message."""
    if isinstance(messages, (str, Message, Mapping)):
        items: Sequence[MessageInput] = [messages]
    else:
        items = list(messages)
    if not items:
        raise ValueError("messages must not be empty")
    out: list[dict[str, Any]] = []
    for item in items:
        if isinstance(item, str):
            out.append(Message(role="user", content=require_text(item, "message text")).to_dict())
        elif isinstance(item, Message):
            out.append(item.to_dict())
        elif isinstance(item, Mapping):
            body = dict(item)
            require_text(body.get("role"), "message role")
            require_text(body.get("content"), "message content")
            out.append(body)
        else:
            raise TypeError(f"unsupported message type: {type(item).__name__}")
    return out


def headers(api_key: str, org_id: str) -> dict[str, str]:
    return {
        "Authorization": f"Bearer {api_key}",
        "X-Organization-ID": org_id,
        "Accept": "application/json",
        "User-Agent": f"memquira-python/{__version__}",
    }


def settings_from_env(base_url: str | None, api_key: str | None, org_id: str | None) -> tuple[str, str, str]:
    """Fill missing settings from MEMQUIRA_URL, MEMQUIRA_API_KEY and MEMQUIRA_ORG_ID."""
    resolved = []
    for value, var in ((base_url, "MEMQUIRA_URL"), (api_key, "MEMQUIRA_API_KEY"), (org_id, "MEMQUIRA_ORG_ID")):
        value = value or os.environ.get(var)
        if not value:
            raise ValueError(f"{var} is not set")
        resolved.append(value)
    return resolved[0], resolved[1], resolved[2]


def check_settings(base_url: str, api_key: str, org_id: str) -> str:
    require_text(base_url, "base_url")
    require_text(api_key, "api_key")
    require_text(org_id, "org_id")
    if not base_url.startswith(("http://", "https://")):
        raise ValueError("base_url must start with http:// or https://")
    return base_url.rstrip("/")


# ---------------------------------------------------------------------------------------------------------------
# Retry policy


def parse_retry_after(value: str | None, now: float | None = None) -> float | None:
    """Seconds from a ``Retry-After`` header (delta-seconds or an HTTP date); None if absent or unreadable."""
    if not value:
        return None
    text = value.strip()
    try:
        return max(0.0, float(text))
    except ValueError:
        pass
    try:
        when: datetime | None = email.utils.parsedate_to_datetime(text)
    except (TypeError, ValueError, IndexError):
        return None
    if when is None:
        return None
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    return max(0.0, when.timestamp() - (time.time() if now is None else now))


# Errors raised before the request reached the server: always safe to send again.
_NOT_SENT = (httpx.ConnectError, httpx.ConnectTimeout, httpx.PoolTimeout)
# Errors after the request may have reached the server: safe only for idempotent requests.
_MAYBE_SENT = (httpx.TimeoutException, httpx.NetworkError, httpx.RemoteProtocolError)


class RetryPolicy:
    """When to try a request again.

    - 429: always, after ``Retry-After`` (the server rejected the request before running it).
    - 503: only for idempotent requests, after ``Retry-After``.
    - Connection failures before sending: always.
    - Timeouts and dropped connections after sending: only for idempotent requests.

    A ``Retry-After`` longer than ``max_retry_wait`` is not waited for; the error is raised instead.
    Reads and deletes are idempotent (deleting an already deleted memory or user succeeds again); ``add`` is
    idempotent only when every message has a ``message_id``, because the server answers a resent message with
    the result of the first call.
    """

    def __init__(self, max_retries: int, max_retry_wait: float, backoff_base: float = 0.5) -> None:
        if max_retries < 0:
            raise ValueError("max_retries must be >= 0")
        self.max_retries = max_retries
        self.max_retry_wait = max_retry_wait
        self.backoff_base = backoff_base

    def _backoff(self, attempt: int) -> float:
        delay: float = min(self.max_retry_wait, self.backoff_base * (2**attempt))
        return delay * (0.5 + random.random() / 2)

    def after_response(self, attempt: int, response: httpx.Response, idempotent: bool) -> float | None:
        if attempt >= self.max_retries:
            return None
        status = response.status_code
        if status != 429 and not (status == 503 and idempotent):
            return None
        hinted = parse_retry_after(response.headers.get("Retry-After"))
        if hinted is None:
            return self._backoff(attempt)
        return hinted if hinted <= self.max_retry_wait else None

    def after_exception(self, attempt: int, exc: Exception, idempotent: bool) -> float | None:
        if attempt >= self.max_retries:
            return None
        if isinstance(exc, _NOT_SENT) or (idempotent and isinstance(exc, _MAYBE_SENT)):
            return self._backoff(attempt)
        return None


# ---------------------------------------------------------------------------------------------------------------
# Responses


def _json(response: httpx.Response) -> Any:
    try:
        return response.json()
    except ValueError:
        return None


def error_from_response(response: httpx.Response) -> errors.APIError:
    body = _json(response)
    detail = body.get("error") if isinstance(body, dict) else None
    detail = detail if isinstance(detail, dict) else {}
    message = detail.get("message")
    if message is None and body is None:
        message = response.text[:200] or None
    return errors.error_class_for_status(response.status_code)(
        response.status_code,
        code=detail.get("code"),
        message=message,
        request_id=detail.get("request_id") or response.headers.get("X-Request-ID"),
        details=detail.get("details"),
        retry_after=parse_retry_after(response.headers.get("Retry-After")),
        body=body,
    )


def handle_response(spec: RequestSpec[T], response: httpx.Response) -> T:
    if response.is_success or response.status_code in spec.pass_through:
        body = _json(response)
        if not isinstance(body, dict):
            raise errors.MemquiraError(f"expected a JSON object from {spec.method} {spec.path}")
        return spec.parse(response.status_code, body)
    raise error_from_response(response)


def transport_error(spec: RequestSpec[Any], exc: Exception) -> errors.TransportError:
    return errors.TransportError(f"{spec.method} {spec.path} failed: {type(exc).__name__}: {exc}")


# ---------------------------------------------------------------------------------------------------------------
# Endpoints


def add_spec(
    user_id: str,
    messages: MessagesInput,
    *,
    session_id: str | None,
    session_timestamp: str | datetime | None,
    metadata: Mapping[str, Any] | None,
) -> RequestSpec[AddResult]:
    body: dict[str, Any] = {"user_id": require_text(user_id, "user_id"), "messages": normalize_messages(messages)}
    if session_id is not None:
        body["session_id"] = require_text(session_id, "session_id")
    if session_timestamp is not None:
        body["session_timestamp"] = (
            session_timestamp.isoformat() if isinstance(session_timestamp, datetime) else session_timestamp
        )
    if metadata is not None:
        body["metadata"] = dict(metadata)
    idempotent = all(m.get("message_id") for m in body["messages"])
    return RequestSpec(
        "POST", "/api/v1/memories", lambda _s, d: AddResult.from_dict(d), json=body, idempotent=idempotent
    )


def search_spec(
    user_id: str,
    query: str,
    *,
    limit: int | None,
    memory_types: Sequence[str] | None,
    include_history: bool,
    retrieval_mode: str | None,
) -> RequestSpec[list[SearchHit]]:
    body: dict[str, Any] = {"user_id": require_text(user_id, "user_id"), "query": require_text(query, "query")}
    if limit is not None:
        body["limit"] = limit
    if memory_types:
        body["filters"] = {"memory_type": list(memory_types)}
    if include_history:
        body["include_history"] = True
    if retrieval_mode is not None:
        body["retrieval_mode"] = retrieval_mode
    return RequestSpec(
        "POST",
        "/api/v1/memories/search",
        lambda _s, d: [SearchHit.from_dict(r) for r in d.get("results") or []],
        json=body,
        idempotent=True,
    )


def context_spec(
    user_id: str,
    query: str,
    *,
    token_budget: int | None,
    include_items: bool,
    packing: str | None,
    exclude_message_ids: Sequence[str] | None,
) -> RequestSpec[MemoryContext]:
    body: dict[str, Any] = {"user_id": require_text(user_id, "user_id"), "query": require_text(query, "query")}
    if token_budget is not None:
        body["token_budget"] = token_budget
    if include_items:
        body["include_items"] = True
    if packing is not None:
        body["packing"] = packing
    if exclude_message_ids:
        body["exclude_message_ids"] = list(exclude_message_ids)
    return RequestSpec(
        "POST",
        "/api/v1/memories/context",
        lambda _s, d: MemoryContext.from_dict(d),
        json=body,
        idempotent=True,
    )


def list_spec(
    user_id: str,
    *,
    limit: int | None,
    cursor: str | None,
    memory_type: str | None,
    query: str | None,
) -> RequestSpec[MemoryPage]:
    params: dict[str, Any] = {}
    if limit is not None:
        params["limit"] = limit
    if cursor:
        params["cursor"] = cursor
    if memory_type:
        params["memory_type"] = memory_type
    if query:
        params["q"] = query
    return RequestSpec(
        "GET",
        f"/api/v1/users/{segment(user_id, 'user_id')}/memories",
        lambda _s, d: MemoryPage.from_dict(d),
        params=params or None,
        idempotent=True,
    )


def delete_spec(user_id: str, memory_id: str) -> RequestSpec[bool]:
    return RequestSpec(
        "DELETE",
        f"/api/v1/users/{segment(user_id, 'user_id')}/memories/{segment(memory_id, 'memory_id')}",
        lambda _s, d: bool(d.get("deleted")),
        idempotent=True,
    )


def forget_user_spec(user_id: str) -> RequestSpec[bool]:
    return RequestSpec(
        "DELETE",
        f"/api/v1/users/{segment(user_id, 'user_id')}",
        lambda _s, d: bool(d.get("deleted")),
        idempotent=True,
    )


def version_spec() -> RequestSpec[VersionInfo]:
    return RequestSpec("GET", "/api/v1/version", lambda _s, d: VersionInfo.from_dict(d), idempotent=True)


def ready_spec() -> RequestSpec[Readiness]:
    return RequestSpec(
        "GET",
        "/health/ready",
        Readiness.from_response,
        idempotent=True,
        pass_through=frozenset({503}),
    )
