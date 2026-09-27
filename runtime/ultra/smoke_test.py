"""Opt-in GPU test of the private helper; does not use microphone or network.

python runtime/ultra/smoke_test.py --python <runtime>/python.exe --model-dir <weights> --wav <16k mono PCM wav>
"""
import argparse
import importlib.util
from pathlib import Path
import subprocess
import threading
import time
import wave

spec = importlib.util.spec_from_file_location("echo_ultra_helper", Path(__file__).with_name("helper.py"))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--python", required=True)
    parser.add_argument("--model-dir", required=True)
    parser.add_argument("--wav", required=True)
    args = parser.parse_args()
    with wave.open(args.wav) as audio:
        if (audio.getframerate(), audio.getnchannels(), audio.getsampwidth()) != (16000, 1, 2):
            parser.error("Use 16 kHz mono PCM16 WAV")
        samples = audio.readframes(audio.getnframes())
    process = subprocess.Popen([args.python, "-I", str(Path(__file__).with_name("helper.py").resolve()), "--model-dir", args.model_dir], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    watchdog = threading.Timer(240, process.kill)
    watchdog.start()
    started = time.monotonic()
    try:
        message = helper.read_frame(process.stdout)
        if message["kind"] != "ready":
            raise RuntimeError(message["text"])
        print(f"GPU model ready after {time.monotonic() - started:.1f}s")
        for session in [1, 2]:
            def send(kind, **fields):
                import json
                import struct
                data = json.dumps({"protocol": 1, "session": session, "kind": kind, **fields}).encode()
                process.stdin.write(struct.pack("<I", len(data)) + data)
                process.stdin.flush()

            send("start")
            for offset in range(0, len(samples), 8192):
                import struct
                chunk = samples[offset:offset + 8192]
                send("audio", audio=list(struct.unpack(f"<{len(chunk) // 2}h", chunk)))
            send("stop")
            while True:
                message = helper.read_frame(process.stdout)
                if message["session"] != session:
                    raise RuntimeError("Wrong session")
                if message["kind"] == "error":
                    raise RuntimeError(message["text"])
                if message["kind"] == "final":
                    if not message["text"].strip():
                        raise RuntimeError("Empty transcript for speech fixture")
                    print(f"Session {session}: final transcript received ({len(message['text'])} characters)")
                    break
        process.stdin.close()
        if process.wait(timeout=15) != 0:
            raise RuntimeError("Helper shutdown failed")
    finally:
        watchdog.cancel()
        if process.poll() is None:
            process.kill()
        process.wait()


if __name__ == "__main__":
    main()
