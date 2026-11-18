"""Synchronous and asynchronous Memquira clients."""

from __future__ import annotations

import asyncio
import time
from collections.abc import AsyncIterator, Awaitable, Callable, Iterator, Mapping, Sequence
from datetime import datetime
from typing import Any, TypeVar

import httpx

from . import _base
from ._base import DEFAULT_MAX_RETRIES, DEFAULT_MAX_RETRY_WAIT, DEFAULT_TIMEOUT, MessagesInput, RequestSpec
from .models import AddResult, Memory, MemoryContext, MemoryPage, Readiness, SearchHit, VersionInfo

T = TypeVar("T")


class _ClientCore:
    def __init__(
        self,
        base_url: str | None,
        api_key: str | None,
        org_id: str | None,
        max_retries: int,
        max_retry_wait: float,
    ) -> None:
        base_url, api_key, org_id = _base.settings_from_env(base_url, api_key, org_id)
        self._base_url = _base.check_settings(base_url, api_key, org_id)
        self._headers = _base.headers(api_key, org_id)
        self._org_id = org_id
        self._retry = _base.RetryPolicy(max_retries, max_retry_wait)

    @property
    def base_url(self) -> str:
        return self._base_url

    @property
    def org_id(self) -> str:
        return self._org_id

    def __repr__(self) -> str:  # never shows the API key
        return f"{type(self).__name__}(base_url={self._base_url!r}, org_id={self._org_id!r})"


