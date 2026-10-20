"""Synthetic native-history paging benchmark; never opens the user's history.

Run in Hermes' managed Python with integration/hermes on PYTHONPATH. The private
fixture has 100,000 messages, 50 sessions and over 1 GiB of source text. FTS
triggers are omitted only while building the fixture: this benchmarks incremental
history reads, not Hermes indexing, model latency or desktop frame rate.
"""
import json
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import statistics
import tempfile
import time
import tracemalloc


def main():
    with tempfile.TemporaryDirectory(prefix="echo-history-benchmark-") as temporary:
        os.environ["HERMES_HOME"] = temporary
        from hermes_state import SessionDB
        from echo_bridge.history import history_page
        import psutil
        db = SessionDB()
        sessions = [f"echo-benchmark-{n}" for n in range(50)]
        for session in sessions:
            db.create_session(session, "echo")
        path = db.db_path
        db.close()
        payload = "# Notes\n\n漢字👩🏽‍💻 **bold** [unfinished](\n```rust\n" + "x" * 10740
        byte_count = 0
        with closing(sqlite3.connect(path)) as conn:
            # Removing indexing triggers in this disposable copy makes fixture
            # creation independent of unrelated FTS indexing throughput.
            triggers = conn.execute("SELECT name FROM sqlite_master WHERE type='trigger'").fetchall()
            for (name,) in triggers:
                conn.execute('DROP TRIGGER "' + name.replace('"', '""') + '"')
            for batch in range(100):
                conn.executemany("INSERT INTO messages(session_id,role,content,timestamp) VALUES(?,?,?,?)",
                    ((sessions[n % 50], "assistant" if n % 2 else "user", payload, n)
                     for n in range(batch * 1000, (batch + 1) * 1000)))
                conn.commit()
                byte_count += 1000 * len(payload.encode())
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        process = psutil.Process()
        baseline_rss = process.memory_info().rss
        tracemalloc.start()
        times = []
        peak_blocks = 0
        for session in sessions:
            cursor = {"message": 0, "end": 0}
            for _ in range(4):
                started = time.perf_counter()
                page = history_page(session, cursor["message"], cursor["end"], 120)
                times.append((time.perf_counter() - started) * 1000)
                peak_blocks = max(peak_blocks, len(page["blocks"]))
                assert len(page["blocks"]) <= 120
                assert all(len(block["text"].encode()) <= 4096 for block in page["blocks"])
                cursor = page["older"]
                if cursor is None:
                    break
        _, python_peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        report = {"messages": 100000, "sessions": 50, "source_bytes": byte_count,
                  "pages": len(times), "maximum_blocks": peak_blocks,
                  "page_median_ms": round(statistics.median(times), 2),
                  "page_p95_ms": round(sorted(times)[int(len(times) * .95)], 2),
                  "python_peak_bytes": python_peak,
                  "rss_growth_bytes": process.memory_info().rss - baseline_rss,
                  "scope": "native history paging; excludes FTS indexing and UI frame rate"}
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
