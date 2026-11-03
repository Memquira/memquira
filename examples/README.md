# Examples

Integration examples for the Memquira HTTP API. These are examples, **not** published SDK packages.

| Directory | Language | Notes |
|-----------|----------|-------|
| `curl/` | bash + curl | Shell scripts; needs running API |
| `python/` | Python 3 + requests | Example client class |
| `node/` | Node 18+ fetch | Example client class |

Every example reads `MEMCORE_BASE_URL`, `MEMCORE_API_KEY` and `MEMCORE_ORG_ID` (for the local stack:
`http://127.0.0.1:8080`, `memcore_dev_key`, any organization id). API details: [docs/api-reference.md](../docs/api-reference.md).

## Message ids and retries

The examples send an explicit `session_id` and give every message a stable `message_id` when the
message is created (`new_message` / `newMessage` / `new_id`). A retry resends the same session and
the same messages with the same ids, so stored messages are reused rather than duplicated. Every
new message gets a new id, even if its text repeats an earlier one. Reusing an id for a different
message is rejected with 409 `MESSAGE_ID_CONFLICT`. The examples retry at most once, and only
after `503 REQUEST_DEADLINE_EXCEEDED`; `Retry-After` is a timing hint only. Never send the same
request concurrently. See [docs/api-reference.md](../docs/api-reference.md#error-codes).

Validate without a server:

```bash
./scripts/examples/validate_examples.sh
```