class Memquira(_ClientCore):
    """Client for a Memquira server.

    ``base_url``, ``api_key`` and ``org_id`` default to the ``MEMQUIRA_URL``, ``MEMQUIRA_API_KEY`` and
    ``MEMQUIRA_ORG_ID`` environment variables. Every method takes the ``user_id`` whose memories it acts on;
    ``for_user`` returns a view bound to one user, which is what an agent integration should hand to a model.

    Use it as a context manager, or call ``close()``, to release the connection pool.
    """

    def __init__(
        self,
        base_url: str | None = None,
        api_key: str | None = None,
        org_id: str | None = None,
        *,
        timeout: float = DEFAULT_TIMEOUT,
        max_retries: int = DEFAULT_MAX_RETRIES,
        max_retry_wait: float = DEFAULT_MAX_RETRY_WAIT,
        http_client: httpx.Client | None = None,
    ) -> None:
        super().__init__(base_url, api_key, org_id, max_retries, max_retry_wait)
        self._owns_http = http_client is None
        self._http = http_client or httpx.Client(timeout=timeout)
        self._sleep: Callable[[float], None] = time.sleep

    # -- plumbing ------------------------------------------------------------------------------------------------

    def _send(self, spec: RequestSpec[T]) -> T:
        attempt = 0
        while True:
            try:
                response = self._http.request(
                    spec.method, self._base_url + spec.path, json=spec.json, params=spec.params, headers=self._headers
                )
            except httpx.TransportError as exc:
                delay = self._retry.after_exception(attempt, exc, spec.idempotent)
                if delay is None:
                    raise _base.transport_error(spec, exc) from exc
            else:
                delay = None
                if not response.is_success and response.status_code not in spec.pass_through:
                    delay = self._retry.after_response(attempt, response, spec.idempotent)
                if delay is None:
                    return _base.handle_response(spec, response)
            self._sleep(delay)
            attempt += 1

    def close(self) -> None:
        if self._owns_http:
            self._http.close()

    def __enter__(self) -> Memquira:
        return self

    def __exit__(self, *exc_info: object) -> None:
        self.close()

    # -- memories ------------------------------------------------------------------------------------------------

    def add(
        self,
        user_id: str,
        messages: MessagesInput,
        *,
        session_id: str | None = None,
        session_timestamp: str | datetime | None = None,
        metadata: Mapping[str, Any] | None = None,
    ) -> AddResult:
        """Store messages; the server extracts memories from them with its language model.

        ``messages`` is a string (one user message), a ``Message``, a dict, or a list of them. This call runs
        model inference on the server and can take seconds. It is resent after a lost response only when every
        message carries a ``message_id`` (``Message`` objects and strings always do), so a retry never stores a
        message twice.
        """
        return self._send(
            _base.add_spec(
                user_id, messages, session_id=session_id, session_timestamp=session_timestamp, metadata=metadata
            )
        )

    def search(
        self,
        user_id: str,
        query: str,
        *,
        limit: int | None = None,
        memory_types: Sequence[str] | None = None,
        include_history: bool = False,
        retrieval_mode: str | None = None,
    ) -> list[SearchHit]:
        """Ranked memories for a query. ``include_history`` also returns superseded memories with their lifecycle."""
        return self._send(
            _base.search_spec(
                user_id,
                query,
                limit=limit,
                memory_types=memory_types,
                include_history=include_history,
                retrieval_mode=retrieval_mode,
            )
        )

    def context(
        self,
        user_id: str,
        query: str,
        *,
        token_budget: int | None = None,
        include_items: bool = False,
        packing: str | None = None,
        exclude_message_ids: Sequence[str] | None = None,
    ) -> MemoryContext:
        """The memories that matter for the next prompt, packed into ``token_budget`` tokens as ready-to-use text."""
        return self._send(
            _base.context_spec(
                user_id,
                query,
                token_budget=token_budget,
                include_items=include_items,
                packing=packing,
                exclude_message_ids=exclude_message_ids,
            )
        )

    def list_memories(
        self,
        user_id: str,
        *,
        limit: int | None = None,
        cursor: str | None = None,
        memory_type: str | None = None,
        query: str | None = None,
    ) -> MemoryPage:
        """One page of the user's memories. Pass the page's ``next_cursor`` to get the next one."""
        return self._send(_base.list_spec(user_id, limit=limit, cursor=cursor, memory_type=memory_type, query=query))

    def iter_memories(
        self,
        user_id: str,
        *,
        page_size: int | None = None,
        memory_type: str | None = None,
        query: str | None = None,
    ) -> Iterator[Memory]:
        """Every memory of the user, fetched page by page."""
        cursor: str | None = None
        while True:
            page = self.list_memories(user_id, limit=page_size, cursor=cursor, memory_type=memory_type, query=query)
            yield from page.memories
            if not page.next_cursor:
                return
            cursor = page.next_cursor

    def delete(self, user_id: str, memory_id: str) -> bool:
        """Delete one memory. Deleting it again succeeds; an id the user never had raises ``NotFoundError``."""
        return self._send(_base.delete_spec(user_id, memory_id))

    def forget_user(self, user_id: str) -> bool:
        """Erase everything stored for the user: memories, vectors and episodes.

        Needs the ``UserDelete`` scope. Keep it out of agent integrations: a model should never be able to call it.
        """
        return self._send(_base.forget_user_spec(user_id))

    # -- server --------------------------------------------------------------------------------------------------

    def version(self) -> VersionInfo:
        return self._send(_base.version_spec())

    def ready(self) -> Readiness:
        """The readiness probe. Returns ``ready=False`` instead of raising when the server is not ready."""
        return self._send(_base.ready_spec())

    def for_user(self, user_id: str) -> UserMemory:
        """A view of this client bound to one user."""
        return UserMemory(self, user_id)


