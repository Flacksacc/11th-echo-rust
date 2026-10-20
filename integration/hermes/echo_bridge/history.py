"""Bounded read-through presentation of native Hermes message TEXT blobs.

SQLite's incremental blob reader can seek inside a giant message without
materializing it or repeatedly scanning its UTF-8 prefix with SQL substr().
This module never writes the native database or duplicates execution state.
"""

from __future__ import annotations

from contextlib import closing
import codecs
import sqlite3
import time
import json

BLOCK_BYTES = 4096


def session_source(session_id):
    from hermes_state import SessionDB
    with closing(SessionDB(read_only=True)) as db:
        if db.get_session(session_id) is None:
            raise LookupError("Session not found")
        tip = db.resolve_resume_session_id(session_id)
        return db.db_path, db.get_compression_lineage(tip)


def _stamp_on_connection(conn, lineage, completed_before=None):
    marks = ",".join("?" for _ in lineage)
    fence = " AND role='assistant' AND timestamp<=?" if completed_before is not None else ""
    args = (*lineage, completed_before) if completed_before is not None else lineage
    row = conn.execute(f"SELECT id, role, message_uid FROM messages WHERE session_id IN ({marks}) "
                           "AND (active=1 OR compacted=1) AND COALESCE(display_kind,'')!='hidden' "
                           + fence + " ORDER BY id DESC LIMIT 1", args).fetchone()
    thinking = conn.execute(f"SELECT 1 FROM session_turn_leases WHERE conversation_id IN ({marks}) "
                            "AND expires_at > ? LIMIT 1", (*lineage, time.time())).fetchone() is not None
    preview = ""
    if row and row[1] == "assistant":
        try:
            with conn.blobopen("messages", "content", row[0], readonly=True) as blob:
                decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
                preview = decoder.decode(blob.read(BLOCK_BYTES), final=len(blob) <= BLOCK_BYTES)
        except sqlite3.OperationalError:
            pass
        if preview.startswith("\x00json:"):
            preview = ""  # Structured storage is rendered by history, never spoken as raw JSON.
    return {"session_id":lineage[-1], "message":row[0] if row else 0,
            "message_uid": row[2] if row else "", "role":row[1] if row else "", "thinking":thinking, "preview":preview}


def history_stamp(session_id, completed_before=None):
    if completed_before is None:
        return history_stamps([session_id])[session_id]
    import math
    if not math.isfinite(completed_before) or completed_before < 0:
        raise ValueError("Invalid completion timestamp")
    path, lineage = session_source(session_id)
    with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)) as conn:
        return _stamp_on_connection(conn, lineage, completed_before)


def history_stamps(sessions):
    """One bounded metadata read for all tabs, including native notifier turns."""
    if not isinstance(sessions, list) or len(sessions) > 256 or any(
            not isinstance(session, str) or not session or len(session) > 256 for session in sessions):
        raise ValueError("Invalid conversation metadata page")
    from hermes_state import SessionDB
    result = {}
    with closing(SessionDB(read_only=True)) as db:
        with closing(sqlite3.connect(db.db_path.as_uri() + "?mode=ro", uri=True)) as conn:
            for session in dict.fromkeys(sessions):
                if db.get_session(session) is None:
                    continue
                tip = db.resolve_resume_session_id(session)
                result[session] = _stamp_on_connection(conn, db.get_compression_lineage(tip))
    return result


def text_chunks(session_id):
    """Stream the authoritative source without retaining a full conversation."""
    path, lineage = session_source(session_id)
    marks = ",".join("?" for _ in lineage)
    with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, check_same_thread=False)) as conn:
        cursor = conn.execute(f"SELECT id, role FROM messages WHERE session_id IN ({marks}) "
                              "AND (active=1 OR compacted=1) AND COALESCE(display_kind,'')!='hidden' ORDER BY id", lineage)
        for message, role in cursor:
            yield f"\n\n{role}\n".encode()
            try:
                blob = conn.blobopen("messages", "content", message, readonly=True)
            except sqlite3.OperationalError:
                continue
            with blob:
                decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
                while data := blob.read(64 * 1024):
                    yield decoder.decode(data).encode()
                yield decoder.decode(b"", final=True).encode()


