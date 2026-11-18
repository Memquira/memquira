"""Exceptions raised by the Memquira client."""

from __future__ import annotations

from typing import Any


class MemquiraError(Exception):
    """Base class for every error raised by this package."""


class TransportError(MemquiraError):
    """The request failed before an HTTP response arrived (connection, timeout, protocol)."""


class APIError(MemquiraError):
    """The server answered with an error status.

    ``code`` is the server's stable error code (for example ``MESSAGE_ID_CONFLICT``), ``request_id`` identifies
    the request in the server logs, and ``retry_after`` is the server's ``Retry-After`` hint in seconds, if any.
    """

    def __init__(
        self,
        status_code: int,
        *,
        code: str | None = None,
        message: str | None = None,
        request_id: str | None = None,
        details: Any = None,
        retry_after: float | None = None,
        body: Any = None,
    ) -> None:
        self.status_code = status_code
        self.code = code
        self.message = message
        self.request_id = request_id
        self.details = details
        self.retry_after = retry_after
        self.body = body
        text = f"HTTP {status_code}"
        if code:
            text += f" {code}"
        if message:
            text += f": {message}"
        if request_id:
            text += f" (request_id={request_id})"
        super().__init__(text)


class BadRequestError(APIError):
    """400: the request was invalid."""


class AuthenticationError(APIError):
    """401: the API key is missing, unknown or revoked."""


class PermissionDeniedError(APIError):
    """403: the key lacks a scope for this operation, or the organization does not match the key."""


class NotFoundError(APIError):
    """404: the memory or resource does not exist for this user."""


class ConflictError(APIError):
    """409: for example a ``message_id`` reused for a different message (``MESSAGE_ID_CONFLICT``)."""


class RateLimitError(APIError):
    """429: a rate limit or quota was hit. ``retry_after`` says when to try again."""


class ServerError(APIError):
    """5xx: the server or one of its model providers failed."""


class ServiceUnavailableError(ServerError):
    """503: the server is overloaded, draining or still recovering an earlier write. Safe to retry later."""


_BY_STATUS: dict[int, type[APIError]] = {
    400: BadRequestError,
    401: AuthenticationError,
    403: PermissionDeniedError,
    404: NotFoundError,
    409: ConflictError,
    429: RateLimitError,
    503: ServiceUnavailableError,
}


def error_class_for_status(status_code: int) -> type[APIError]:
    if status_code in _BY_STATUS:
        return _BY_STATUS[status_code]
    if status_code >= 500:
        return ServerError
    return APIError
