"""Command-line entry point."""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

from linux_flow import __version__, config


def cmd_run(args, cfg) -> int:
    from linux_flow.app import DictationApp

    DictationApp(cfg).run()
    return 0


def cmd_login(args, cfg) -> int:
    from linux_flow.backends import wispr

    wispr.interactive_login()
    return 0


def cmd_logout(args, cfg) -> int:
    from linux_flow.backends import wispr

    wispr.logout()
    print("Signed out.")
    return 0


def cmd_stop(args, cfg) -> int:
    from linux_flow import instance

    pid = instance.stop()
    if pid is None:
        print("linux-flow is not running.")
        return 0
    for _ in range(30):
        if instance.running_pid() is None:
            print(f"Stopped linux-flow (pid {pid}).")
            return 0
        time.sleep(0.1)
    print(f"Sent stop to pid {pid}; it hasn't exited yet.")
    return 1


def cmd_status(args, cfg) -> int:
    from linux_flow import instance
    from linux_flow.backends import wispr
    from linux_flow.inject import Injector, desktop, session_type

    pid = instance.running_pid()
    print(f"linux-flow {__version__}")
    print(f"running:  {f'yes (pid {pid})' if pid else 'no'}")
    print(f"config:   {config.config_dir() / 'config.toml'}")
    print(f"session:  {session_type()} ({desktop() or 'unknown desktop'})")
    print(f"output:   {Injector(cfg.output.method).resolve_method()}")
    print(f"backend:  {cfg.general.backend}" + (f" (fallback: {cfg.general.fallback_backend})" if cfg.general.fallback_backend else ""))
    print(f"hotkey:   {'+'.join(cfg.hotkey.keys)} ({cfg.hotkey.mode})")
    print(f"wispr:    {wispr.describe_session()}")
    return 0


def cmd_keys(args, cfg) -> int:
    from linux_flow.hotkey import watch_keys

    watch_keys()
    return 0


def cmd_test_mic(args, cfg) -> int:
    import numpy as np

    from linux_flow.audio import Recorder, to_wav

    rec = Recorder(cfg.audio.sample_rate, cfg.audio.device)
    print(f"Recording {args.seconds}s... speak now")
    rec.start()
    end = time.monotonic() + args.seconds
    while time.monotonic() < end:
        bar = "#" * int(min(rec.level * 300, 50))
        print(f"\r  level |{bar:<50}|", end="", flush=True)
        time.sleep(0.05)
    samples = rec.stop()
    print()
    peak = int(np.abs(samples).max()) if samples.size else 0
    print(f"captured {samples.size / cfg.audio.sample_rate:.1f}s, peak {peak}/32767")
    if args.out:
        Path(args.out).write_bytes(to_wav(samples, cfg.audio.sample_rate))
        print(f"saved {args.out}")
    if args.transcribe:
        _transcribe_and_print(samples, cfg, args.backend)
    return 0


def cmd_transcribe(args, cfg) -> int:
    from linux_flow.audio import read_wav

    samples = read_wav(args.file, cfg.audio.sample_rate)
    _transcribe_and_print(samples, cfg, args.backend)
    return 0


def _transcribe_and_print(samples, cfg, backend_name: str | None) -> None:
    from linux_flow.backends import make_backend

    backend = make_backend(backend_name or cfg.general.backend, cfg)
    t0 = time.monotonic()
    text = backend.transcribe(samples, cfg.audio.sample_rate)
    print(f"[{backend.name}, {time.monotonic() - t0:.2f}s] {text}")


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="linux-flow", description="Hold-to-talk dictation for Linux")
    p.add_argument("--config", type=Path, help="path to config.toml")
    p.add_argument("--version", action="version", version=__version__)
    sub = p.add_subparsers(dest="cmd")

    sub.add_parser("run", help="start the dictation daemon (default)")
    sub.add_parser("login", help="sign in to your Wispr Flow account (opens browser)")
    sub.add_parser("logout", help="forget stored Wispr credentials")
    sub.add_parser("stop", help="stop the running instance (e.g. the autostart one)")
    sub.add_parser("status", help="show detected environment and settings")
    sub.add_parser("keys", help="print key names as you press them (to pick a hotkey)")

    tm = sub.add_parser("test-mic", help="record a few seconds and show levels")
    tm.add_argument("--seconds", type=float, default=4)
    tm.add_argument("--out", help="save the recording as WAV")
    tm.add_argument("--transcribe", action="store_true", help="also transcribe it")
    tm.add_argument("--backend", help="override backend (wispr/local)")

    tr = sub.add_parser("transcribe", help="transcribe a 16-bit WAV file")
    tr.add_argument("file")
    tr.add_argument("--backend", help="override backend (wispr/local)")

    args = p.parse_args(argv)
    cfg = config.load(args.config)
    handlers = {
        None: cmd_run, "run": cmd_run, "login": cmd_login, "logout": cmd_logout, "stop": cmd_stop,
        "status": cmd_status, "keys": cmd_keys, "test-mic": cmd_test_mic, "transcribe": cmd_transcribe,
    }
    try:
        return handlers[args.cmd](args, cfg)
    except KeyboardInterrupt:
        return 130
    except Exception as e:  # surface a clean message
        from linux_flow.backends import TranscriptionError

        if isinstance(e, (TranscriptionError, ValueError)):
            print(f"error: {e}", file=sys.stderr)
            return 1
        raise
