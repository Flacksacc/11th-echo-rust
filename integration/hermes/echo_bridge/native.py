"""Thin native task operations. Hermes remains the only execution authority.

Use a connection per request and a context-local board pin. Never change the
process-wide HERMES_KANBAN_BOARD while handling concurrent conversations.
"""

from __future__ import annotations

from contextlib import contextmanager
from contextlib import closing
from dataclasses import asdict
from typing import Iterator

from hermes_cli import kanban_db as kb
from hermes_cli.kanban_db_connect import connect_closing


@contextmanager
def board_connection(board: str = "echo") -> Iterator:
    with kb.scoped_current_board(board), connect_closing(board=board) as conn:
        yield conn


def signal_worker_tree(pid: int, sig: int) -> None:
    # archive_task verifies host, claim and process birth fingerprint BEFORE
    # invoking this callback. Use Hermes' own portable descendant cleanup.
    from agent.deadline import kill_process_tree

    if not kill_process_tree(pid, sig=sig):
        if kb._pid_alive(pid):
            raise OSError("Native worker tree could not be terminated")
        raise ProcessLookupError("Native worker tree is no longer running")


def stop_task(task_id: str, board: str = "echo") -> dict:
    with board_connection(board) as conn:
        if kb.get_task(conn, task_id) is None:
            raise LookupError("Task not found")
        kb.archive_task(conn, task_id, signal_fn=signal_worker_tree)
        task = kb.get_task(conn, task_id)
        row = conn.execute(
            "SELECT * FROM task_events WHERE task_id = ? AND kind = ? ORDER BY id DESC LIMIT 1",
            (task_id, "archive_worker_termination"),
        ).fetchone()
        termination = kb.Event.from_row(row).payload if row is not None else None
        # Archived is durable and prevents redispatch. It is not proof that a
        # refused/failed OS termination succeeded; keep that distinction visible.
        stopped = not termination or bool(termination.get("terminated"))
        return {"task": asdict(task), "stopped": stopped, "termination": termination}


def queued_instruction(task_id: str, body: str, board: str = "echo") -> dict:
    if not body.strip():
        raise ValueError("Instruction is empty")
    with board_connection(board) as conn:
        task = kb.get_task(conn, task_id)
        if task is None:
            raise LookupError("Task not found")
        if task.status in {"done", "archived"}:
            raise ValueError("Task is no longer accepting worker instructions")
        comment_id = kb.add_comment(conn, task_id, "echo-user", body)
        # A saved comment is queued, not an acknowledgement by the worker.
        return {"task_id": task_id, "comment_id": comment_id, "status": "queued"}


def task_conversation(task_id: str, board: str = "echo") -> str:
    """Use native subscriptions to bind a task to its own fast conversation.

    Hermes' notifier wakes this native session on terminal/review/block events.
    Echo adds no competing task scheduler or notification history.
    """
    import hashlib
    from hermes_state import SessionDB
    from hermes_cli.kanban_db_notify import add_notify_sub
    with board_connection(board) as conn:
        task = kb.get_task(conn, task_id)
        if task is None:
            raise LookupError("Task not found")
        existing = conn.execute("SELECT chat_id FROM kanban_notify_subs WHERE task_id=? "
                                "AND platform='api_server' ORDER BY created_at DESC LIMIT 1", (task_id,)).fetchone()
        if existing:
            return existing[0]
        # A deterministic native session ID lets concurrent plugin/UI binding
        # converge on the same subscription without a second state store.
        session = "api_echo_" + hashlib.sha256(f"{board}:{task_id}".encode()).hexdigest()[:32]
        with closing(SessionDB()) as db:
            db.ensure_session(session, "api_server")
        add_notify_sub(conn, task_id=task_id, platform="api_server", chat_id=session,
                       notifier_profile="default", delivery_mode="wake")
        return session


def comment_payload(conn, task_id, event_id, created_at, payload):
    """Read the authoritative comment matching a native comment event."""
    ordinal = conn.execute(
        "SELECT COUNT(*) FROM task_events WHERE task_id=? AND kind='commented' "
        "AND created_at=? AND id<=? AND json_extract(payload,'$.author')=?",
        (task_id, created_at, event_id, payload.get("author", ""))).fetchone()[0]
    comment = conn.execute(
        "SELECT id, substr(body,1,4096) AS body FROM task_comments "
        "WHERE task_id=? AND author=? AND created_at=? ORDER BY id LIMIT 1 OFFSET ?",
        (task_id, payload.get("author", ""), created_at, max(0, ordinal - 1))).fetchone()
    return {**payload, "body": comment["body"], "comment_id": comment["id"]} if comment is not None else payload


def event_page(after: int, limit: int = 100, board: str = "echo") -> dict:
    if after < 0 or not 1 <= limit <= 500:
        raise ValueError("Invalid event cursor or page size")
    with board_connection(board) as conn:
        # Presentation read-through only. Native event IDs are authoritative;
        # bounded SQL avoids materializing every event before slicing a page.
        rows = conn.execute(
            "SELECT id,task_id,kind,substr(payload,1,4096) AS payload,created_at,run_id FROM task_events WHERE id > ? ORDER BY id LIMIT ?", (after, limit)
        ).fetchall()
        events = [asdict(kb.Event.from_row(row)) for row in rows]
        for event in events:
            if event["kind"] == "commented":
                # Native comment events contain an author and length, not the
                # comment text. Resolve the authoritative comment separately.
                payload = event.get("payload") or {}
                event["payload"] = comment_payload(conn, event["task_id"], event["id"], event["created_at"], payload)
    return {"events": events, "next_cursor": events[-1]["id"] if events else after}
