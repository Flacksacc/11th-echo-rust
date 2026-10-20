"""Explicit workspace approval; native tasks, worktrees and claims remain authoritative."""
import json
import os
from pathlib import Path
import threading
from .native import board_connection, stop_task

_LOCK = threading.RLock()


def grants_path():
    return Path(os.environ["HERMES_HOME"]).parent / "workspace-grants.json"


def grants():
    path = grants_path()
    if not path.is_file():
        return {}
    if path.stat().st_size > 1024 * 1024:
        raise ValueError("Workspace approval metadata exceeded its budget")
    return json.loads(path.read_text(encoding="utf-8"))


def approve_workspace(task_id, workspace, board="echo"):
    from hermes_cli import kanban_db as kb
    proposed = Path(workspace)
    if not proposed.is_absolute() or not proposed.is_dir():
        raise ValueError("Select an existing absolute workspace folder")
    path = proposed.resolve()
    managed = Path(os.environ["HERMES_HOME"]).parent.resolve()
    protected = [Path(path.anchor), Path.home().resolve(), managed]
    if os.environ.get("WINDIR"):
        protected.append(Path(os.environ["WINDIR"]).resolve())
    if path in protected or any(path.is_relative_to(root) for root in protected[2:]):
        raise ValueError("Select a project folder rather than a system, home, or runtime folder")
    is_git = (path / ".git").exists()
    with _LOCK, board_connection(board) as conn:
        task = kb.get_task(conn, task_id)
        if task is None:
            raise LookupError("Task not found")
        if task.status != "blocked" or task.worker_pid:
            raise ValueError("Access can only be approved before this blocked task starts")
        if task.assignee != "echo-codex":
            raise ValueError("Workspace approval currently requires the managed Codex worker")
        worktree_root = path / ".worktrees"
        if is_git and worktree_root.exists() and not worktree_root.resolve().is_relative_to(path):
            raise ValueError("The worktree directory points outside this repository")
        approvals = grants()
        if not is_git:
            for other_id, grant in approvals.items():
                other = kb.get_task(conn, other_id)
                if grant["source"] == str(path) and other and other.status not in {"done", "archived"}:
                    raise ValueError("Another task already has write access to this shared folder")
        # A fresh native attempt records the approved workspace kind at creation.
        # Keep the parked request and its history as the originating task.
        next_id = kb.create_task(conn, title=task.title, body=task.body,
            assignee=task.assignee, session_id=task.session_id, created_by="echo-user",
            creator_task_id=task_id, initial_status="blocked",
            workspace_kind="worktree" if is_git else "dir", workspace_path=str(path),
            model_override=task.model_override, provider_override=task.provider_override)
        approvals[next_id] = {"source": str(path), "capability": "workspace_write"}
        pending = grants_path().with_suffix(".pending")
        pending.write_text(json.dumps(approvals), encoding="utf-8")
        pending.replace(grants_path())
        kb.add_comment(conn, next_id, "echo-user", "Approved write access to this workspace only. Network and desktop actions remain unavailable.")
        kb.add_comment(conn, task_id, "echo-user", f"Approved continuation: {next_id}")
        stop_task(task_id, board)
        if not kb.unblock_task(conn, next_id):
            raise ValueError("Native Hermes did not unblock the approved task")
        return {"task_id": next_id, "origin_task_id": task_id, "workspace": str(path), "isolated_worktree": is_git}


def worker_permissions(task_id, board="echo"):
    from hermes_cli import kanban_db as kb
    with _LOCK, board_connection(board) as conn:
        task = kb.get_task(conn, task_id)
        grant = grants().get(task_id)
        if not grant or not task or task.status != "running" or not task.claim_lock or not task.workspace_path:
            return {"profile": ":read-only"}
        workspace = Path(task.workspace_path).resolve()
        source = Path(grant["source"]).resolve()
        if workspace != source and not workspace.is_relative_to(source / ".worktrees"):
            raise ValueError("Native workspace is outside the approved scope")
        return {"profile": ":workspace", "workspace": str(workspace), "network": False}


def retry_task(task_id, board="echo"):
    """Explicit native continuation; old workspace grants never carry over."""
    from hermes_cli import kanban_db as kb
    with _LOCK, board_connection(board) as conn:
        task = kb.get_task(conn, task_id)
        if task is None:
            raise LookupError("Task not found")
        if task.status not in {"blocked", "archived", "done"} or task.worker_pid or task.claim_lock:
            raise ValueError("Stop the worker and confirm termination before retrying")
        if task.assignee != "echo-codex":
            raise ValueError("Retry currently requires the managed Codex worker")
        termination = conn.execute(
            "SELECT payload FROM task_events WHERE task_id=? AND kind='archive_worker_termination' ORDER BY id DESC LIMIT 1",
            (task_id,),
        ).fetchone()
        if termination and not json.loads(termination[0] or "{}").get("terminated"):
            raise ValueError("The previous worker's termination is unconfirmed")
        previous_run = kb.latest_run(conn, task_id)
        if previous_run and previous_run.worker_pid:
            if task.status == "archived" and previous_run.outcome == "reclaimed" and not termination:
                raise ValueError("The previous worker's termination is still being confirmed")
            if kb._pid_alive(previous_run.worker_pid):
                raise ValueError("Wait for the previous worker process to finish before retrying")
        # Native idempotency plus the adapter lock makes repeated button clicks
        # converge on the same continuation. Hermes owns attempt scheduling.
        key = "echo-user-retry:" + task_id
        existing = conn.execute(
            "SELECT id,status FROM tasks WHERE idempotency_key=? AND status!='archived' ORDER BY created_at DESC LIMIT 1",
            (key,),
        ).fetchone()
        if existing:
            return {"task_id": existing["id"], "origin_task_id": task_id,
                    "status": existing["status"]}
        needs_access = task_id in grants() or task.workspace_kind in {"dir", "worktree"}
        # A modification parked before its first grant is also an access request.
        needs_access = needs_access or conn.execute(
            "SELECT 1 FROM task_comments WHERE task_id=? AND author='echo' AND body=? LIMIT 1",
            (task_id, "Waiting for the user's workspace access selection."),
        ).fetchone() is not None
        next_id = kb.create_task(
            conn, title=task.title,
            body=f"Explicit retry of native task {task_id}. Read its comments and prior result before proceeding.\n\n{task.body or ''}",
            assignee=task.assignee, session_id=task.session_id, created_by="echo-user",
            creator_task_id=task_id, idempotency_key=key, initial_status="blocked",
            workspace_kind="scratch", model_override=task.model_override,
            provider_override="openai" if task.model_override else None,
            reasoning_effort=task.reasoning_effort,
        )
        if task.status == "blocked":
            stop_task(task_id, board)
        kb.add_comment(conn, task_id, "echo-user", f"Explicit retry continuation: {next_id}")
        if needs_access:
            kb.add_comment(conn, next_id, "echo", "Waiting for the user's workspace access selection.")
        elif not kb.unblock_task(conn, next_id):
            raise ValueError("Native Hermes did not unblock the retry")
        return {"task_id": next_id, "origin_task_id": task_id,
                "status": "blocked" if needs_access else "ready"}
