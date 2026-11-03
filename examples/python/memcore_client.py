"""Example memcore HTTP client (not a published SDK)."""

from __future__ import annotations

import uuid
from typing import Any, Optional

import requests


def new_message(role: str, content: str, *, name: Optional[str] = None) -> dict[str, Any]:
    """Create a message once, with its own stable ``message_id``.

    Keep the returned dict and send it unchanged on a retry: the same ``message_id`` with the
    same message is a retry, not a new message. Every new message (even one repeating earlier
    text) gets a new id; reusing an id for a different message is rejected with 409
    ``MESSAGE_ID_CONFLICT``.
    """
    message: dict[str, Any] = {
        "role": role,
        "content": content,
        "message_id": f"msg_{uuid.uuid4().hex}",
    }
    if name:
        message["name"] = name
    return message


class MemcoreApiError(Exception):
    def __init__(
        self, status_code: int, payload: Any, retry_after: Optional[str] = None
    ) -> None:
        self.status_code = status_code
        self.payload = payload
        self.retry_after = retry_after
        code = None
        message = None
        request_id = None
        if isinstance(payload, dict):
            err = payload.get("error") or {}
            if isinstance(err, dict):
                code = err.get("code")
                message = err.get("message")
                request_id = err.get("request_id")
        self.code = code
        super().__init__(
            f"memcore HTTP {status_code}"
            + (f" code={code}" if code else "")
            + (f" request_id={request_id}" if request_id else "")
            + (f": {message}" if message else "")
        )


class MemcoreClient:
    def __init__(
        self,
        base_url: str,
        api_key: str,
        org_id: str,
        *,
        timeout: float = 30.0,
        session: Optional[requests.Session] = None,
    ) -> None:
        if not base_url or not api_key or not org_id:
            raise ValueError("base_url, api_key, and org_id are required")
        self.base_url = base_url.rstrip("/")
        self._api_key = api_key
        self.org_id = org_id
        self.timeout = timeout
        self._session = session or requests.Session()

    def _headers(self) -> dict[str, str]:
        return {
            "Authorization": f"Bearer {self._api_key}",
            "X-Organization-ID": self.org_id,
            "Content-Type": "application/json",
            "Accept": "application/json",
        }

    def _request(self, method: str, path: str, **kwargs: Any) -> dict[str, Any]:
        url = f"{self.base_url}{path}"
        response = self._session.request(
            method,
            url,
            headers=self._headers(),
            timeout=self.timeout,
            **kwargs,
        )
        try:
            payload: Any = response.json()
        except ValueError:
            payload = {"raw": response.text[:500]}
        if not response.ok:
            raise MemcoreApiError(
                response.status_code, payload, response.headers.get("Retry-After")
            )
        if not isinstance(payload, dict):
            raise MemcoreApiError(response.status_code, {"error": {"message": "expected JSON object"}})
        return payload

    def create_memory(
        self, user_id: str, session_id: str, messages: list[dict[str, Any]]
    ) -> dict[str, Any]:
        """Add messages made with ``new_message`` to ``session_id``.

        Message ids are unique per organization, user and session, so a retry must send the
        same ``session_id`` and the same message dicts.
        """
        return self._request(
            "POST",
            "/api/v1/memories",
            json={
                "user_id": user_id,
                "session_id": session_id,
                "messages": messages,
                "metadata": {"source": "examples/python"},
            },
        )

    def search_memories(self, user_id: str, query: str, limit: int = 5) -> dict[str, Any]:
        return self._request(
            "POST",
            "/api/v1/memories/search",
            json={"user_id": user_id, "query": query, "limit": limit},
        )

    def build_context(
        self, user_id: str, query: str, max_tokens: int = 1000
    ) -> dict[str, Any]:
        return self._request(
            "POST",
            "/api/v1/context",
            json={"user_id": user_id, "query": query, "max_tokens": max_tokens},
        )

    def memory_context(
        self,
        user_id: str,
        query: str,
        *,
        token_budget: Optional[int] = None,
        include_items: bool = False,
        packing: Optional[str] = None,
        exclude_message_ids: Optional[list[str]] = None,
    ) -> dict[str, Any]:
        """The server-side context of one memory user (``POST /api/v1/memories/context``).

        Only the options you pass are sent, so a call without them gets the server's validated
        default. ``exclude_message_ids`` leaves out the *episodes* of those client message ids
        (facts are never removed); see ``multi_participant_context``.
        """
        body: dict[str, Any] = {"user_id": user_id, "query": query}
        if token_budget is not None:
            body["token_budget"] = token_budget
        if include_items:
            body["include_items"] = True
        if packing is not None:
            body["packing"] = packing
        if exclude_message_ids:
            body["exclude_message_ids"] = list(exclude_message_ids)
        return self._request("POST", "/api/v1/memories/context", json=body)

    def multi_participant_context(
        self,
        user_ids: list[str],
        query: str,
        *,
        token_budget: Optional[int] = None,
        packing: Optional[str] = None,
    ) -> dict[str, Any]:
        """Contexts of several memory users for one prompt, without repeating a message.

        When one conversation is stored for every participant, each message is an episode of each
        of them, so concatenated per-user contexts repeat it verbatim. Users are fetched in order;
        the message ids of episodes already returned are excluded from the later users' contexts.
        Facts are never excluded (each user's extracted facts are their own memories).
        Returns ``{"contexts": [...], "excluded_message_ids": [...]}`` in ``user_ids`` order.
        """
        seen: list[str] = []
        contexts = []
        for user_id in user_ids:
            context = self.memory_context(
                user_id,
                query,
                token_budget=token_budget,
                include_items=True,
                packing=packing,
                exclude_message_ids=seen,
            )
            contexts.append(context)
            for item in context.get("items") or []:
                message_id = item.get("message_id")
                if item.get("kind") == "episode" and message_id and message_id not in seen:
                    seen.append(message_id)
        return {"contexts": contexts, "excluded_message_ids": seen}

    def list_memories(self, user_id: str, *, limit: int = 50, cursor: Optional[str] = None) -> dict[str, Any]:
        params: dict[str, Any] = {"limit": limit}
        if cursor:
            params["cursor"] = cursor
        return self._request("GET", f"/api/v1/users/{user_id}/memories", params=params)

    def delete_memory(self, user_id: str, memory_id: str) -> dict[str, Any]:
        return self._request("DELETE", f"/api/v1/users/{user_id}/memories/{memory_id}")

    def export_user(self, user_id: str) -> dict[str, Any]:
        return self._request(
            "GET",
            f"/api/v1/users/{user_id}/export",
            params={"include_events": "true"},
        )

    def import_user(
        self, user_id: str, payload: dict[str, Any], dry_run: bool = False
    ) -> dict[str, Any]:
        body = dict(payload)
        body["dry_run"] = dry_run
        return self._request("POST", f"/api/v1/users/{user_id}/import", json=body)