class UserMemory:
    """The memory operations of one user. ``user_id`` is fixed here, so callers (or models) cannot change it."""

    def __init__(self, client: Memquira, user_id: str) -> None:
        self._client = client
        self.user_id = _base.require_text(user_id, "user_id")

    def __repr__(self) -> str:
        return f"UserMemory(user_id={self.user_id!r}, client={self._client!r})"

    def add(
        self,
        messages: MessagesInput,
        *,
        session_id: str | None = None,
        session_timestamp: str | datetime | None = None,
        metadata: Mapping[str, Any] | None = None,
    ) -> AddResult:
        return self._client.add(
            self.user_id, messages, session_id=session_id, session_timestamp=session_timestamp, metadata=metadata
        )

    def search(
        self,
        query: str,
        *,
        limit: int | None = None,
        memory_types: Sequence[str] | None = None,
        include_history: bool = False,
        retrieval_mode: str | None = None,
    ) -> list[SearchHit]:
        return self._client.search(
            self.user_id,
            query,
            limit=limit,
            memory_types=memory_types,
            include_history=include_history,
            retrieval_mode=retrieval_mode,
        )

    def context(
        self,
        query: str,
        *,
        token_budget: int | None = None,
        include_items: bool = False,
        packing: str | None = None,
        exclude_message_ids: Sequence[str] | None = None,
    ) -> MemoryContext:
        return self._client.context(
            self.user_id,
            query,
            token_budget=token_budget,
            include_items=include_items,
            packing=packing,
            exclude_message_ids=exclude_message_ids,
        )

    def list_memories(
        self,
        *,
        limit: int | None = None,
        cursor: str | None = None,
        memory_type: str | None = None,
        query: str | None = None,
    ) -> MemoryPage:
        return self._client.list_memories(
            self.user_id, limit=limit, cursor=cursor, memory_type=memory_type, query=query
        )

    def iter_memories(
        self, *, page_size: int | None = None, memory_type: str | None = None, query: str | None = None
    ) -> Iterator[Memory]:
        return self._client.iter_memories(self.user_id, page_size=page_size, memory_type=memory_type, query=query)

    def delete(self, memory_id: str) -> bool:
        return self._client.delete(self.user_id, memory_id)


class AsyncMemquira(_ClientCore):
    """Asynchronous client; the same methods as ``Memquira``, awaited. Use ``async with`` or ``await aclose()``."""

    def __init__(
        self,
        base_url: str | None = None,
        api_key: str | None = None,
        org_id: str | None = None,
        *,
        timeout: float = DEFAULT_TIMEOUT,
        max_retries: int = DEFAULT_MAX_RETRIES,
        max_retry_wait: float = DEFAULT_MAX_RETRY_WAIT,
        http_client: httpx.AsyncClient | None = None,
    ) -> None:
        super().__init__(base_url, api_key, org_id, max_retries, max_retry_wait)
        self._owns_http = http_client is None
        self._http = http_client or httpx.AsyncClient(timeout=timeout)
        self._sleep: Callable[[float], Awaitable[None]] = asyncio.sleep

    async def _send(self, spec: RequestSpec[T]) -> T:
        attempt = 0
        while True:
            try:
                response = await self._http.request(
                    spec.method, self._base_url + spec.path, json=spec.json, params=spec.params, headers=self._headers
                )
            except httpx.TransportError as exc:
                delay = self._retry.after_exception(attempt, exc, spec.idempotent)
                if delay is None:
                    raise _base.transport_error(spec, exc) from exc
            else:
                delay = None
                if not response.is_success and response.status_code not in spec.pass_through:
                    delay = self._retry.after_response(attempt, response, spec.idempotent)
                if delay is None:
                    return _base.handle_response(spec, response)
            await self._sleep(delay)
            attempt += 1

    async def aclose(self) -> None:
        if self._owns_http:
            await self._http.aclose()

    async def __aenter__(self) -> AsyncMemquira:
        return self

    async def __aexit__(self, *exc_info: object) -> None:
        await self.aclose()

    async def add(
        self,
        user_id: str,
        messages: MessagesInput,
        *,
        session_id: str | None = None,
        session_timestamp: str | datetime | None = None,
        metadata: Mapping[str, Any] | None = None,
    ) -> AddResult:
        """See ``Memquira.add``."""
        return await self._send(
            _base.add_spec(
                user_id, messages, session_id=session_id, session_timestamp=session_timestamp, metadata=metadata
            )
        )

    async def search(
        self,
        user_id: str,
        query: str,
        *,
        limit: int | None = None,
        memory_types: Sequence[str] | None = None,
        include_history: bool = False,
        retrieval_mode: str | None = None,
    ) -> list[SearchHit]:
        """See ``Memquira.search``."""
        return await self._send(
            _base.search_spec(
                user_id,
                query,
                limit=limit,
                memory_types=memory_types,
                include_history=include_history,
                retrieval_mode=retrieval_mode,
            )
        )

    async def context(
        self,
        user_id: str,
        query: str,
        *,
        token_budget: int | None = None,
        include_items: bool = False,
        packing: str | None = None,
        exclude_message_ids: Sequence[str] | None = None,
    ) -> MemoryContext:
        """See ``Memquira.context``."""
        return await self._send(
            _base.context_spec(
                user_id,
                query,
                token_budget=token_budget,
                include_items=include_items,
                packing=packing,
                exclude_message_ids=exclude_message_ids,
            )
        )

    async def list_memories(
        self,
        user_id: str,
        *,
        limit: int | None = None,
        cursor: str | None = None,
        memory_type: str | None = None,
        query: str | None = None,
    ) -> MemoryPage:
        """See ``Memquira.list_memories``."""
        return await self._send(
            _base.list_spec(user_id, limit=limit, cursor=cursor, memory_type=memory_type, query=query)
        )

    async def iter_memories(
        self,
        user_id: str,
        *,
        page_size: int | None = None,
        memory_type: str | None = None,
        query: str | None = None,
    ) -> AsyncIterator[Memory]:
        """Every memory of the user, fetched page by page (``async for``)."""
        cursor: str | None = None
        while True:
            page = await self.list_memories(
                user_id, limit=page_size, cursor=cursor, memory_type=memory_type, query=query
            )
            for memory in page.memories:
                yield memory
            if not page.next_cursor:
                return
            cursor = page.next_cursor

    async def delete(self, user_id: str, memory_id: str) -> bool:
        """See ``Memquira.delete``."""
        return await self._send(_base.delete_spec(user_id, memory_id))

    async def forget_user(self, user_id: str) -> bool:
        """See ``Memquira.forget_user``. Keep it out of agent integrations."""
        return await self._send(_base.forget_user_spec(user_id))

    async def version(self) -> VersionInfo:
        return await self._send(_base.version_spec())

    async def ready(self) -> Readiness:
        """See ``Memquira.ready``."""
        return await self._send(_base.ready_spec())

    def for_user(self, user_id: str) -> AsyncUserMemory:
        """A view of this client bound to one user."""
        return AsyncUserMemory(self, user_id)


