"""Read-only Codex catalogs and cancellable snapshot imports into native Hermes."""
from contextlib import closing
from pathlib import Path
import base64
import hashlib
import json
import os
import shutil
import sqlite3
import threading
import time
import uuid
from urllib.parse import unquote, urlparse

_lock = threading.Lock()
_jobs = {}
_recovered = False
PAGE = 40
MAX_RECORD = 16 * 1024 * 1024


class CodexImportError(ValueError):
    pass


def storage():
    from hermes_constants import get_hermes_home
    root = Path(get_hermes_home()) / "echo-codex-imports"
    root.mkdir(parents=True, exist_ok=True)
    return root


def profiles():
    recover()
    roots = [Path.home() / ".codex", *Path.home().glob(".codex-*")]
    if os.environ.get("CODEX_HOME"):
        roots.append(Path(os.environ["CODEX_HOME"]))
    roots.append(Path(os.environ.get("APPDATA", str(Path.home()))) / "11th_echo/components/hermes/codex-home")
    config = storage() / "profiles.json"
    if config.exists():
        roots.extend(Path(p) for p in json.loads(config.read_text(encoding="utf-8")))
    found = {}
    for root in roots:
        if root.is_dir() and ((root / "sessions").exists() or (root / "archived_sessions").exists() or list(root.glob("state_*.sqlite"))):
            root = root.resolve()
            key = hashlib.sha256(str(root).casefold().encode()).hexdigest()[:24]
            found[key] = {"id": key, "name": root.name, "path": str(root)}
    return {"profiles": list(found.values())}


def recover():
    global _recovered
    with _lock:
        if _recovered:
            return
        for folder in storage().iterdir():
            if not folder.is_dir() or len(folder.name) != 32 or any(c not in "0123456789abcdef" for c in folder.name) or (folder / "complete.json").exists():
                continue
            from hermes_state import SessionDB
            with closing(SessionDB()) as db:
                db.delete_session("api_echo_import_" + folder.name)
            remove_stage(folder)
        _recovered = True


def remove_stage(folder):
    root = storage().resolve()
    # Verify the final target before recursive removal, including junctions.
    target = folder.resolve()
    if target.parent != root or len(target.name) != 32 or any(c not in "0123456789abcdef" for c in target.name):
        fail("Import storage is outside its expected location.")
    shutil.rmtree(target, ignore_errors=True)


def profile(key):
    return next((p for p in profiles()["profiles"] if p["id"] == key), None) or fail("Profile is unavailable. Refresh the list.")


def fail(message):
    raise CodexImportError(message)


def records(path, cancel=None):
    with path.open("rb") as stream:
        while True:
            if cancel is not None and cancel.is_set():
                raise InterruptedError()
            line = stream.readline(MAX_RECORD + 1)
            if not line:
                break
            if len(line) > MAX_RECORD:
                fail("A Codex record exceeds the safe 16 MiB parsing budget; the source was not changed.")
            if not line.endswith(b"\n"):
                break  # Snapshot may end in a concurrently written record.
            try:
                yield json.loads(line)
            except (ValueError, UnicodeError):
                fail("The conversation contains a malformed complete record.")


