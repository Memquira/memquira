#!/usr/bin/env python3
"""Create → search → context → delete using the example MemcoreClient."""

from __future__ import annotations

import os
import sys
import time
import uuid

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from memcore_client import MemcoreApiError, MemcoreClient, new_message


def require_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        print(f"error: missing required environment variable: {name}", file=sys.stderr)
        sys.exit(1)
    return value


def main() -> int:
    base_url = require_env("MEMCORE_BASE_URL")
    api_key = require_env("MEMCORE_API_KEY")
    org_id = require_env("MEMCORE_ORG_ID")
    user_id = os.environ.get("MEMCORE_USER_ID", "user_demo").strip() or "user_demo"

    client = MemcoreClient(base_url, api_key, org_id)

    # An explicit session, and each message created once with its own stable message_id.
    session_id = f"session_{uuid.uuid4().hex}"
    messages = [
        new_message("user", "User prefers concise technical summaries."),
        new_message("assistant", "Noted."),
    ]

    try:
        try:
            created = client.create_memory(user_id, session_id, messages)
        except MemcoreApiError as exc:
            if exc.code != "REQUEST_DEADLINE_EXCEEDED":
                raise
            # The timed-out request may have been partly stored. Retry once, later, with the SAME
            # session and message dicts (same message_ids): stored messages are reused, not
            # duplicated. Retry-After is only a timing hint. Never send the same request
            # concurrently.
            time.sleep(float(exc.retry_after or 1))
            created = client.create_memory(user_id, session_id, messages)
        memories = created.get("memories") or []
        memory_id = memories[0]["id"] if memories else None
        print(f"create status={created.get('status')} memories={len(memories)}")

        search = client.search_memories(user_id, "technical summaries", limit=5)
        print(f"search results={len(search.get('results') or [])}")

        context = client.build_context(
            user_id, "How should replies be written?", max_tokens=1000
        )
        print(
            f"context status={context.get('status')} "
            f"chars={len(context.get('context') or '')}"
        )

        if memory_id:
            deleted = client.delete_memory(user_id, memory_id)
            print(f"delete status={deleted.get('status')}")
        else:
            print("warning: no memory id returned; skipping delete", file=sys.stderr)
    except MemcoreApiError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
