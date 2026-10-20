"""Exit fallback through native cancellation, independent of HTTP availability."""
from .native import board_connection, stop_task


def main():
    with board_connection() as conn:
        ids = [row[0] for row in conn.execute("SELECT id FROM tasks WHERE status NOT IN ('done','archived')")]
    confirmed = True
    for task in ids:
        try:
            confirmed = stop_task(task)["stopped"] and confirmed
        except Exception:
            confirmed = False
    return 0 if confirmed else 1


if __name__ == "__main__":
    raise SystemExit(main())
