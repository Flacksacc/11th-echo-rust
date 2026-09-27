"""Process fixture for Rust IPC/lifecycle tests; never included in the runtime."""
import importlib.util
from pathlib import Path
import os
import struct
import sys

spec = importlib.util.spec_from_file_location("echo_helper", Path(__file__).with_name("helper.py"))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
source, target = sys.stdin.buffer, sys.stdout.buffer
mode = sys.argv[1]
helper.write_frame(target, 0, "ready")
samples = 0
last = 0
while True:
    try:
        frame = helper.read_frame(source)
    except EOFError:
        break
    session = frame["session"]
    if frame["kind"] == "start":
        if mode == "crash":
            os._exit(1)
        if mode == "malformed":
            target.write(struct.pack("<I", helper.MAX_FRAME + 1))
            target.flush()
            break
        samples = 0
    elif frame["kind"] == "audio":
        samples += len(frame["audio"])
        last = frame["audio"][-1]
        helper.write_frame(target, session if mode != "stale" else session + 1, "partial", "Preview replacement")
    elif frame["kind"] == "stop":
        helper.write_frame(target, session, "final", f"samples={samples};last={last}")
