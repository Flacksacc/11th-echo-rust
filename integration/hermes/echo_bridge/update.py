"""Echo transaction around Hermes' native source updater. No model calls or secrets in output.

Executed as a standalone stdlib script, including after an interrupted update.
The caller holds Echo's exclusive runtime lock and has stopped its idle gateway.
Dependency generations stay in place; a native lease protects the old interpreter.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import stat
import subprocess
import sys
import uuid

HOME_EXCLUDED = {"tools", "installs", "backups", "state-snapshots"}
SOURCE_EXCLUDED = {"__pycache__", ".venv", "venv", "node_modules"}
INSTALL_EXCLUDED = {"environments", "generations", "bootstrap"}
JOURNAL = "update-journal.json"


def atomic_json(path, value):
    pending = path.with_suffix(".pending")
    with pending.open("w", encoding="utf-8") as stream:
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(pending, path)


def safe_tree(path):
    # Do not follow user-created symlinks/junctions into another installation.
    reparse = path.exists() and getattr(path.lstat(), "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT
    if path.is_symlink() or reparse:
        raise ValueError("Linked runtime files cannot be updated safely")


def copy_tree(source, destination, excluded=frozenset(), recursive_excluded=frozenset()):
    safe_tree(source)
    destination.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        if entry.name in excluded or entry.name.endswith(("-wal", "-shm")):
            continue
        safe_tree(entry)
        if entry.is_dir():
            copy_tree(entry, destination / entry.name, recursive_excluded, recursive_excluded)
        else:
            sqlite = False
            if entry.suffix in {".db", ".sqlite", ".sqlite3"}:
                with entry.open("rb") as stream:
                    sqlite = stream.read(16) == b"SQLite format 3\x00"
            if sqlite:
                # Include committed WAL data rather than copying a stale main file.
                from contextlib import closing
                with closing(sqlite3.connect(entry.as_uri() + "?mode=ro", uri=True)) as db:
                    with closing(sqlite3.connect(destination / entry.name)) as snapshot:
                        db.backup(snapshot)
            else:
                shutil.copy2(entry, destination / entry.name)


def copy_install_state(source, destination):
    if not source.exists():
        return
    safe_tree(source)
    destination.mkdir(parents=True, exist_ok=True)
    for entry in source.iterdir():
        if entry.name in INSTALL_EXCLUDED or entry.name.startswith("."):
            continue
        safe_tree(entry)
        if entry.is_dir():
            copy_install_state(entry, destination / entry.name)
        else:
            shutil.copy2(entry, destination / entry.name)


def clear_tree(path, excluded=frozenset()):
    safe_tree(path)
    for entry in path.iterdir():
        if entry.name in excluded:
            continue
        safe_tree(entry)
        if entry.is_dir():
            clear_tree(entry)
            entry.rmdir()
        else:
            # Git object files are read-only on Windows.
            entry.chmod(stat.S_IWRITE | stat.S_IREAD)
            entry.unlink()


def clear_install_state(path):
    if not path.exists():
        return
    safe_tree(path)
    for entry in path.iterdir():
        if entry.name in INSTALL_EXCLUDED or entry.name.startswith("."):
            continue
        safe_tree(entry)
        if entry.is_dir():
            clear_install_state(entry)
        else:
            entry.unlink()


def selected_python(root):
    receipts = sorted((root / "home/installs").glob("*/facts.json"),
                      key=lambda p: p.stat().st_mtime, reverse=True)
    for receipt in receipts:
        value = json.loads(receipt.read_text(encoding="utf-8"))
        environment = value.get("packages", {}).get("venv", {}).get("environment")
        if environment:
            path = (Path(environment) / "Scripts/python.exe").resolve(strict=True)
            if not path.is_relative_to(root.resolve(strict=True)):
                raise ValueError("Interpreter is outside the managed runtime")
            return path
    raise ValueError("No managed interpreter")


def recover(root):
    journal = root / JOURNAL
    if not journal.exists():
        return False
    if journal.stat().st_size > 4096:
        raise ValueError("Invalid update recovery record")
    value = json.loads(journal.read_text(encoding="utf-8"))
    identifier = value.get("backup", "")
    if not re.fullmatch(r"[a-f0-9]{32}", identifier):
        raise ValueError("Invalid backup identifier")
    backup = root / "updates" / identifier
    for path in (root, root / "updates", backup, backup / "source", backup / "home",
                 backup / "installs", root / "source", root / "home"):
        safe_tree(path)
    complete = backup / "complete.json"
    if (not complete.is_file() or complete.stat().st_size > 256
            or json.loads(complete.read_text(encoding="utf-8")).get("schema") != 1
            or any(not (backup / name).is_dir() for name in ("source", "home", "installs"))):
        raise ValueError("Update backup is incomplete")
    # Keep backups immutable so restoration itself can be interrupted and retried.
    clear_tree(root / "source")
    copy_tree(backup / "source", root / "source")
    clear_tree(root / "home", HOME_EXCLUDED)
    copy_tree(backup / "home", root / "home")
    clear_install_state(root / "home/installs")
    copy_install_state(backup / "installs", root / "home/installs")
    tool_facts = root / "home/tools/facts.json"
    safe_tree(tool_facts.parent)
    safe_tree(tool_facts)
    if (backup / "tool-facts.json").exists():
        shutil.copy2(backup / "tool-facts.json", tool_facts)
    elif tool_facts.exists():
        tool_facts.unlink()
    journal.unlink()
    return True


def native_command(python, source, args):
    return subprocess.run([str(python), *args], cwd=source, stdin=subprocess.DEVNULL,
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                          check=False).returncode == 0


def assert_idle():
    import time
    from contextlib import closing
    home = Path(os.environ["HERMES_HOME"]).resolve(strict=True)
    component = home.parent
    try:
        import psutil
        ancestors = {process.pid for process in psutil.Process().parents()} | {os.getpid()}
        processes = [(p.info["pid"], p.info["exe"]) for p in psutil.process_iter(["pid", "exe"])]
    except ImportError:
        # A dependency repair can start with only the managed base Python.
        # Read process paths with Windows itself; never request command lines.
        raw = subprocess.check_output(["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            "@(Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,ExecutablePath) | ConvertTo-Json -Compress"],
            stderr=subprocess.DEVNULL, timeout=30, creationflags=subprocess.CREATE_NO_WINDOW)
        rows = json.loads(raw)
        parents = {row["ProcessId"]: row["ParentProcessId"] for row in rows}
        ancestors = {os.getpid()}
        current = os.getpid()
        while (current := parents.get(current)) and current not in ancestors:
            ancestors.add(current)
        processes = [(row["ProcessId"], row["ExecutablePath"]) for row in rows]
    for pid, executable in processes:
        if pid in ancestors or not executable:
            continue
        executable = Path(executable).resolve()
        if executable.is_relative_to(component) and executable.name.lower() in {"python.exe", "pythonw.exe", "hermes.exe"}:
            raise ValueError("Another process is using the managed Hermes runtime")
    if (home / "state.db").exists():
        with closing(sqlite3.connect((home / "state.db").as_uri() + "?mode=ro", uri=True)) as db:
            if db.execute("SELECT 1 FROM session_turn_leases WHERE expires_at > ? LIMIT 1", (time.time(),)).fetchone():
                raise ValueError("Conversations are still running")
    for path in (home / "kanban").rglob("kanban.db"):
        if "_archived" in path.parts:
            continue
        with closing(sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)) as db:
            if db.execute("SELECT 1 FROM tasks WHERE status NOT IN ('done','archived') LIMIT 1").fetchone():
                raise ValueError("Background tasks are still running")


def transaction(root, python, mode, runner=native_command, probe=None, idle=assert_idle):
    source = root / "source"
    sys.path.insert(0, str(source))
    idle()
    if mode == "update":
        safe_tree(source / ".git")
        if not (source / ".git").is_dir():
            raise ValueError("Updates require a managed Git checkout, not an external worktree")
        origin = subprocess.check_output(["git", "remote", "get-url", "origin"], cwd=source,
                                         stderr=subprocess.DEVNULL, text=True).strip()
        if origin not in {"https://github.com/NousResearch/hermes-agent", "https://github.com/NousResearch/hermes-agent.git"}:
            raise ValueError("Updates require the official HTTPS Hermes repository")
    from hermes_cli.runtime_state import lease_generation
    release = lease_generation(python.parent.parent)
    try:
        identifier = uuid.uuid4().hex
        backup = root / "updates" / identifier
        safe_tree(root / "updates")
        backup.mkdir(parents=True)
        copy_tree(source, backup / "source", SOURCE_EXCLUDED, SOURCE_EXCLUDED)
        copy_tree(root / "home", backup / "home", HOME_EXCLUDED)
        copy_install_state(root / "home/installs", backup / "installs")
        tool_facts = root / "home/tools/facts.json"
        if tool_facts.exists():
            safe_tree(tool_facts.parent)
            safe_tree(tool_facts)
            shutil.copy2(tool_facts, backup / "tool-facts.json")
        atomic_json(backup / "complete.json", {"schema": 1})
        atomic_json(root / JOURNAL, {"backup": identifier, "python": str(python.relative_to(root))})
        stage = "native_update"
        try:
            if mode == "repair":
                args = ["-m", "hermes_cli.venv_sync", "--project-root", str(source), "--json"]
            else:
                args = ["-m", "hermes_cli.main", "update", "--yes", "--backup", "--keep-stash",
                        "--no-gateway-restart", "--channel", "main"]
            if not runner(python, source, args):
                raise ValueError("Native updater failed")
            stage = "compatibility"
            current = selected_python(root)
            check = probe or (lambda p: native_command(p, source, ["-m", "echo_bridge.update_probe"]))
            if not check(current):
                raise ValueError("Echo compatibility check failed")
            stage = "revision"
            commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source,
                                             stderr=subprocess.DEVNULL, text=True).strip()
            if not re.fullmatch(r"[a-f0-9]{40,64}", commit):
                raise ValueError("Invalid revision")
            (root / "update-failure.json").unlink(missing_ok=True)
            (root / JOURNAL).unlink()
            return "updated" if mode == "update" else "repaired"
        except Exception:
            recover(root)
            atomic_json(root / "update-failure.json", {"stage": stage})
            return "rolled_back"
    finally:
        release()


def main():
    root = Path(sys.argv[1]).resolve(strict=True)
    mode = sys.argv[2]
    try:
        recovered = recover(root)
        if mode == "recover":
            result = "recovered" if recovered else "unchanged"
        elif mode in {"update", "repair"}:
            try:
                python = selected_python(root)
            except (OSError, ValueError):
                if mode != "repair":
                    raise
                python = Path(sys.executable).resolve(strict=True)
                if not python.is_relative_to(root):
                    raise ValueError("Repair interpreter is outside the managed runtime")
            result = transaction(root, python, mode)
        else:
            raise ValueError("Invalid update operation")
        atomic_json(root / "update-result.json", {"result": result})
        return 0 if result != "rolled_back" else 2
    except Exception:
        # Never expose native exception text, which may contain private config.
        return 1


if __name__ == "__main__":
    sys.exit(main())