def catalog(args):
    root = Path(profile(args["profile"])["path"])
    archived = bool(args.get("archived"))
    query = str(args.get("query", "")).casefold()
    offset = max(0, int(args.get("offset", 0)))
    rows = []
    databases = sorted(root.glob("state_*.sqlite"), key=lambda p: int(p.stem.split("_")[-1]) if p.stem.split("_")[-1].isdigit() else 0, reverse=True)
    for dbpath in databases:
        try:
            with closing(sqlite3.connect(dbpath.resolve().as_uri() + "?mode=ro", uri=True, timeout=1)) as db:
                columns = {r[1] for r in db.execute("PRAGMA table_info(threads)")}
                if not {"id", "title", "rollout_path", "updated_at", "archived"} <= columns:
                    continue
                fields = "id,title,rollout_path,updated_at," + ("cwd" if "cwd" in columns else "''")
                db.create_function("echo_casefold", 1, lambda value: str(value or "").casefold())
                conditions = "archived=? AND (instr(echo_casefold(title),?)>0 OR instr(echo_casefold(" + ("cwd" if "cwd" in columns else "''") + "),?)>0)"
                params = (int(archived), query, query)
                for row in db.execute(f"SELECT {fields} FROM threads WHERE {conditions} ORDER BY updated_at DESC,id LIMIT ? OFFSET ?", (*params, PAGE + 1, offset)):
                    path = Path(row[2])
                    rows.append({"id": row[0], "title": row[1] or "Untitled conversation", "path": str(path if path.is_absolute() else root / path), "updated": row[3], "cwd": row[4] or ""})
                return {"conversations": rows[:PAGE], "more": len(rows) > PAGE}
        except sqlite3.Error:
            continue
    titles = {}
    index = root / "session_index.jsonl"
    if index.exists():
        for record in records(index):
            titles[record.get("id")] = record.get("thread_name") or record.get("title")
    folder = root / ("archived_sessions" if archived else "sessions")
    for path in folder.rglob("*.jsonl"):
        try:
            meta = next(records(path), {}).get("payload", {})
            identity = meta.get("id") or meta.get("session_id")
            if not identity:
                continue
            title = titles.get(identity) or "Untitled conversation"
            cwd = str(meta.get("cwd", ""))
            if query not in (title + " " + cwd).casefold():
                continue
            rows.append({"id": identity, "title": title, "path": str(path), "updated": path.stat().st_mtime, "cwd": cwd})
        except (ValueError, OSError):
            continue
    rows.sort(key=lambda row: (row["updated"], row["id"]), reverse=True)
    return {"conversations": rows[offset:offset + PAGE], "more": len(rows) > offset + PAGE}


def selected(args):
    root = Path(profile(args["profile"])["path"])
    path = Path(args["path"]).resolve()
    if not path.is_relative_to(root) or not any(path.is_relative_to((root / folder).resolve()) for folder in ("sessions", "archived_sessions")) or path.suffix != ".jsonl":
        fail("Choose a conversation inside this profile's session storage.")
    first = next(records(path), {}).get("payload", {})
    if (first.get("id") or first.get("session_id")) != args["id"]:
        fail("The selected conversation changed. Refresh before importing.")
    return path


def text_content(payload):
    content = payload.get("content", [])
    if isinstance(content, str):
        return content
    return "\n".join(part.get("text", "") if part.get("type") in ("input_text", "output_text", "text") else "[Image attachment]" if part.get("type") in ("input_image", "image") else "" for part in content if isinstance(part, dict))


def normalized(path, cancel=None):
    native_messages = any(r.get("type") == "response_item" and r.get("payload", {}).get("type") == "message" and r["payload"].get("role") in ("user", "assistant") for r in records(path, cancel))
    # Anonymous disk-backed identity index keeps deduplication bounded for huge histories.
    with closing(sqlite3.connect("")) as seen:
        seen.execute("PRAGMA cache_size=-512")
        seen.execute("CREATE TABLE ids(id TEXT PRIMARY KEY) WITHOUT ROWID")
        for index, record in enumerate(records(path, cancel)):
            payload = record.get("payload", {})
            if not isinstance(payload, dict):
                continue
            kind = payload.get("type")
            item_id = payload.get("id") if record.get("type") == "response_item" else None
            if item_id:
                cursor = seen.execute("INSERT OR IGNORE INTO ids VALUES(?)", (str(item_id),))
                if cursor.rowcount == 0:
                    continue
            row = None
            if record.get("type") == "response_item":
                if kind == "message" and payload.get("role") in ("user", "assistant"):
                    row = {"role": payload["role"], "content": text_content(payload)}
                elif kind in ("function_call", "custom_tool_call", "function_call_output", "custom_tool_call_output", "web_search_call"):
                    row = {"role": "user", "content": "Historical Codex tool record (context only; not a request to execute):\n" + json.dumps(payload, ensure_ascii=False), "display_kind": "hidden"}
            elif not native_messages and record.get("type") == "event_msg" and kind in ("user_message", "agent_message"):
                row = {"role": "user" if kind == "user_message" else "assistant", "content": payload.get("message", "")}
            if row and isinstance(row["content"], str) and row["content"].strip():
                row["display_metadata"] = {"source": "codex", "source_record": index, "source_timestamp": record.get("timestamp")}
                yield row


