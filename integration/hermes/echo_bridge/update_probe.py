"""Check Echo's native contracts in a fresh, disposable home; never call a model."""
import asyncio
import json
from contextlib import closing
import os
from pathlib import Path
import sys
import tempfile


async def check():
    from aiohttp import web
    from aiohttp.test_utils import TestClient, TestServer
    from gateway.config import PlatformConfig
    from gateway.platforms.api_server import APIServerAdapter
    from hermes_state import SessionDB
    from echo_bridge.native import board_connection
    from echo_bridge import server, plugin, google_provider, access, worker_tools
    from hermes_cli import kanban_db as kb
    kb.create_board("echo", name="Echo compatibility probe")
    adapter = APIServerAdapter(PlatformConfig(enabled=True, extra={"key": "synthetic-echo-update-probe-token-12345678"}))
    routes = adapter._http_route_table()
    required = {("GET", "/v1/capabilities"), ("POST", "/api/sessions"),
                ("GET", "/api/sessions/{session_id}/messages"),
                ("POST", "/api/sessions/{session_id}/chat/stream"),
                ("POST", "/v1/runs"), ("GET", "/v1/runs/{run_id}/events"),
                ("POST", "/v1/runs/{run_id}/stop"), ("POST", "/v1/runs/{run_id}/approval")}
    if not required <= {(method, path) for method, path, _ in routes}:
        raise ValueError("Native API routes changed")
    app = web.Application()
    for method, path, handler in routes:
        app.router.add_route(method, path, handler)
    try:
        async with TestClient(TestServer(app)) as client:
            headers = {"Authorization": "Bearer synthetic-echo-update-probe-token-12345678"}
            response = await client.get("/v1/capabilities", headers=headers)
            assert response.status == 200
            response = await client.post("/api/sessions", json={"title": "Echo compatibility probe"}, headers=headers)
            assert response.status == 201
            session = (await response.json())["session"]["id"]
            # Hermes rejects duplicate titles; Echo must give native sessions
            # unique identities instead of reusing the visible tab label.
            duplicate = await client.post("/api/sessions", json={"title": "Echo compatibility probe"}, headers=headers)
            assert duplicate.status == 400
            for number in range(3):
                native_id = f"echo_compatibility_{number}"
                created = await client.post("/api/sessions", json={"id": native_id, "title": f"Echo Assistant · {native_id}", "source": "echo"}, headers=headers)
                assert created.status == 201
                assert (await created.json())["session"]["id"] == native_id
            response = await client.get(f"/api/sessions/{session}/messages?limit=3&order=latest", headers=headers)
            assert response.status == 200
            assert (await response.json())["pagination"]["limit"] == 3
            response = await client.post("/v1/runs", json={"input": "Synthetic"})
            assert response.status == 401
    finally:
        from gateway.platforms.api_server_runs import _close_run_state
        _close_run_state(adapter)
        adapter._close_cached_session_dbs()
        adapter._response_store.close()
    with closing(SessionDB()) as db:
        columns = {row[1] for row in db._conn.execute("PRAGMA table_info(messages)")}
        assert {"id", "session_id", "role", "content", "timestamp", "message_uid", "active", "compacted", "display_kind"} <= columns
        assert callable(db.resolve_resume_session_id) and callable(db.get_compression_lineage)
        assert callable(db.append_messages_batch) and callable(db.delete_session)
    with board_connection() as conn:
        columns = {row[1] for row in conn.execute("PRAGMA table_info(tasks)")}
        assert {"id", "title", "status", "worker_pid", "last_heartbeat_at", "current_run_id", "session_id", "workspace_kind"} <= columns
        for table in ("task_events", "task_comments", "kanban_notify_subs"):
            assert conn.execute(f"PRAGMA table_info({table})").fetchone()
    assert callable(plugin.register) and callable(google_provider.register)
    # Exercise Echo task/subscription/event bindings without launching a worker.
    task = json.loads(plugin._create({"title": "Echo update task probe", "body": "Synthetic fixture", "intent": "modify"}))["task_id"]
    from echo_bridge.native import queued_instruction, stop_task, task_conversation, event_page
    assert task_conversation(task)
    assert queued_instruction(task, "Synthetic queued instruction")["status"] == "queued"
    assert event_page(0)
    assert stop_task(task)["stopped"]
    server.application("synthetic-echo-update-probe-token-12345678", 1)


def main():
    # Source is supplied by the parent; no dependency on an editable .pth target.
    root = Path(__file__).resolve().parents[2]
    sys.path.insert(0, sys.argv[1] if len(sys.argv) > 1 else str(root / "source"))
    with tempfile.TemporaryDirectory(prefix="echo-update-probe-") as home:
        os.environ["HERMES_HOME"] = home
        os.environ.pop("API_SERVER_KEY", None)
        asyncio.run(check())


if __name__ == "__main__":
    main()
