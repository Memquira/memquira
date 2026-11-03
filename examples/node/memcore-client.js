/**
 * Example memcore HTTP client (not a published SDK).
 * Uses Node 18+ global fetch. Never logs the API key.
 */

import { randomUUID } from "node:crypto";

/**
 * Create a message once, with its own stable `message_id`. Keep the returned object and send it
 * unchanged on a retry: the same `message_id` with the same message is a retry, not a new message.
 * Every new message (even one repeating earlier text) gets a new id; reusing an id for a different
 * message is rejected with 409 `MESSAGE_ID_CONFLICT`.
 */
export function newMessage(role, content, { name } = {}) {
  const message = { role, content, message_id: `msg_${randomUUID()}` };
  if (name) {
    message.name = name;
  }
  return message;
}

export class MemcoreApiError extends Error {
  constructor(status, payload, retryAfter = null) {
    const err = payload && typeof payload === "object" ? payload.error || {} : {};
    const code = err.code ? ` code=${err.code}` : "";
    const requestId = err.request_id ? ` request_id=${err.request_id}` : "";
    const message = err.message ? `: ${err.message}` : "";
    super(`memcore HTTP ${status}${code}${requestId}${message}`);
    this.name = "MemcoreApiError";
    this.status = status;
    this.code = err.code || null;
    this.retryAfter = retryAfter;
    this.payload = payload;
  }
}

export class MemcoreClient {
  constructor({ baseUrl, apiKey, orgId, timeoutMs = 30_000 }) {
    if (!baseUrl || !apiKey || !orgId) {
      throw new Error("baseUrl, apiKey, and orgId are required");
    }
    this.baseUrl = String(baseUrl).replace(/\/$/, "");
    this._apiKey = apiKey;
    this.orgId = orgId;
    this.timeoutMs = timeoutMs;
  }

  _headers() {
    return {
      Authorization: `Bearer ${this._apiKey}`,
      "X-Organization-ID": this.orgId,
      "Content-Type": "application/json",
      Accept: "application/json",
    };
  }

  async _request(method, path, { query, body } = {}) {
    const url = new URL(`${this.baseUrl}${path}`);
    if (query) {
      for (const [key, value] of Object.entries(query)) {
        if (value !== undefined && value !== null && value !== "") {
          url.searchParams.set(key, String(value));
        }
      }
    }

    const response = await fetch(url, {
      method,
      headers: this._headers(),
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });

    let payload;
    try {
      payload = await response.json();
    } catch {
      payload = { raw: "(non-JSON body)" };
    }

    if (!response.ok) {
      throw new MemcoreApiError(response.status, payload, response.headers.get("Retry-After"));
    }
    return payload;
  }

  /**
   * Add messages made with `newMessage` to `sessionId`. Message ids are unique per organization,
   * user and session, so a retry must send the same `sessionId` and the same message objects.
   */
  createMemory({ userId, sessionId, messages }) {
    return this._request("POST", "/api/v1/memories", {
      body: {
        user_id: userId,
        session_id: sessionId,
        messages,
        metadata: { source: "examples/node" },
      },
    });
  }

  searchMemories({ userId, query, limit = 5 }) {
    return this._request("POST", "/api/v1/memories/search", {
      body: { user_id: userId, query, limit },
    });
  }

  buildContext({ userId, query, maxTokens = 1000 }) {
    return this._request("POST", "/api/v1/context", {
      body: { user_id: userId, query, max_tokens: maxTokens },
    });
  }

  /**
   * The server-side context of one memory user (`POST /api/v1/memories/context`). Only the options given
   * are sent, so a call without them gets the server's validated default. `excludeMessageIds`
   * leaves out the episodes of those client message ids (facts are never removed).
   */
  memoryContext({ userId, query, tokenBudget, includeItems = false, packing, excludeMessageIds }) {
    const body = { user_id: userId, query };
    if (tokenBudget !== undefined) body.token_budget = tokenBudget;
    if (includeItems) body.include_items = true;
    if (packing !== undefined) body.packing = packing;
    if (excludeMessageIds && excludeMessageIds.length > 0) {
      body.exclude_message_ids = [...excludeMessageIds];
    }
    return this._request("POST", "/api/v1/memories/context", { body });
  }

  /**
   * Contexts of several memory users for one prompt, without repeating a message: users are
   * fetched in order and the message ids of episodes already returned are excluded from the later
   * users' contexts. Facts are never excluded. Resolves to `{ contexts, excludedMessageIds }`.
   */
  async multiParticipantContext({ userIds, query, tokenBudget, packing }) {
    const seen = [];
    const contexts = [];
    for (const userId of userIds) {
      const context = await this.memoryContext({
        userId,
        query,
        tokenBudget,
        includeItems: true,
        packing,
        excludeMessageIds: seen,
      });
      contexts.push(context);
      for (const item of context.items ?? []) {
        if (item.kind === "episode" && item.message_id && !seen.includes(item.message_id)) {
          seen.push(item.message_id);
        }
      }
    }
    return { contexts, excludedMessageIds: seen };
  }

  listMemories({ userId, limit = 50, cursor }) {
    return this._request("GET", `/api/v1/users/${encodeURIComponent(userId)}/memories`, {
      query: { limit, cursor },
    });
  }

  deleteMemory({ userId, memoryId }) {
    return this._request(
      "DELETE",
      `/api/v1/users/${encodeURIComponent(userId)}/memories/${encodeURIComponent(memoryId)}`,
    );
  }

  exportUser({ userId }) {
    return this._request("GET", `/api/v1/users/${encodeURIComponent(userId)}/export`, {
      query: { include_events: "true" },
    });
  }

  importUser({ userId, payload, dryRun = false }) {
    return this._request("POST", `/api/v1/users/${encodeURIComponent(userId)}/import`, {
      body: { ...payload, dry_run: dryRun },
    });
  }
}
