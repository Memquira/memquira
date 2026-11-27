"""Turn SDK exceptions into tool errors the model can act on, without internals or credentials."""

from __future__ import annotations

from typing import Any

import memquira

# Server messages are shown only for these codes; they describe the request, not the server.
_SAFE_MESSAGE_STATUSES = {400, 403, 404, 409, 413, 422, 429, 503}
# 503 codes the server returns before doing any work (pause, closed preview, not ready, admission control).
_REFUSED_BEFORE_WORK = {"PAID_OPERATIONS_PAUSED", "PREVIEW_DISABLED", "PREVIEW_NOT_READY", "OVERLOADED"}


def _truncate(text: str, limit: int = 300) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def describe(exc: BaseException, *, tool: str, request_id: str | None = None) -> dict[str, Any]:
    """A JSON-serialisable error description for a failed tool call."""
    writes = tool == "remember"
    info: dict[str, Any] = {"tool": tool}
    if isinstance(exc, memquira.APIError):
        info["status"] = exc.status_code
        if exc.code:
            info["code"] = exc.code
        if exc.request_id:
            info["server_request_id"] = exc.request_id
        if exc.retry_after is not None:
            info["retry_after_seconds"] = round(exc.retry_after, 1)
        server_text = _truncate(exc.message) if exc.message and exc.status_code in _SAFE_MESSAGE_STATUSES else None
        info["message"] = _api_message(exc, server_text, writes)
        info["retryable"] = exc.status_code in {429, 503} or (exc.status_code >= 500 and not writes)
    elif isinstance(exc, memquira.TransportError):
        if writes:
            info["message"] = (
                "Memquira did not answer, so it is unknown whether the memory was stored. Retry with the same "
                "request_id: if it was stored, nothing is stored twice."
            )
        else:
            info["message"] = "Memquira could not be reached (connection failed or timed out). Try again later."
        info["code"] = "UNREACHABLE"
        info["retryable"] = True
    else:
        info["message"] = "The tool failed unexpectedly. Details are in the connector's log (stderr)."
        info["code"] = "INTERNAL"
        info["retryable"] = False
    if writes and request_id:
        info["request_id"] = request_id
    return info


def _api_message(exc: memquira.APIError, server_text: str | None, writes: bool) -> str:
    status = exc.status_code
    not_stored = "Nothing was stored. " if writes and status < 500 and status != 409 else ""
    if status == 401:
        return (
            "The Memquira API key or developer token was rejected (missing, unknown, expired or revoked). "
            "Check MEMQUIRA_API_KEY."
        )
    if status == 403:
        if exc.code == "TRIAL_NOT_AVAILABLE":
            return not_stored + (server_text or "Paid memory operations are not available for this account.")
        return not_stored + (
            "The API key is not allowed to do this. It needs the scope for this operation "
            "(MemoryRead to read, MemoryWrite to remember, MemoryDelete to delete; on hosted tokens read/write/delete)."
            + (f" Server: {server_text}" if server_text else "")
        )
    if status == 404:
        return "No memory with this id exists for this user (it may already have been deleted)."
    if status == 409:
        return (
            "This request_id was already used for different content, so nothing was stored. "
            "Use a new request_id for new content."
            if exc.code == "MESSAGE_ID_CONFLICT"
            else (server_text or "The request conflicts with the stored state.")
        )
    if status == 429:
        wait = f" Try again in about {round(exc.retry_after)} s." if exc.retry_after else ""
        return not_stored + (server_text or "A rate limit or quota was reached.") + wait
    if status in {400, 413, 422}:
        return not_stored + "The server refused the request: " + (server_text or "invalid request")
    if status == 503:
        if exc.code in _REFUSED_BEFORE_WORK:
            return ("Nothing was stored. " if writes else "") + (server_text or "Memquira is paused or not ready.")
        return "Memquira is temporarily unavailable." + (
            " The memory may not have been stored; retry with the same request_id." if writes else " Try again shortly."
        )
    if status >= 500:
        if writes:
            return (
                f"Memquira failed (HTTP {status}); the memory may not have been stored. Retry with the same request_id "
                "to be safe."
            )
        return f"Memquira failed (HTTP {status}). Try again later."
    if 300 <= status < 400:
        return (
            f"Memquira answered with a redirect (HTTP {status}). Redirects are not followed, so the key is never sent "
            "to another address. Set MEMQUIRA_URL to the server's final address."
        )
    return f"Unexpected response from Memquira (HTTP {status})."