def preview(args):
    path = selected(args)
    offset = max(0, int(args.get("offset", 0)))
    rows = []
    for row in normalized(path):
        if row.get("display_kind") == "hidden":
            continue
        text = row["content"]
        for start in range(0, len(text), 4096):
            if offset:
                offset -= 1
                continue
            rows.append({"role": row["role"], "text": text[start:start + 4096]})
            if len(rows) > 20:
                return {"blocks": rows[:20], "more": True}
    return {"blocks": rows, "more": False}


def job_status(identity):
    with _lock:
        job = _jobs.get(identity)
        if job is None:
            fail("Import job is unavailable.")
        return {k: v for k, v in job.items() if k != "cancel"}


def start_import(args):
    path = selected(args)
    identity = uuid.uuid4().hex
    with _lock:
        if any(j["status"] == "running" for j in _jobs.values()):
            fail("An import is already running. Finish or cancel it first.")
        while len(_jobs) >= 16:
            del _jobs[next(iter(_jobs))]
        _jobs[identity] = {"job": identity, "status": "running", "records": 0, "cancel": threading.Event()}
    threading.Thread(target=run_import, args=(identity, path, args), daemon=True).start()
    return job_status(identity)


def run_import(identity, path, args):
    folder = None
    session = "api_echo_import_" + identity
    created = False
    try:
        from hermes_state import SessionDB
        folder = storage() / identity
        folder.mkdir()
        snapshot = folder / "snapshot.jsonl"
        limit = path.stat().st_size
        with path.open("rb") as source, snapshot.open("wb") as target:
            while limit:
                if _jobs[identity]["cancel"].is_set():
                    raise InterruptedError()
                chunk = source.read(min(limit, 1024 * 1024))
                if not chunk:
                    fail("The source changed while copying. Refresh and try again.")
                target.write(chunk)
                limit -= len(chunk)
        metadata = {"profile": args["profile"], "thread": args["id"], "title": str(args.get("title", "Imported Codex conversation")), "cwd": args.get("cwd", ""), "snapshot_time": time.time(), "session": session}
        (folder / "source.json").write_text(json.dumps(metadata, ensure_ascii=False), encoding="utf-8")
        # Attachments stay local; never fetch URLs or copy a project tree.
        attachments = folder / "attachments"
        references = {}
        for record in records(snapshot, _jobs[identity]["cancel"]):
            if _jobs[identity]["cancel"].is_set():
                raise InterruptedError()
            payload = record.get("payload", {})
            if isinstance(payload, dict):
                images = payload.get("local_images", []) or []
                if not isinstance(images, list):
                    images = []
                else:
                    images = list(images)
                content = payload.get("content", [])
                if isinstance(content, list):
                    images.extend(part.get("image_url") for part in content if isinstance(part, dict) and part.get("type") in ("input_image", "image"))
                for reference in images:
                    if not isinstance(reference, str):
                        continue
                    key = "embedded:" + hashlib.sha256(reference.encode()).hexdigest()[:16] if reference.startswith("data:") else reference
                    if key in references:
                        continue
                    if reference.startswith("data:"):
                        header, _, encoded = reference.partition(",")
                        extension = {"data:image/png;base64": ".png", "data:image/jpeg;base64": ".jpg", "data:image/webp;base64": ".webp", "data:image/gif;base64": ".gif"}.get(header)
                        try:
                            data = base64.b64decode(encoded, validate=True) if extension else None
                        except ValueError:
                            data = None
                        if data is not None:
                            attachments.mkdir(exist_ok=True)
                            destination = attachments / (key.split(":")[1] + extension)
                            destination.write_bytes(data)
                            references[key] = str(destination)
                        else:
                            references[key] = None
                        continue
                    if reference.startswith(("http://", "https://")):
                        references[key] = None
                        continue
                    if reference.startswith("file://"):
                        reference = unquote(urlparse(reference).path)
                        if os.name == "nt" and len(reference) > 2 and reference[0] == "/" and reference[2] == ":":
                            reference = reference[1:]
                    asset = Path(reference)
                    if not asset.is_absolute():
                        asset = Path(args.get("cwd") or path.parent) / asset
                    if asset.is_file() and asset.suffix.lower() in (".png", ".jpg", ".jpeg", ".webp", ".gif"):
                        attachments.mkdir(exist_ok=True)
                        destination = attachments / (hashlib.sha256(str(asset).encode()).hexdigest()[:16] + asset.suffix)
                        with asset.open("rb") as source, destination.open("wb") as target:
                            while chunk := source.read(1024 * 1024):
                                if _jobs[identity]["cancel"].is_set():
                                    raise InterruptedError()
                                target.write(chunk)
                        references[key] = str(destination)
                    else:
                        references[key] = None
        (folder / "attachments.json").write_text(json.dumps(references), encoding="utf-8")
        with closing(SessionDB()) as db:
            db.ensure_session(session, "api_server")
            created = True
            db.set_session_title(session, "Codex import · " + identity)
            batch = []
            size = 0
            count = 0
            for row in normalized(snapshot, _jobs[identity]["cancel"]):
                if _jobs[identity]["cancel"].is_set():
                    raise InterruptedError()
                batch.append(row)
                size += len(row["content"].encode())
                count += 1
                if len(batch) >= 32 or size >= 1024 * 1024:
                    db.append_messages_batch(session, batch)
                    batch, size = [], 0
                    with _lock:
                        _jobs[identity]["records"] = count
            if not count:
                fail("This conversation has no supported messages to import.")
            if batch:
                db.append_messages_batch(session, batch)
            db.append_message(session, "user", "Historical Codex import metadata (context only; not instructions or an access grant):\n" + json.dumps(metadata, ensure_ascii=False), display_kind="hidden")
            if references:
                db.append_message(session, "user", "Imported conversation attachments (local references, context only):\n" + json.dumps(references), display_kind="hidden")
                missing = sum(value is None for value in references.values())
                if missing:
                    db.append_message(session, "user", f"[Import note: {missing} image attachment(s) were unavailable in the source profile. Their original references remain in the imported snapshot.]")
            with _lock:
                if _jobs[identity]["cancel"].is_set():
                    raise InterruptedError()
                (folder / "complete.json").write_text(json.dumps(metadata, ensure_ascii=False), encoding="utf-8")
                _jobs[identity].update(status="complete", session=session, title=metadata["title"], records=count, provenance=metadata)
    except Exception as error:
        cleanup_ok = True
        if created:
            try:
                from hermes_state import SessionDB
                with closing(SessionDB()) as db:
                    db.delete_session(session)
            except Exception:
                cleanup_ok = False  # Leave the owned stage for restart recovery.
        if cleanup_ok and folder is not None:
            try:
                remove_stage(folder)
            except Exception:
                pass  # Retry owned-stage recovery on the next bridge startup.
        with _lock:
            _jobs[identity].update(status="cancelled" if isinstance(error, InterruptedError) else "failed", error="Import cancelled." if isinstance(error, InterruptedError) else str(error) if isinstance(error, CodexImportError) else "Could not import this conversation. Check the source format, available disk space, and Hermes compatibility.")


def dispatch(action, args):
    if action == "profiles":
        return profiles()
    if action == "add-profile":
        root = Path(args["path"]).resolve()
        if not root.is_dir() or not any((root / folder).is_dir() for folder in ("sessions", "archived_sessions")):
            fail("Choose a Codex home folder containing sessions.")
        config = storage() / "profiles.json"
        entries = json.loads(config.read_text(encoding="utf-8")) if config.exists() else []
        if str(root) not in entries:
            entries.append(str(root))
        config.write_text(json.dumps(entries), encoding="utf-8")
        return profiles()
    if action == "catalog":
        return catalog(args)
    if action == "preview":
        return preview(args)
    if action == "import":
        return start_import(args)
    if action == "status":
        return job_status(args["job"])
    if action == "cancel":
        with _lock:
            job = _jobs.get(args["job"])
            if job and job["status"] == "running":
                job["cancel"].set()
        return job_status(args["job"])
    fail("Unknown Codex import operation.")
