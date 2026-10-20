"""Authenticated loopback presentation API; delegates execution to Hermes."""

from __future__ import annotations

import asyncio
from dataclasses import asdict
import hmac
import os
import re

from aiohttp import ClientSession, ClientTimeout, ClientError, web

from . import PROTOCOL_VERSION
from .native import board_connection, event_page, queued_instruction, stop_task, task_conversation
from .history import history_page, text_chunks, search_text, history_stamp, history_stamps, task_updates
from .access import approve_workspace, worker_permissions, retry_task

_NATIVE_PATH = re.compile(
    r"(?:health(?:/detailed)?|v1/(?:capabilities|models|toolsets|runs(?:/[A-Za-z0-9_-]+(?:/(?:events|stop|approval|steer))?)?)|"
    r"api/(?:model/options|sessions(?:/[A-Za-z0-9_-]+(?:/(?:messages|chat/stream|model))?)?))\Z"
)
_CLIENT = web.AppKey("native_client", ClientSession)


def application(token: str, gateway_port: int, board: str = "echo") -> web.Application:
    if len(token) < 32 or not 1 <= gateway_port <= 65535:
        raise ValueError("A strong local token and valid native gateway port are required")

    @web.middleware
    async def authorize(request, handler):
        supplied = request.headers.get("Authorization", "")
        if not hmac.compare_digest(supplied, "Bearer " + token):
            raise web.HTTPUnauthorized()
        # Loopback clients may still be browsers. Never accept ambient cookies,
        # CORS requests or a forged browser origin as authorization.
        if request.headers.get("Origin"):
            raise web.HTTPForbidden(reason="Browser origins are not supported")
        try:
            return await handler(request)
        except LookupError:
            raise web.HTTPNotFound(reason="Native resource not found") from None
        except ValueError as exc:
            raise web.HTTPBadRequest(reason=str(exc)) from None
        except ClientError:
            raise web.HTTPServiceUnavailable(reason="Native gateway is loading or disconnected") from None

    app = web.Application(middlewares=[authorize], client_max_size=1024 * 1024)

    async def codex(request):
        from .codex_import import dispatch, CodexImportError
        body = await request.json()
        if not isinstance(body, dict):
            raise ValueError("Codex import requires an object")
        try:
            return web.json_response(await asyncio.to_thread(dispatch, request.match_info["action"], body))
        except CodexImportError as error:
            return web.json_response({"error": str(error)})
        except Exception:
            return web.json_response({"error": "Codex history is unavailable. Refresh the profile or check its storage format."})

    app.router.add_post("/echo/codex/{action}", codex)

    async def clients(app):
        async with ClientSession(
            trust_env=False, timeout=ClientTimeout(total=None, sock_connect=5)
        ) as client:
            app[_CLIENT] = client
            yield

    app.cleanup_ctx.append(clients)

    async def capabilities(request):
        from hermes_cli.version_info import get_version_info
        version = get_version_info()
        return web.json_response({
            "protocol": PROTOCOL_VERSION, "hermes_commit": version.commit,
            "hermes_version": version.display_version,
            "task_authority": "hermes-native", "queued_instructions": True,
            "event_cursor_replay": True, "descendant_cancellation": True,
            "history_authority": "hermes-native",
        })

    async def tasks(request):
        def read():
            with board_connection(board) as conn:
                # Metadata-only projection: do not read potentially enormous
                # task bodies or results on every heartbeat.
                rows = conn.execute("SELECT id, substr(title,1,256) AS title, status, worker_pid, last_heartbeat_at, current_run_id, session_id, workspace_kind FROM tasks ORDER BY created_at DESC LIMIT 200").fetchall()
                return [dict(row) for row in rows]
        return web.json_response({"tasks": await asyncio.to_thread(read)})

    async def runtime_idle(request):
        def read():
            import time
            from contextlib import closing
            from hermes_state import SessionDB
            with board_connection(board) as conn:
                pending = conn.execute("SELECT 1 FROM tasks WHERE status NOT IN ('done','archived') LIMIT 1").fetchone()
            with closing(SessionDB(read_only=True)) as db:
                lease = db._read_one("SELECT 1 FROM session_turn_leases WHERE expires_at > ? LIMIT 1", (time.time(),))
            return {"idle": not pending and not lease}
        return web.json_response(await asyncio.to_thread(read))

    async def stamps(request):
        body = await request.json()
        if not isinstance(body, dict):
            raise ValueError("History metadata requires an object")
        return web.json_response(await asyncio.to_thread(history_stamps, body.get("sessions")))

    async def task_view(request):
        def read():
            from hermes_cli import kanban_db as kb
            task_id = request.match_info["id"]
            with board_connection(board) as conn:
                if conn.execute("SELECT 1 FROM tasks WHERE id=?", (task_id,)).fetchone() is None:
                    raise LookupError("Task not found")
                if request.query.get("output") == "1":
                    path = kb.worker_logs_dir(board=board) / f"{task_id}.log"
                    if not path.is_file():
                        return {"blocks": [], "older": None}
                    with path.open("rb") as stream:
                        stream.seek(0, 2)
                        requested_end = int(request.query.get("end", stream.tell()))
                        if requested_end < 0:
                            raise ValueError("Invalid output cursor")
                        end = min(requested_end, stream.tell())
                        start = max(0, end - 64 * 1024)
                        stream.seek(start)
                        text = stream.read(end - start).decode("utf-8", errors="replace")
                    return {"blocks": [{"message": 0, "offset": start, "text": text, "role": "assistant", "first": True, "last": True}],
                            "older": {"message": 0, "end": start} if start else None}
                before = int(request.query.get("before", "9223372036854775807"))
                rows = conn.execute("SELECT id,kind,substr(payload,1,4096) AS payload,created_at FROM task_events WHERE task_id=? AND id < ? ORDER BY id DESC LIMIT 100", (task_id, before)).fetchall()
                rows.reverse()
                blocks = [{"message": r["id"], "offset": 0, "text": f'{r["kind"]}\n{r["payload"] or ""}', "role": "assistant", "first": True, "last": True} for r in rows]
                return {"blocks": blocks, "older": {"message": rows[0]["id"], "end": 0} if len(rows) == 100 else None}
        return web.json_response(await asyncio.to_thread(read))

    async def updates(request):
        body = await request.json()
        if not isinstance(body, dict) or not isinstance(body.get("session"), str):
            raise ValueError("Task updates require a conversation")
        return web.json_response(await asyncio.to_thread(task_updates,
            request.match_info["id"], body["session"], board, body.get("cursor")))

    async def instruction(request):
        body = await request.json()
        text = body.get("text")
        if not isinstance(text, str):
            raise ValueError("Instruction text must be a string")
        result = await asyncio.to_thread(queued_instruction, request.match_info["id"], text, board)
        return web.json_response(result, status=202)

    async def conversation(request):
        session = await asyncio.to_thread(task_conversation, request.match_info["id"], board)
        return web.json_response({"session_id": session})

    async def stamp(request):
        before = request.query.get("completed_before")
        return web.json_response(await asyncio.to_thread(history_stamp, request.match_info["id"],
            float(before) if before is not None else None))

    async def stop(request):
        result = await asyncio.to_thread(stop_task, request.match_info["id"], board)
        return web.json_response(result, status=200 if result["stopped"] else 202)

    async def access(request):
        body = await request.json()
        workspace = body.get("workspace")
        if not isinstance(workspace,str):
            raise ValueError("Workspace must be a folder path")
        return web.json_response(await asyncio.to_thread(approve_workspace, request.match_info["id"], workspace, board))

    async def permissions(request):
        return web.json_response(await asyncio.to_thread(worker_permissions, request.match_info["id"], board))

    async def retry(request):
        return web.json_response(await asyncio.to_thread(retry_task, request.match_info["id"], board))

    async def model_override(request):
        body = await request.json()
        model = body.get("model")
        if not isinstance(model,str) or len(model) > 200:
            raise ValueError("Worker model must be a short model identifier")
        def save():
            from hermes_cli import kanban_db as kb
            with board_connection(board) as conn:
                task = kb.get_task(conn,request.match_info["id"])
                if task is None:
                    raise LookupError("Task not found")
                if task.status == "archived":
                    raise ValueError("Archived tasks cannot change model")
                # Match the private worker's native app-server route. Direct
                # openai-codex would demand a separate Hermes OAuth login.
                kb.set_model_override(conn,task.id,model or None,"openai" if model else None)
            return {"status":"saved_for_next_attempt"}
        return web.json_response(await asyncio.to_thread(save))

    async def events(request):
        result = await asyncio.to_thread(
            event_page, int(request.query.get("after", "0")),
            int(request.query.get("limit", "100")), board,
        )
        return web.json_response(result)

    async def history(request):
        result = await asyncio.to_thread(
            history_page, request.match_info["id"],
            int(request.query.get("message", "0")), int(request.query.get("end", "0")),
            int(request.query.get("limit", "120")),
        )
        return web.json_response(result)

    async def search(request):
        body = await request.json()
        return web.json_response(await asyncio.to_thread(search_text, request.match_info["id"], body.get("text")))

    async def export(request):
        source = text_chunks(request.match_info["id"])
        sentinel = object()
        # Resolve session ownership before sending HTTP success headers.
        first = await asyncio.to_thread(next,source,sentinel)
        response = web.StreamResponse(headers={"Content-Type":"text/plain; charset=utf-8","Cache-Control":"no-store"})
        try:
            await response.prepare(request)
            chunk = first
            while chunk is not sentinel:
                if chunk:
                    await response.write(chunk)
                chunk = await asyncio.to_thread(next,source,sentinel)
            await response.write_eof()
            return response
        finally:
            await asyncio.to_thread(source.close)

    async def native(request):
        path = request.match_info["path"]
        if not _NATIVE_PATH.fullmatch(path):
            raise web.HTTPNotFound()
        headers = {"Authorization": "Bearer " + token}
        for name in ("Idempotency-Key", "Last-Event-ID", "Content-Type"):
            if name in request.headers:
                headers[name] = request.headers[name]
        async with app[_CLIENT].request(
            request.method, f"http://127.0.0.1:{gateway_port}/{path}",
            params=request.query, headers=headers, data=await request.read(),
            allow_redirects=False,
        ) as response:
            result = web.StreamResponse(status=response.status, headers={
                "Content-Type": response.headers.get("Content-Type", "application/json"),
                "Cache-Control": "no-store",
            })
            await result.prepare(request)
            # Stream and backpressure, including native SSE. Never collect an
            # entire transcript or output log in a temporary response buffer.
            async for chunk in response.content.iter_chunked(16 * 1024):
                await result.write(chunk)
            await result.write_eof()
            return result

    app.router.add_get("/echo/capabilities", capabilities)
    app.router.add_get("/echo/tasks", tasks)
    app.router.add_get("/echo/runtime/idle", runtime_idle)
    app.router.add_post("/echo/history-stamps", stamps)
    app.router.add_get("/echo/tasks/{id}/view", task_view)
    app.router.add_post("/echo/tasks/{id}/updates", updates)
    app.router.add_get("/echo/events", events)
    app.router.add_get("/echo/history/{id}", history)
    app.router.add_get("/echo/history/{id}/stamp", stamp)
    app.router.add_post("/echo/history/{id}/search", search)
    app.router.add_get("/echo/history/{id}/export", export)
    app.router.add_post("/echo/tasks/{id}/instruction", instruction)
    app.router.add_post("/echo/tasks/{id}/conversation", conversation)
    app.router.add_post("/echo/tasks/{id}/stop", stop)
    app.router.add_post("/echo/tasks/{id}/access", access)
    app.router.add_get("/echo/tasks/{id}/permissions", permissions)
    app.router.add_post("/echo/tasks/{id}/model", model_override)
    app.router.add_post("/echo/tasks/{id}/retry", retry)
    app.router.add_route("*", "/native/{path:.*}", native)
    return app


def main() -> None:
    # Credentials are environment-only; never CLI arguments or startup logs.
    token = os.environ.get("API_SERVER_KEY", "")
    port = int(os.environ.get("ECHO_BRIDGE_PORT", "8643"))
    gateway_port = int(os.environ.get("API_SERVER_PORT", "8642"))
    web.run_app(application(token, gateway_port), host="127.0.0.1", port=port,
                print=None, access_log=None)


if __name__ == "__main__":
    main()
