"""End-to-end check of ``memory_context`` / ``multi_participant_context`` against a running memcore.

Needs ``MEMCORE_E2E_BASE_URL`` (a disposable local server with episodes on, e.g. mock providers);
skipped otherwise. ``MEMCORE_E2E_API_KEY`` / ``MEMCORE_E2E_ORG_ID`` default to placeholders for a
server with auth off. Invented data. Run: ``python -m unittest test_memory_context``.
"""

from __future__ import annotations

import os
import unittest
import uuid

from memcore_client import MemcoreClient, new_message

BASE_URL = os.environ.get("MEMCORE_E2E_BASE_URL", "")


@unittest.skipUnless(BASE_URL, "MEMCORE_E2E_BASE_URL not set")
class MemoryContextE2E(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = MemcoreClient(
            BASE_URL,
            os.environ.get("MEMCORE_E2E_API_KEY", "local-dev-key"),
            os.environ.get("MEMCORE_E2E_ORG_ID", f"org_e2e_{uuid.uuid4().hex[:8]}"),
        )
        # One conversation stored for both participants (the per-participant memory protocol):
        # every message is an episode of both users, with the same message ids.
        cls.users = [f"user_a_{uuid.uuid4().hex[:6]}", f"user_b_{uuid.uuid4().hex[:6]}"]
        cls.messages = [
            new_message("user", "Asha: I moved to Porto last spring and I love the river walks."),
            new_message("user", "Ben: I still live in Lisbon, but I visit Porto every month."),
            new_message("user", "Asha: My sister Mei is starting a bakery near the river."),
        ]
        session = f"s_{uuid.uuid4().hex[:8]}"
        for user in cls.users:
            cls.client.create_memory(user, session, cls.messages)
        cls.query = "Where do Asha and Ben live?"

    def test_default_call_sends_no_options_and_keeps_the_reference_behaviour(self) -> None:
        plain = self.client.memory_context(self.users[0], self.query)
        explicit_empty = self.client.memory_context(self.users[0], self.query, exclude_message_ids=[])
        self.assertEqual(plain["text"], explicit_empty["text"])
        self.assertNotIn("items", plain)

    def test_episodes_already_returned_are_not_repeated_and_facts_stay(self) -> None:
        separate = [
            self.client.memory_context(u, self.query, include_items=True) for u in self.users
        ]
        combined = self.client.multi_participant_context(self.users, self.query)
        first, second = combined["contexts"]
        first_eps = {i["message_id"] for i in first["items"] if i["kind"] == "episode" and i.get("message_id")}
        second_eps = {i["message_id"] for i in second["items"] if i["kind"] == "episode" and i.get("message_id")}
        self.assertTrue(first_eps, "the first participant's context holds episodes")
        self.assertFalse(first_eps & second_eps, "no message appears in both contexts")
        self.assertEqual(sorted(combined["excluded_message_ids"]), sorted(first_eps | second_eps))
        # The first participant's context is exactly the plain one; facts of the second are kept.
        self.assertEqual(first["text"], separate[0]["text"])
        facts_alone = {i["id"] for i in separate[1]["items"] if i["kind"] == "fact"}
        facts_combined = {i["id"] for i in second["items"] if i["kind"] == "fact"}
        self.assertTrue(facts_alone <= facts_combined, "exclusion never removes a fact")
        self.assertGreaterEqual(second.get("excluded_candidates", 0), len(first_eps & {
            i["message_id"] for i in separate[1]["items"] if i["kind"] == "episode" and i.get("message_id")
        }))
        # Provenance: every episode carries its client message id (facts carry
        # ``source_message_ids`` when the extraction recorded them; omitted when empty).
        for item in first["items"] + second["items"]:
            if item["kind"] == "episode":
                self.assertTrue(item.get("message_id"))


if __name__ == "__main__":
    unittest.main()