def search_text(session_id, needle, limit=20):
    """Literal, case-sensitive search with bounded previews and scan memory."""
    if not isinstance(needle,str) or not needle or len(needle.encode()) > 512:
        raise ValueError("Search text must contain 1 to 512 bytes")
    if not 1 <= limit <= 20:
        raise ValueError("Invalid search result limit")
    path, lineage = session_source(session_id)
    marks = ",".join("?" for _ in lineage)
    pattern = needle.encode()
    results = []
    with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)) as conn:
        rows = conn.execute(f"SELECT id FROM messages WHERE session_id IN ({marks}) "
                            "AND (active=1 OR compacted=1) AND COALESCE(display_kind,'')!='hidden' ORDER BY id DESC", lineage)
        for (message,) in rows:
            try:
                blob = conn.blobopen("messages", "content", message, readonly=True)
            except sqlite3.OperationalError:
                continue
            with blob:
                carry = b""; offset = 0
                while data := blob.read(64 * 1024):
                    window = carry + data
                    start = window.find(pattern)
                    if start >= 0:
                        results.append({"message": message, "offset": offset-len(carry)+start,
                                        "preview": window[max(0,start-100):start+len(pattern)+200].decode("utf-8",errors="replace")})
                        if len(results) == limit:
                            return {"results":results,"limited":True}
                        break
                    offset += len(data)
                    carry = window[-(len(pattern)-1):] if len(pattern) > 1 else b""
    return {"results":results,"limited":False}


def history_page(session_id: str, message_id: int = 0, end_offset: int = 0, limit: int = 120) -> dict:
    from hermes_state import SessionDB

    if message_id < 0 or end_offset < 0 or not 1 <= limit <= 120:
        raise ValueError("Invalid history cursor or page size")
    # Resolve native compression lineage, ownership, and path using Hermes.
    with closing(SessionDB(read_only=True)) as db:
        if db.get_session(session_id) is None:
            raise LookupError("Session not found")
        tip = db.resolve_resume_session_id(session_id)
        lineage = db.get_compression_lineage(tip)
        path = db.db_path
    marks = ",".join("?" for _ in lineage)
    blocks = []
    cursor = None
    with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)) as conn:
        conn.row_factory = sqlite3.Row
        # Only metadata, never the content column. A page is bounded even if
        # the latest conversation contains one gigabyte-long paragraph.
        ceiling = message_id if message_id else 2**63 - 1
        rows = conn.execute(
            f"SELECT id, role, timestamp, substr(tool_name,1,128) AS tool_name, COALESCE(display_kind,'') AS display_kind FROM messages "
            f"WHERE session_id IN ({marks}) AND id <= ? "
            "AND (active = 1 OR compacted = 1) AND COALESCE(display_kind,'') != 'hidden' "
            "ORDER BY id DESC LIMIT 120", (*lineage, ceiling),
        ).fetchall()
        for row in rows:
            if row["role"] == "tool":
                name = row["tool_name"] or "tool"
                blocks.append({"message": row["id"], "offset": 0, "end": 0,
                               "text": f"Finished {name}.", "role": "assistant", "activity": True,
                               "first": True, "last": True})
                if len(blocks) >= limit:
                    cursor = {"message": row["id"] - 1, "end": 0} if row["id"] > 1 else None
                    break
                continue
            try:
                blob = conn.blobopen("messages", "content", row["id"], readonly=True)
            except sqlite3.OperationalError:
                # NULL/scalar content has no text blob; preserve a visible row.
                blocks.append({"message": row["id"], "offset": 0, "end": 0,
                               "text": "", "role": row["role"], "first": True, "last": True})
                if len(blocks) >= limit:
                    cursor = {"message": row["id"] - 1, "end": 0} if row["id"] > 1 else None
                    break
                continue
            with blob:
                total = len(blob)
                end = min(end_offset, total) if row["id"] == message_id and end_offset else total
                if end == 0:
                    blocks.append({"message": row["id"], "offset": 0, "end": 0,
                                   "text": "", "role": row["role"], "first": True, "last": True})
                    if len(blocks) >= limit:
                        cursor = {"message": row["id"] - 1, "end": 0} if row["id"] > 1 else None
                        break
                while end:
                    start = max(0, end - BLOCK_BYTES)
                    blob.seek(start)
                    data = blob.read(end - start)
                    # A cursor produced here always ends at a UTF-8 boundary.
                    # Advance a start cutting through a multi-byte scalar.
                    skip = 0
                    while skip < len(data) and data[skip] & 0xC0 == 0x80:
                        skip += 1
                    if skip == len(data):
                        skip = 0  # malformed imported bytes must still advance
                    start += skip
                    text = data[skip:].decode("utf-8", errors="replace")
                    blocks.append({"message": row["id"], "offset": start, "end": end,
                                   "text": text, "role": row["role"],
                                   "first": start == 0, "last": end == total})
                    end = start
                    if len(blocks) == limit:
                        cursor = ({"message": row["id"], "end": end} if end else
                                  {"message": row["id"] - 1, "end": 0} if row["id"] > 1 else None)
                        break
            if len(blocks) >= limit:
                break
        if cursor is None and len(rows) == 120:
            cursor = {"message": rows[-1]["id"] - 1, "end": 0} if rows[-1]["id"] > 1 else None
    times = {row["id"]: row["timestamp"] for row in rows}
    for block in blocks:
        block["timestamp"] = times[block["message"]]
    blocks.reverse()
    return {"session_id": tip, "blocks": blocks, "older": cursor}


