import unittest
import uuid

from echo_bridge.history import history_page, BLOCK_BYTES, search_text, text_chunks, history_stamps, history_stamp, task_updates
from hermes_state import SessionDB


class HistoryGate(unittest.TestCase):
    def test_tool_activity_is_compact_without_changing_native_results(self):
        content = "Synthetic large tool output " * 10000
        self.db.append_message(self.session, "user", "Inspect the project")
        self.db.append_message(self.session, "tool", content, tool_name="read_file")
        self.db.append_message(self.session, "assistant", "The project is ready.")
        page = history_page(self.session)
        self.assertEqual(len(page["blocks"]), 3)
        self.assertTrue(page["blocks"][1]["activity"])
        self.assertEqual(page["blocks"][1]["text"], "Finished read_file.")
        self.assertEqual(self.db.get_messages(self.session)[1]["content"], content)
    def setUp(self):
        self.db = SessionDB()
        self.addCleanup(self.db.close)
        self.session = "echo-history-" + uuid.uuid4().hex
        self.db.create_session(self.session, "echo")

    def test_history_and_stable_completion_identity_survive_database_reopen(self):
        self.db.append_message(self.session, "user", "Synthetic restart request")
        self.db.append_message(self.session, "assistant", "Synthetic persisted answer")
        before = history_stamp(self.session)
        page = history_page(self.session, 0, 0, 8)
        self.db.close()
        self.db = SessionDB()
        self.addCleanup(self.db.close)
        after = history_stamp(self.session)
        self.assertEqual(before["message"], after["message"])
        self.assertEqual(page["blocks"], history_page(self.session, 0, 0, 8)["blocks"])
        self.assertEqual(len(self.db.get_messages(self.session)), 2)

    def test_giant_unicode_message_pages_without_loss_or_duplicates(self):
        text = "👩🏽‍💻漢字é" * 2000
        self.db.append_message(self.session, "assistant", text)
        cursor = {"message": 0, "end": 0}
        pages = []
        while cursor is not None:
            self.assertLess(len(pages), 100, "History cursor failed to make progress")
            page = history_page(self.session, cursor["message"], cursor["end"], 3)
            self.assertLessEqual(len(page["blocks"]), 3)
            self.assertTrue(all(len(b["text"].encode()) <= BLOCK_BYTES for b in page["blocks"]))
            pages.append("".join(b["text"] for b in page["blocks"]))
            cursor = page["older"]
        self.assertEqual("".join(reversed(pages)), text)

    def test_all_tab_stamps_bound_preview_and_report_native_turn_lease(self):
        text = "漢字🙂" * 3000
        message = self.db.append_message(self.session, "assistant", text)
        self.assertTrue(self.db.try_acquire_session_turn_lease(self.session, "echo-stamp-fixture", ttl_seconds=60))
        self.addCleanup(self.db.release_session_turn_lease, self.session, "echo-stamp-fixture")
        stamp = history_stamps([self.session])[self.session]
        self.assertEqual(stamp["message"], message)
        self.assertTrue(stamp["thinking"])
        self.assertLessEqual(len(stamp["preview"].encode()), BLOCK_BYTES)
        self.assertTrue(text.startswith(stamp["preview"]))
        self.assertNotIn("�", stamp["preview"])
        self.db.release_session_turn_lease(self.session, "echo-stamp-fixture")
        self.assertFalse(history_stamps([self.session])[self.session]["thinking"])
        with self.assertRaises(ValueError):
            history_stamps([self.session] * 257)

    def test_completion_identity_distinguishes_identical_replies_and_fences_later_notifier(self):
        import time
        self.db.append_message(self.session, "assistant", "The same answer.")
        first = history_stamp(self.session)
        completed_before = time.time()
        time.sleep(0.002)
        self.db.append_message(self.session, "assistant", "The same answer.")
        second = history_stamp(self.session)
        self.assertNotEqual(first["message_uid"], second["message_uid"])
        self.assertEqual(history_stamp(self.session, completed_before)["message_uid"], first["message_uid"])
        with self.assertRaises(ValueError):
            history_stamp(self.session, float("nan"))

    def test_multiple_messages_retain_native_ids_and_empty_rows(self):
        ids = [self.db.append_message(self.session, "user", text) for text in ("first", None, "last")]
        page = history_page(self.session, limit=2)
        self.assertEqual([b["message"] for b in page["blocks"]], ids[-2:])
        older = page["older"]
        page = history_page(self.session, older["message"], older["end"], 2)
        self.assertEqual([b["message"] for b in page["blocks"]], ids[:1])

    def test_search_across_chunk_boundary_and_export_preserve_unicode(self):
        text = "x" * (65536-2) + "漢字👩🏽‍💻 target" + "y" * 80000
        message = self.db.append_message(self.session,"assistant",text)
        result = search_text(self.session,"漢字👩🏽‍💻 target")
        self.assertEqual(len(result["results"]),1)
        self.assertEqual(result["results"][0]["message"],message)
        self.assertEqual(result["results"][0]["offset"],65534)
        exported = b"".join(text_chunks(self.session)).decode()
        self.assertEqual(exported,"\n\nassistant\n"+text)
        self.assertTrue(all(len(chunk) <= 65540 for chunk in text_chunks(self.session)))

    def test_task_updates_page_messages_and_native_continuation_events_without_loss(self):
        from hermes_cli import kanban_db as kb
        from echo_bridge.native import board_connection, task_conversation
        from hermes_cli.kanban_db_notify import add_notify_sub
        board = "echo-timeline-" + uuid.uuid4().hex[:10]
        kb.create_board(board, name="Timeline fixture")
        with board_connection(board) as conn:
            task = kb.create_task(conn, title="Timeline task", initial_status="blocked")
            continuation = kb.create_task(conn, title="Continuation", initial_status="blocked")
        session = task_conversation(task, board)
        with board_connection(board) as conn:
            add_notify_sub(conn, task_id=continuation, platform="api_server", chat_id=session,
                           notifier_profile="default", delivery_mode="wake")
        text = "漢字🙂" * 3000
        message = self.db.append_message(session, "assistant", text)
        # Native order survives a clock moving backwards between messages.
        late_message = self.db.append_message(session, "user", "later native message")
        self.db._execute_write(lambda conn: conn.execute("UPDATE messages SET timestamp=0 WHERE id=?", (late_message,)))
        seen = set()
        parts = []
        cursor = None
        for _ in range(100):
            page = task_updates(continuation, session, board, cursor, limit=2)
            self.assertLessEqual(len(page["blocks"]), 2)
            for block in reversed(page["blocks"]):
                identity = (block["source"], block["message"], block["offset"])
                self.assertNotIn(identity, seen)
                seen.add(identity)
                if block["source"] == "message" and block["message"] == message:
                    parts.append(block["text"])
            cursor = page["older"]
            if cursor is None:
                break
        else:
            self.fail("Task update cursor did not finish")
        self.assertEqual("".join(reversed(parts)), text)
        self.assertTrue(any(identity[0] == "message" and identity[1] == late_message for identity in seen))
        with board_connection(board) as conn:
            native_ids = {row[0] for row in conn.execute("SELECT id FROM task_events WHERE task_id IN (?,?)", (task, continuation))}
        self.assertEqual({identity[1] for identity in seen if identity[0] == "event"}, native_ids)
        with self.assertRaises(ValueError):
            task_updates(task, self.session, board)


if __name__ == "__main__":
    unittest.main(verbosity=2)
