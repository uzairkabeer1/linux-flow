"""Main dictation loop: hotkey -> record -> transcribe -> (cleanup) -> type."""

from __future__ import annotations

import queue
import sys
import threading
import time

import numpy as np

from linux_flow import audio
from linux_flow.backends import Backend, TranscriptionError, make_backend
from linux_flow.cleanup import Cleaner
from linux_flow.config import Config
from linux_flow.feedback import Feedback
from linux_flow.hotkey import HotkeyListener
from linux_flow.inject import Injector, session_type


def log(msg: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


class DictationApp:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.recorder = audio.Recorder(cfg.audio.sample_rate, cfg.audio.device, cfg.audio.max_seconds)
        self.feedback = Feedback(cfg.feedback.sounds, cfg.feedback.notifications)
        self.injector = Injector(cfg.output.method, cfg.output.restore_clipboard)
        self.cleaner = Cleaner(cfg.cleanup) if cfg.cleanup.enabled else None
        self.primary: Backend = make_backend(cfg.general.backend, cfg)
        self.fallback: Backend | None = (
            make_backend(cfg.general.fallback_backend, cfg) if cfg.general.fallback_backend else None
        )
        self._jobs: queue.Queue[np.ndarray | None] = queue.Queue()
        self._lock = threading.Lock()
        self._pressed_at = 0.0
        self.hotkey = HotkeyListener(cfg.hotkey.keys, self._on_down, self._on_up)

    # --- hotkey handlers (run on listener thread; keep fast) -----------------
    def _on_down(self) -> None:
        with self._lock:
            if self.cfg.hotkey.mode == "toggle":
                if self.recorder.recording:
                    self._finish()
                else:
                    self._begin()
            else:
                self._begin()

    def _on_up(self) -> None:
        if self.cfg.hotkey.mode == "toggle":
            return
        with self._lock:
            if not self.recorder.recording:
                return
            held_ms = (time.monotonic() - self._pressed_at) * 1000
            if held_ms < self.cfg.hotkey.min_hold_ms:
                self.recorder.stop()
                log(f"tap ({held_ms:.0f} ms) ignored")
                return
            self._finish()

    def _begin(self) -> None:
        if self.recorder.recording:
            return
        try:
            self.recorder.start()
        except Exception as e:
            log(f"mic error: {e}")
            self.feedback.error(f"Microphone error: {e}")
            return
        self._pressed_at = time.monotonic()
        self.feedback.started()
        log("recording...")

    def _finish(self) -> None:
        samples = self.recorder.stop()
        self.feedback.stopped()
        log(f"captured {len(samples) / self.cfg.audio.sample_rate:.1f}s")
        self._jobs.put(samples)

    # --- worker -------------------------------------------------------------
    def _watchdog(self) -> None:
        while True:
            time.sleep(0.5)
            if self.recorder.over_limit():
                with self._lock:
                    if self.recorder.over_limit():
                        log("max duration reached, stopping")
                        self._finish()

    def _worker(self) -> None:
        while True:
            samples = self._jobs.get()
            if samples is None:
                return
            try:
                self._process(samples)
            except Exception as e:  # never let the worker die
                log(f"error: {e}")
                self.feedback.error(str(e)[:200])

    def _transcribe(self, samples: np.ndarray) -> str:
        rate = self.cfg.audio.sample_rate
        try:
            return self.primary.transcribe(samples, rate)
        except TranscriptionError as e:
            if not self.fallback:
                raise
            log(f"{self.primary.name} failed ({e}); falling back to {self.fallback.name}")
            return self.fallback.transcribe(samples, rate)

    def _process(self, samples: np.ndarray) -> None:
        if audio.is_silent(samples):
            log("silence, skipped")
            return
        t0 = time.monotonic()
        text = self._transcribe(samples).strip()
        t1 = time.monotonic()
        if self.cleaner and text:
            try:
                text = self.cleaner.clean(text)
            except Exception as e:
                log(f"cleanup failed, using raw text: {e}")
        if not text:
            log("empty transcript")
            return
        if self.cfg.output.trailing_space and not text.endswith((" ", "\n")):
            text += " "
        method = self.injector.insert(text)
        log(f"[{t1 - t0:.2f}s, {method}] {text.strip()}")
        if method == "clipboard-only":
            self.feedback.notify("linux-flow", "Couldn't type into the window; text is on your clipboard.")
        elif method.endswith("-partial"):
            self.feedback.notify("linux-flow", "Typing may be incomplete; the full text is on your clipboard.")

    # --- run ----------------------------------------------------------------
    def run(self) -> None:
        import signal

        from linux_flow import instance

        instance.acquire()  # exits if autostart (or another terminal) is already running one
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))  # `linux-flow stop` -> clean exit
        log(f"session={session_type()} backend={self.primary.name} output={self.injector.resolve_method()}")
        try:
            self.primary.warmup()
        except TranscriptionError as e:
            log(f"warmup: {e}")
            if not self.fallback:
                raise SystemExit(str(e))
        threading.Thread(target=self._worker, daemon=True).start()
        threading.Thread(target=self._watchdog, daemon=True).start()
        keys = "+".join(self.cfg.hotkey.keys)
        verb = "Hold" if self.cfg.hotkey.mode == "hold" else "Press"
        log(f"ready. {verb} {keys} to dictate. Ctrl+C to quit.")
        try:
            self.hotkey.run()
        except KeyboardInterrupt:
            pass
        finally:
            self.hotkey.stop()
            self._jobs.put(None)
            print(file=sys.stderr)
