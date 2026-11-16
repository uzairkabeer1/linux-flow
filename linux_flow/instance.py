"""Single-instance guard, so autostart + a manual `linux-flow run` can't both dictate."""

from __future__ import annotations

import os
import signal
from pathlib import Path

from linux_flow.config import state_dir

_held = None  # keeps the locked file object alive for the life of the process


def lock_path() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    return Path(runtime) / "linux-flow.lock" if runtime else state_dir() / "run.lock"


def running_pid() -> int | None:
    """PID of the running instance, or None."""
    p = lock_path()
    if not p.exists():
        return None
    try:
        import fcntl
    except ImportError:  # not Linux
        return None
    with open(p, "a+") as f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            f.seek(0)
            txt = f.read().strip()
            return int(txt) if txt.isdigit() else -1
        fcntl.flock(f, fcntl.LOCK_UN)
    return None


def acquire() -> None:
    """Take the lock or exit with a helpful message if another instance holds it."""
    global _held
    try:
        import fcntl
    except ImportError:
        return
    p = lock_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    f = open(p, "a+")
    try:
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        f.seek(0)
        pid = f.read().strip() or "?"
        f.close()
        raise SystemExit(
            f"linux-flow is already running (pid {pid}), probably from autostart.\n"
            "Just use the hotkey, or run `linux-flow stop` first to restart it here."
        )
    f.seek(0)
    f.truncate()
    f.write(str(os.getpid()))
    f.flush()
    _held = f


def stop() -> int | None:
    """SIGTERM the running instance. Returns its pid, or None if none was running."""
    pid = running_pid()
    if not pid or pid < 0:
        return None
    os.kill(pid, signal.SIGTERM)
    return pid
