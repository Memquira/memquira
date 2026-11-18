"""Request helpers and typed results.

Every result keeps the server's full JSON in ``raw``, so fields added by newer servers stay reachable.
"""

from __future__ import annotations

import uuid
from collections.abc import Mapping
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any


def new_message_id() -> str:
    """A fresh, unique message id."""
    return f"msg_{uuid.uuid4().hex}"


def _iso(value: str | datetime | None) -> str | None:
    if value is None or isinstance(value, str):
        return value
    return value.isoformat()


@dataclass(frozen=True)
class Message:
    """One chat message to store.

    A message gets its own ``message_id`` when it is created. Keep the object and send it unchanged on a
    retry: the server recognises the same id with the same message and does not store it twice. Every new
    message, even one repeating earlier text, needs a new id; reusing an id for a different message is
    rejected with ``ConflictError`` (``MESSAGE_ID_CONFLICT``).
    """

    role: str
    content: str
    message_id: str | None = field(default_factory=new_message_id)
    name: str | None = None
    timestamp: str | datetime | None = None

    def to_dict(self) -> dict[str, Any]:
        body: dict[str, Any] = {"role": self.role, "content": self.content}
        if self.message_id is not None:
            body["message_id"] = self.message_id
        if self.name is not None:
            body["name"] = self.name
        if self.timestamp is not None:
            body["timestamp"] = _iso(self.timestamp)
        return body


def _opt_float(value: Any) -> float | None:
    return None if value is None else float(value)


@dataclass(frozen=True)
class Memory:
    """A stored memory (an extracted fact)."""

    id: str
    content: str
    memory_type: str
    importance: float | None
    confidence: float | None
    metadata: Any
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Memory:
        return cls(
            id=str(data["id"]),
            content=str(data.get("content", "")),
            memory_type=str(data.get("memory_type", "")),
            importance=_opt_float(data.get("importance")),
            confidence=_opt_float(data.get("confidence")),
            metadata=data.get("metadata"),
            raw=data,
        )


@dataclass(frozen=True)
class IngestSummary:
    """What extraction did with the messages of one ``add`` call."""

    extracted: int
    stored: int
    added: int
    updated: int
    deleted: int
    noop: int
    invalidated: int
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> IngestSummary:
        def n(key: str) -> int:
            return int(data.get(key) or 0)

        return cls(
            extracted=n("extracted"),
            stored=n("stored"),
            added=n("added"),
            updated=n("updated"),
            deleted=n("deleted"),
            noop=n("noop"),
            invalidated=n("invalidated"),
            raw=data,
        )


@dataclass(frozen=True)
class AddResult:
    """The response of ``add``: the memories stored by this call and a summary."""

    status: str
    memories: list[Memory]
    summary: IngestSummary
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> AddResult:
        return cls(
            status=str(data.get("status", "")),
            memories=[Memory.from_dict(m) for m in data.get("memories") or []],
            summary=IngestSummary.from_dict(data.get("summary") or {}),
            raw=data,
        )


@dataclass(frozen=True)
class Lifecycle:
    """History state of a search hit, returned with ``include_history=True``."""

    status: str
    superseded_at: str | None
    superseded_by: str | None
    invalid_at: str | None

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> Lifecycle:
        return cls(
            status=str(data.get("status", "")),
            superseded_at=data.get("superseded_at"),
            superseded_by=data.get("superseded_by"),
            invalid_at=data.get("invalid_at"),
        )


@dataclass(frozen=True)
class SearchHit:
    """One ranked search result. ``score`` is the fused ranking score, ``vector_score`` the raw similarity."""

    id: str
    content: str
    memory_type: str
    score: float
    vector_score: float | None
    metadata: Any
    lifecycle: Lifecycle | None
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> SearchHit:
        lifecycle = data.get("lifecycle")
        return cls(
            id=str(data["fact_id"]),
            content=str(data.get("content", "")),
            memory_type=str(data.get("memory_type", "")),
            score=float(data.get("score") or 0.0),
            vector_score=_opt_float(data.get("vector_score")),
            metadata=data.get("metadata"),
            lifecycle=Lifecycle.from_dict(lifecycle) if isinstance(lifecycle, Mapping) else None,
            raw=data,
        )


@dataclass(frozen=True)
class ContextItem:
    """One memory or episode packed into a context."""

    id: str
    kind: str
    text: str
    line: str
    timestamp: str | None
    tokens: int
    vector_score: float | None
    message_id: str | None
    source_message_ids: list[str]
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> ContextItem:
        return cls(
            id=str(data.get("id", "")),
            kind=str(data.get("kind", "")),
            text=str(data.get("text", "")),
            line=str(data.get("line", "")),
            timestamp=data.get("timestamp"),
            tokens=int(data.get("tokens") or 0),
            vector_score=_opt_float(data.get("vector_score")),
            message_id=data.get("message_id"),
            source_message_ids=list(data.get("source_message_ids") or []),
            raw=data,
        )


@dataclass(frozen=True)
class MemoryContext:
    """Memories for one prompt, packed into a token budget. ``text`` is ready for a system prompt."""

    text: str
    token_count: int
    token_budget: int
    selected_count: int
    candidates_considered: int
    budget_bound: bool
    items: list[ContextItem] | None
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> MemoryContext:
        items = data.get("items")
        return cls(
            text=str(data.get("text", "")),
            token_count=int(data.get("token_count") or 0),
            token_budget=int(data.get("token_budget") or 0),
            selected_count=int(data.get("selected_count") or 0),
            candidates_considered=int(data.get("candidates_considered") or 0),
            budget_bound=bool(data.get("budget_bound")),
            items=[ContextItem.from_dict(i) for i in items] if isinstance(items, list) else None,
            raw=data,
        )


@dataclass(frozen=True)
class MemoryPage:
    """One page of a user's memories. Pass ``next_cursor`` to get the next page; ``None`` means the last page."""

    memories: list[Memory]
    next_cursor: str | None
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> MemoryPage:
        return cls(
            memories=[Memory.from_dict(m) for m in data.get("memories") or []],
            next_cursor=data.get("next_cursor") or None,
            raw=data,
        )


@dataclass(frozen=True)
class VersionInfo:
    """The server build."""

    package_version: str
    git_sha: str
    profile: str
    build_timestamp: str
    rustc_version: str
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> VersionInfo:
        version = data.get("version") or {}
        return cls(
            package_version=str(version.get("package_version", "")),
            git_sha=str(version.get("git_sha", "")),
            profile=str(version.get("profile", "")),
            build_timestamp=str(version.get("build_timestamp", "")),
            rustc_version=str(version.get("rustc_version", "")),
            raw=data,
        )


@dataclass(frozen=True)
class Readiness:
    """The server's readiness. ``ready`` is False while it starts, drains or misses a dependency."""

    ready: bool
    status: str
    draining: bool
    quality_tier: str | None
    raw: Mapping[str, Any] = field(repr=False, compare=False)

    @classmethod
    def from_response(cls, status_code: int, data: Mapping[str, Any]) -> Readiness:
        quality = data.get("quality")
        return cls(
            ready=status_code == 200,
            status=str(data.get("status", "")),
            draining=bool(data.get("draining")),
            quality_tier=str(quality.get("tier")) if isinstance(quality, Mapping) and "tier" in quality else None,
            raw=data,
        )
