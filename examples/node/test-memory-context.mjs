// End-to-end check of memoryContext / multiParticipantContext against a running memcore.
// Needs MEMCORE_E2E_BASE_URL (a disposable local server with episodes on); skipped otherwise.
// Invented data. Run: node test-memory-context.mjs
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { MemcoreClient, newMessage } from "./memcore-client.js";

const baseUrl = process.env.MEMCORE_E2E_BASE_URL;
if (!baseUrl) {
  console.log("skipped: MEMCORE_E2E_BASE_URL not set");
  process.exit(0);
}
const client = new MemcoreClient({
  baseUrl,
  apiKey: process.env.MEMCORE_E2E_API_KEY ?? "local-dev-key",
  orgId: process.env.MEMCORE_E2E_ORG_ID ?? `org_e2e_${randomUUID().slice(0, 8)}`,
});
const users = [`user_a_${randomUUID().slice(0, 6)}`, `user_b_${randomUUID().slice(0, 6)}`];
const messages = [
  newMessage("user", "Asha: I moved to Porto last spring and I love the river walks."),
  newMessage("user", "Ben: I still live in Lisbon, but I visit Porto every month."),
];
for (const userId of users) {
  await client.createMemory({ userId, sessionId: "s_node", messages });
}
const query = "Where do Asha and Ben live?";
const plain = await client.memoryContext({ userId: users[0], query });
assert.equal(plain.items, undefined, "no options sent by default");
const { contexts, excludedMessageIds } = await client.multiParticipantContext({ userIds: users, query });
const eps = (ctx) => new Set(ctx.items.filter((i) => i.kind === "episode").map((i) => i.message_id));
const [first, second] = contexts.map(eps);
assert.ok(first.size > 0);
for (const id of first) assert.ok(!second.has(id), "no message in both contexts");
assert.equal(contexts[0].text, plain.text);
assert.deepEqual([...excludedMessageIds].sort(), [...new Set([...first, ...second])].sort());
const alone = await client.memoryContext({ userId: users[1], query, includeItems: true });
const factIds = (ctx) => new Set(ctx.items.filter((i) => i.kind === "fact").map((i) => i.id));
for (const id of factIds(alone)) assert.ok(factIds(contexts[1]).has(id), "facts are never excluded");
console.log("ok: node memoryContext / multiParticipantContext");