def task_updates(task_id, session_id, board="echo", cursor=None, limit=120):
    """Merge two authoritative native sources with independent stable cursors.

    Continuations share the native conversation; include every task subscribed
    to it. Only bounded message fragments and event previews are loaded.
    """
    from .native import board_connection, comment_payload
    if not 1 <= limit <= 120 or (cursor is not None and not isinstance(cursor, dict)):
        raise ValueError("Invalid task update cursor")
    cursor = cursor or {}
    message_cursor = cursor.get("messages")
    event_cursor = cursor.get("events")
    if message_cursor is not None and message_cursor is not False and not isinstance(message_cursor, dict):
        raise ValueError("Invalid conversation cursor")
    if event_cursor is not None and event_cursor is not False and (type(event_cursor) is not int or event_cursor < 0):
        raise ValueError("Invalid event cursor")
    with board_connection(board) as conn:
        if conn.execute("SELECT 1 FROM tasks WHERE id=?", (task_id,)).fetchone() is None:
            raise LookupError("Task not found")
        # Validate the caller's session against native task subscriptions.
        if conn.execute("SELECT 1 FROM kanban_notify_subs WHERE task_id=? AND chat_id=? AND platform='api_server'",
                        (task_id, session_id)).fetchone() is None:
            raise ValueError("Conversation does not belong to this task")
        events = [] if event_cursor is False else conn.execute(
            "SELECT id,task_id,kind,substr(payload,1,4096) AS payload,created_at FROM task_events "
            "WHERE task_id IN (SELECT task_id FROM kanban_notify_subs WHERE chat_id=? AND platform='api_server') "
            "AND id < ? AND kind NOT IN ('heartbeat','heartbeat_updated') ORDER BY id DESC LIMIT 120",
            (session_id, event_cursor if event_cursor is not None else 2**63-1)).fetchall()
        events = [dict(event) for event in events]
        for event in events:
            if event["kind"] == "commented":
                try:
                    payload = json.loads(event["payload"] or "{}")
                except (ValueError, TypeError):
                    payload = {}
                event["payload"] = json.dumps(comment_payload(conn, event["task_id"], event["id"], event["created_at"], payload))
    messages = {"blocks": [], "older": None} if message_cursor is False else history_page(
        session_id, (message_cursor or {}).get("message", 0), (message_cursor or {}).get("end", 0), 120)
    candidates = []
    for block in messages["blocks"]:
        candidates.append({**block, "source": "message"})
    for event in events:
        try:
            payload = json.loads(event["payload"] or "{}")
        except (ValueError, TypeError):
            payload = {}
        text = next((payload[key] for key in ("body", "summary", "result", "error", "reason")
                     if isinstance(payload.get(key), str)), event["kind"].replace("_", " "))
        text = text.encode("utf-8")[:BLOCK_BYTES].decode("utf-8", errors="ignore")
        candidates.append({"source": "event", "message": event["id"], "offset": 0,
                           "timestamp": event["created_at"], "text": text,
                           "role": "assistant", "first": True, "last": True})
    # Timestamps can regress. Preserve native ID/fragment order within each
    # source, choosing only between the two heads when interleaving them.
    pending_messages = list(reversed([block for block in candidates if block["source"] == "message"]))
    pending_events = [block for block in candidates if block["source"] == "event"]
    message_index = event_index = 0
    selected = []
    while len(selected) < limit and (message_index < len(pending_messages) or event_index < len(pending_events)):
        take_message = event_index == len(pending_events) or (message_index < len(pending_messages)
            and (pending_messages[message_index]["timestamp"] or 0) >= (pending_events[event_index]["timestamp"] or 0))
        if take_message:
            selected.append(pending_messages[message_index]); message_index += 1
        else:
            selected.append(pending_events[event_index]); event_index += 1
    next_messages = message_cursor if messages["blocks"] else False
    next_events = event_cursor if events else False
    for block in selected:
        if block["source"] == "event":
            next_events = block["message"]
        else:
            next_messages = ({"message": block["message"], "end": block["offset"]}
                             if block["offset"] else {"message": block["message"] - 1, "end": 0}
                             if block["message"] > 1 else False)
        block["before"] = {"messages": next_messages, "events": next_events}
    if sum(block["source"] == "message" for block in selected) == len(messages["blocks"]) and messages["older"] is None:
        next_messages = False
    if sum(block["source"] == "event" for block in selected) == len(events) and len(events) < 120:
        next_events = False
    selected.reverse()
    return {"blocks": selected, "older": {"messages": next_messages, "events": next_events}
            if next_messages is not False or next_events is not False else None}
