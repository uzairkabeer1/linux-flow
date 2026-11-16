"""Audible cues and desktop notifications."""

from __future__ import annotations

import shutil
import subprocess
import threading

import numpy as np


def _tone(freqs: list[float], dur: float = 0.07, rate: int = 44100, vol: float = 0.18) -> np.ndarray:
    parts = []
    for f in freqs:
        t = np.linspace(0, dur, int(rate * dur), endpoint=False)
        env = np.minimum(1, np.minimum(t, dur - t) / 0.01)  # 10 ms fade in/out
        parts.append(np.sin(2 * np.pi * f * t) * env * vol)
    return np.concatenate(parts).astype(np.float32)


class Feedback:
    def __init__(self, sounds: bool = True, notifications: bool = True):
        self.sounds = sounds
        self.notifications = notifications
        self._start = _tone([660, 990])
        self._stop = _tone([990, 660])
        self._error = _tone([300, 220], dur=0.12)

    def _play(self, wave: np.ndarray) -> None:
        if not self.sounds:
            return

        def go():
            try:
                import sounddevice as sd

                sd.play(wave, 44100, blocking=True)
            except Exception:
                pass

        threading.Thread(target=go, daemon=True).start()

    def started(self) -> None:
        self._play(self._start)

    def stopped(self) -> None:
        self._play(self._stop)

    def error(self, message: str) -> None:
        self._play(self._error)
        self.notify("linux-flow", message, urgency="critical")

    def notify(self, title: str, body: str, urgency: str = "normal") -> None:
        if not self.notifications or not shutil.which("notify-send"):
            return
        try:
            subprocess.Popen(
                ["notify-send", "-a", "linux-flow", "-u", urgency, "-t", "4000", title, body],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        except OSError:
            pass
