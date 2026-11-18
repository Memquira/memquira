# Memquira Python client

A small, typed client for the [Memquira](https://github.com/Memquira/memquira) memory API, with synchronous and
asynchronous versions. It covers the operations an application or agent needs:

- store messages;
- search memories;
- build a prompt context;
- list memories;
- delete one memory;
- erase a user.

It is not on PyPI yet. Install it from a checkout of this repository:

```bash
pip install ./clients/python
```

Python 3.10 or newer; the only dependency is [`httpx`](https://www.python-httpx.org/).

## Quick start

Start a local server first; the [Quick start](../../README.md#quick-start) runs one with mock models, no keys and
no cost.

```python
from memquira import Memquira

with Memquira("http://127.0.0.1:8080", api_key="memcore_dev_key", org_id="demo") as client:
    alice = client.for_user("alice")

    result = alice.add("I am vegetarian and I live in Lisbon.", session_id="chat-1")
    print(result.summary.added, [m.content for m in result.memories])

    for hit in alice.search("vegetarian Lisbon", limit=5):
        print(f"{hit.score:.3f}  {hit.content}")

    context = alice.context("Suggest a restaurant for tonight", token_budget=500)
    print(context.text)  # ready to place in a system prompt
```

`memcore_dev_key` is the fixed key of the local development stack, which uses `dev` authentication. Never use it
anywhere else. With database authentication, create an application key with only the scopes it needs, as described
in [Getting started](../../docs/getting-started.md#use-a-real-model).

`base_url`, `api_key` and `org_id` can also come from `MEMQUIRA_URL`, `MEMQUIRA_API_KEY` and `MEMQUIRA_ORG_ID`:

```python
client = Memquira()  # reads the three environment variables
```

### Async

```python
from memquira import AsyncMemquira

async with AsyncMemquira() as client:
    alice = client.for_user("alice")
    await alice.add("I prefer meetings after 2pm.")
    context = await alice.context("Schedule a call with this user")
```

## Operations

| Method | Endpoint | Scope |
|---|---|---|
| `add(user_id, messages, *, session_id, session_timestamp, metadata)` | `POST /api/v1/memories` | `MemoryWrite` |
| `search(user_id, query, *, limit, memory_types, include_history, retrieval_mode)` | `POST /api/v1/memories/search` | `MemoryRead` |
| `context(user_id, query, *, token_budget, include_items, packing, exclude_message_ids)` | `POST /api/v1/memories/context` | `MemoryRead` |
| `list_memories(user_id, *, limit, cursor, memory_type, query)`, `iter_memories(...)` | `GET /api/v1/users/{user_id}/memories` | `MemoryRead` |
| `delete(user_id, memory_id)` | `DELETE /api/v1/users/{user_id}/memories/{memory_id}` | `MemoryDelete` |
| `forget_user(user_id)` | `DELETE /api/v1/users/{user_id}` | `UserDelete` |
| `version()`, `ready()` | `GET /api/v1/version`, `GET /health/ready` | none |

Results are typed dataclasses (`AddResult`, `SearchHit`, `MemoryContext`, `MemoryPage`, ...). Each keeps the
server's full JSON in `raw`, so fields added by newer servers are still available. The
[API reference](../../docs/api-reference.md) describes every field.

`add` runs the server's language model to extract memories, so it takes seconds, not milliseconds. In a chat
application, store the finished turn after replying, not before.

## Messages and safe retries

`messages` can be a string (one user message), a `Message`, a dict, or a list of them. A `Message` or a string
gets a unique `message_id` when it is created:

```python
from memquira import Message

turn = [Message("user", "We moved the launch to March."), Message("assistant", "Noted.")]
client.add("alice", turn, session_id="chat-7")
```

The server stores a message with a given `message_id` once. Sending the same `Message` objects again is a safe
retry: the server answers with the first call's result and stores nothing new. Reusing an id for a different message is rejected with `ConflictError` (`MESSAGE_ID_CONFLICT`).

Do not send the same messages from two places at once; resend them only after the first attempt has failed.

The client retries automatically, up to `max_retries` times (default 2):

- **429:** always, after the server's `Retry-After`.
- **503:** only for requests that are safe to repeat. Reads and deletes are always safe (deleting something
  already deleted succeeds again), and `add` is safe when every message has a `message_id`.
- **Connection failures before the request was sent:** always.
- **Timeouts and dropped connections after sending:** only for requests that are safe to repeat.

The client never waits longer than `max_retry_wait` seconds (default 30). A longer `Retry-After` is raised as
`RateLimitError` with `retry_after` set.

## Errors

| Exception | When |
|---|---|
| `BadRequestError` | 400: invalid request |
| `AuthenticationError` | 401: missing, unknown or revoked key |
| `PermissionDeniedError` | 403: missing scope or wrong organization |
| `NotFoundError` | 404: no such memory for this user |
| `ConflictError` | 409: for example `MESSAGE_ID_CONFLICT` |
| `RateLimitError` | 429: rate limit or quota; see `retry_after` |
| `ServiceUnavailableError` | 503: overloaded, draining or recovering; retry later |
| `ServerError` | other 5xx, including model-provider failures (502) |
| `TransportError` | no HTTP response: connection refused, timeout |

All of them derive from `MemquiraError`. `APIError` subclasses carry `status_code`, `code`, `message`,
`request_id` and `details`.

## Using it in an agent

Memquira keys belong to an organization, not to an end user. Whoever chooses `user_id` can read that user's
memories, so:

- Take `user_id` from your application's authenticated session, never from a model's tool arguments. Hand the
  model a `client.for_user(user_id)` view, whose user cannot be changed.
- Give an agent's key only `MemoryRead` and `MemoryWrite`. Add `MemoryDelete` only if the agent may delete
  memories. Never give it `UserDelete` or admin scopes, and do not expose `forget_user` as a tool.

## Development

```bash
cd clients/python
pip install -e ".[test]"
pytest                      # unit tests against a mocked transport
ruff check . && ruff format --check . && mypy
```

`tests/test_e2e_local.py` runs the client against a real server. It is skipped unless `MEMQUIRA_E2E_URL`,
`MEMQUIRA_E2E_API_KEY` and `MEMQUIRA_E2E_ORG_ID` are set. Point it at a server with mock models, such as the
local Docker stack. It writes and then erases its own test users.
