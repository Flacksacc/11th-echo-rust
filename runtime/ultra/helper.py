"""Echo's private, versioned stdin/stdout bridge to Photon. Never writes audio to disk."""
import argparse
import asyncio
import json
import os
from pathlib import Path
import socket
import struct
import sys
import threading

PROTOCOL = 1
MAX_FRAME = 256 * 1024
MAX_TEXT = 100_000
MODEL = "moondream/parakeet-ultra"


def configure_streaming():
    """Tune previews only; Photon still decodes the full block on stop.

    Kestrel 0.8.1 has no public preview timing options. Keep its existing
    scheduler and context, adjusting only its chunk and lookahead constants.
    Its scheduler calculates fractional frame counts with subsecond lookahead;
    convert those to integer sample indices at the buffer boundary.
    """
    from kestrel.models.parakeet_tdt import longform

    original_buffer = longform.LiveAudioBuffer

    class PreviewAudioBuffer(original_buffer):
        def snapshot(self, *, offset_frames=0, frame_count=None):
            return super().snapshot(
                offset_frames=offset_frames,
                frame_count=None if frame_count is None else round(frame_count),
            )

    longform.LiveAudioBuffer = PreviewAudioBuffer
    longform._LIVE_CHUNK_SECONDS = 1
    longform._LIVE_RIGHT_SECONDS = 0.5


def read_exact(source, count):
    data = bytearray()
    while len(data) < count:
        chunk = source.read(count - len(data))
        if not chunk:
            raise EOFError("Echo disconnected")
        data.extend(chunk)
    return bytes(data)


def read_frame(source):
    size = struct.unpack("<I", read_exact(source, 4))[0]
    if not 0 < size <= MAX_FRAME:
        raise ValueError("Invalid protocol message size")
    frame = json.loads(read_exact(source, size))
    if not isinstance(frame, dict) or frame.get("protocol") != PROTOCOL:
        raise ValueError("Unsupported protocol version")
    if type(frame.get("session")) is not int or frame["session"] < 0:
        raise ValueError("Invalid session")
    return frame


def write_frame(target, session, kind, text=""):
    if not isinstance(text, str) or len(text) > MAX_TEXT:
        raise ValueError("Transcript exceeded Echo's limit")
    data = json.dumps({"protocol": PROTOCOL, "session": session, "kind": kind, "text": text}, ensure_ascii=False).encode("utf-8")
    if len(data) > MAX_FRAME:
        raise ValueError("Transcript exceeded protocol limit")
    target.write(struct.pack("<I", len(data)))
    target.write(data)
    target.flush()


def block_network():
    """Keep inference offline, including third-party usage reporting and downloads."""
    original_pair = socket.socketpair
    internal = threading.local()
    class OfflineSocket(socket.socket):
        def connect(self, address):
            if getattr(internal, "socketpair", False):
                return super().connect(address)
            raise OSError("Echo GPU inference is offline")

        def connect_ex(self, address):
            raise OSError("Echo GPU inference is offline")

    def no_network(*args, **kwargs):
        raise OSError("Echo GPU inference is offline")

    def local_socketpair(*args, **kwargs):
        # Windows implements asyncio's private wakeup pipe with a loopback
        # socket pair. Permit only this internal construction, never callers'
        # arbitrary connections or DNS queries.
        internal.socketpair = True
        try:
            return original_pair(*args, **kwargs)
        finally:
            internal.socketpair = False

    socket.socket = OfflineSocket
    socket.socketpair = local_socketpair
    socket.create_connection = no_network
    socket.getaddrinfo = no_network
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
    os.environ.pop("MOONDREAM_API_KEY", None)


def gpu_device(torch):
    if not torch.cuda.is_available():
        raise RuntimeError("CUDA is unavailable. Update the NVIDIA driver or select Parakeet V2/V3 for CPU transcription.")
    for index in range(torch.cuda.device_count()):
        if torch.cuda.get_device_capability(index)[0] >= 8:
            return f"cuda:{index}"
    raise RuntimeError("Ultra requires an NVIDIA Ampere or newer GPU. Select Parakeet V2/V3 for CPU transcription.")


def public_error(error):
    # Exception strings from dependencies can contain request data. Never echo those.
    name = type(error).__name__
    if "OutOfMemory" in name:
        return "Insufficient GPU memory. Close other GPU applications and retry, or select Parakeet V2/V3."
    if isinstance(error, (EOFError, BrokenPipeError)):
        return "Echo disconnected from the GPU runtime."
    if isinstance(error, (ValueError, json.JSONDecodeError)):
        return "The GPU runtime received an invalid message or exceeded a transcript limit."
    return f"GPU transcription failed ({name}). Retry, repair the GPU runtime, or select Parakeet V2/V3."


async def transcribe_session(speech, source, target, session, np):
    queue = asyncio.Queue(maxsize=128)

    async def produce():
        while True:
            frame = await asyncio.to_thread(read_frame, source)
            if frame["session"] != session:
                raise ValueError("Stale session")
            if frame.get("kind") == "stop":
                await queue.put(None)
                return
            if frame.get("kind") != "audio":
                raise ValueError("Expected audio")
            samples = frame.get("audio")
            if not isinstance(samples, list) or not 0 < len(samples) <= 8192:
                raise ValueError("Invalid audio chunk")
            if any(type(sample) is not int or not -32768 <= sample <= 32767 for sample in samples):
                raise ValueError("Invalid PCM sample")
            await queue.put(np.asarray(samples, dtype=np.float32) / 32768.0)

    async def chunks():
        while True:
            chunk = await queue.get()
            if chunk is None:
                return
            yield chunk

    async def consume():
        updates = await speech.atranscribe(audio=chunks(), sample_rate=16000, timestamps="none", stream=True)
        async for update in updates:
            write_frame(target, session, "partial", update["text"])
        final = await updates.aresult()
        return final["text"]

    producer = asyncio.create_task(produce())
    consumer = asyncio.create_task(consume())
    try:
        _, final = await asyncio.gather(producer, consumer)
        write_frame(target, session, "final", final)
    finally:
        producer.cancel()
        consumer.cancel()
        await asyncio.gather(producer, consumer, return_exceptions=True)


async def serve(speech, source, target, np):
    last_session = 0
    while True:
        try:
            frame = await asyncio.to_thread(read_frame, source)
        except EOFError:
            return
        if frame.get("kind") == "shutdown" and frame["session"] == 0:
            return
        session = frame["session"]
        if frame.get("kind") != "start" or session <= last_session:
            raise ValueError("Invalid session start")
        last_session = session
        try:
            await transcribe_session(speech, source, target, session, np)
        except Exception as error:
            write_frame(target, session, "error", public_error(error))
            return


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model-dir", type=Path, required=True)
    args = parser.parse_args()
    source, target = sys.stdin.buffer, sys.stdout.buffer
    # Capture dependency output so it cannot corrupt IPC or expose private text.
    quiet = open(os.devnull, "w", encoding="utf-8")
    sys.stdout = sys.stderr = quiet
    block_network()
    try:
        import torch
        import numpy as np
        import moondream as md
        configure_streaming()
        device = gpu_device(torch)
        with md.photon(MODEL, device=device, model_path=str(args.model_dir.resolve()), single_pass_batch_capacity=1) as speech:
            write_frame(target, 0, "ready")
            asyncio.run(serve(speech, source, target, np))
        return 0
    except Exception as error:
        write_frame(target, 0, "error", public_error(error))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
