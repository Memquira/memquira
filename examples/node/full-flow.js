#!/usr/bin/env node
/**
 * Create → search → context → delete using the example MemcoreClient.
 * Never prints MEMCORE_API_KEY.
 */

import { randomUUID } from "node:crypto";

import { MemcoreApiError, MemcoreClient, newMessage } from "./memcore-client.js";

function requireEnv(name) {
  const value = (process.env[name] || "").trim();
  if (!value) {
    console.error(`error: missing required environment variable: ${name}`);
    process.exit(1);
  }
  return value;
}

async function main() {
  const baseUrl = requireEnv("MEMCORE_BASE_URL");
  const apiKey = requireEnv("MEMCORE_API_KEY");
  const orgId = requireEnv("MEMCORE_ORG_ID");
  const userId = (process.env.MEMCORE_USER_ID || "user_demo").trim() || "user_demo";

  const client = new MemcoreClient({ baseUrl, apiKey, orgId });

  // An explicit session, and each message created once with its own stable message_id.
  const sessionId = `session_${randomUUID()}`;
  const messages = [
    newMessage("user", "User prefers concise technical summaries."),
    newMessage("assistant", "Noted."),
  ];

  try {
    let created;
    try {
      created = await client.createMemory({ userId, sessionId, messages });
    } catch (err) {
      if (!(err instanceof MemcoreApiError) || err.code !== "REQUEST_DEADLINE_EXCEEDED") {
        throw err;
      }
      // The timed-out request may have been partly stored. Retry once, later, with the SAME
      // session and message objects (same message_ids): stored messages are reused, not
      // duplicated. Retry-After is only a timing hint. Never send the same request concurrently.
      await new Promise((resolve) => setTimeout(resolve, Number(err.retryAfter || 1) * 1000));
      created = await client.createMemory({ userId, sessionId, messages });
    }
    const memories = created.memories || [];
    const memoryId = memories[0]?.id;
    console.log(`create status=${created.status} memories=${memories.length}`);

    const search = await client.searchMemories({
      userId,
      query: "technical summaries",
      limit: 5,
    });
    console.log(`search results=${(search.results || []).length}`);

    const context = await client.buildContext({
      userId,
      query: "How should replies be written?",
      maxTokens: 1000,
    });
    console.log(
      `context status=${context.status} chars=${(context.context || "").length}`,
    );

    if (memoryId) {
      const deleted = await client.deleteMemory({ userId, memoryId });
      console.log(`delete status=${deleted.status}`);
    } else {
      console.error("warning: no memory id returned; skipping delete");
    }
  } catch (error) {
    if (error instanceof MemcoreApiError) {
      console.error(String(error));
      process.exit(1);
    }
    throw error;
  }
}

main();