class AsyncUserMemory:
    """Asynchronous ``UserMemory``."""

    def __init__(self, client: AsyncMemquira, user_id: str) -> None:
        self._client = client
        self.user_id = _base.require_text(user_id, "user_id")

    def __repr__(self) -> str:
        return f"AsyncUserMemory(user_id={self.user_id!r}, client={self._client!r})"

    async def add(
        self,
        messages: MessagesInput,
        *,
        session_id: str | None = None,
        session_timestamp: str | datetime | None = None,
        metadata: Mapping[str, Any] | None = None,
    ) -> AddResult:
        return await self._client.add(
            self.user_id, messages, session_id=session_id, session_timestamp=session_timestamp, metadata=metadata
        )

    async def search(
        self,
        query: str,
        *,
        limit: int | None = None,
        memory_types: Sequence[str] | None = None,
        include_history: bool = False,
        retrieval_mode: str | None = None,
    ) -> list[SearchHit]:
        return await self._client.search(
            self.user_id,
            query,
            limit=limit,
            memory_types=memory_types,
            include_history=include_history,
            retrieval_mode=retrieval_mode,
        )

    async def context(
        self,
        query: str,
        *,
        token_budget: int | None = None,
        include_items: bool = False,
        packing: str | None = None,
        exclude_message_ids: Sequence[str] | None = None,
    ) -> MemoryContext:
        return await self._client.context(
            self.user_id,
            query,
            token_budget=token_budget,
            include_items=include_items,
            packing=packing,
            exclude_message_ids=exclude_message_ids,
        )

    async def list_memories(
        self,
        *,
        limit: int | None = None,
        cursor: str | None = None,
        memory_type: str | None = None,
        query: str | None = None,
    ) -> MemoryPage:
        return await self._client.list_memories(
            self.user_id, limit=limit, cursor=cursor, memory_type=memory_type, query=query
        )

    def iter_memories(
        self, *, page_size: int | None = None, memory_type: str | None = None, query: str | None = None
    ) -> AsyncIterator[Memory]:
        return self._client.iter_memories(self.user_id, page_size=page_size, memory_type=memory_type, query=query)

    async def delete(self, memory_id: str) -> bool:
        return await self._client.delete(self.user_id, memory_id)
