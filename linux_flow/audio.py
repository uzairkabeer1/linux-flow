"""Microphone capture -> 16 kHz mono int16 PCM."""

from __future__ import annotations

import io
import threading
import time
import wave

import numpy as np


class Recorder:
    def __init__(self, sample_rate: int = 16000, device: str | int | None = None, max_seconds: int = 360):
        self.sample_rate = sample_rate
        self.device = _parse_device(device)
        self.max_seconds = max_seconds
        self._chunks: list[np.ndarray] = []
        self._lock = threading.Lock()
        self._stream = None
        self._started = 0.0
        self.level = 0.0  # last RMS, 0..1, for UI

    @property
    def recording(self) -> bool:
        return self._stream is not None

    def start(self) -> None:
        import sounddevice as sd

        if self._stream is not None:
            return
        with self._lock:
            self._chunks = []
        self._started = time.monotonic()
        self._stream = sd.InputStream(
            samplerate=self.sample_rate,
            channels=1,
            dtype="int16",
            device=self.device,
            blocksize=int(self.sample_rate * 0.02),  # 20 ms
            callback=self._callback,
        )
        self._stream.start()

    def _callback(self, indata, frames, time_info, status) -> None:  # noqa: ARG002
        block = indata[:, 0].copy()
        with self._lock:
            self._chunks.append(block)
        self.level = float(np.sqrt(np.mean((block.astype(np.float32) / 32768.0) ** 2)))

    def elapsed(self) -> float:
        return time.monotonic() - self._started if self._stream else 0.0

    def over_limit(self) -> bool:
        return self.recording and self.elapsed() >= self.max_seconds

    def stop(self) -> np.ndarray:
        """Stop and return int16 mono samples."""
        if self._stream is None:
            return np.zeros(0, dtype=np.int16)
        try:
            self._stream.stop()
            self._stream.close()
        finally:
            self._stream = None
        with self._lock:
            data = np.concatenate(self._chunks) if self._chunks else np.zeros(0, dtype=np.int16)
            self._chunks = []
        return data


def _parse_device(device):
    if device in (None, ""):
        return None
    try:
        return int(device)
    except (TypeError, ValueError):
        return device


def to_wav(samples: np.ndarray, sample_rate: int = 16000) -> bytes:
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(samples.astype(np.int16).tobytes())
    return buf.getvalue()


def read_wav(path: str, target_rate: int = 16000) -> np.ndarray:
    """Read a WAV file into int16 mono at target_rate (simple linear resample)."""
    with wave.open(path, "rb") as w:
        rate, ch, width = w.getframerate(), w.getnchannels(), w.getsampwidth()
        raw = w.readframes(w.getnframes())
    if width != 2:
        raise ValueError("only 16-bit WAV supported")
    data = np.frombuffer(raw, dtype=np.int16)
    if ch > 1:
        data = data.reshape(-1, ch).mean(axis=1).astype(np.int16)
    if rate != target_rate and len(data):
        n = int(len(data) * target_rate / rate)
        data = np.interp(np.linspace(0, len(data) - 1, n), np.arange(len(data)), data).astype(np.int16)
    return data


def is_silent(samples: np.ndarray, threshold: float = 0.004) -> bool:
    if samples.size == 0:
        return True
    rms = float(np.sqrt(np.mean((samples.astype(np.float32) / 32768.0) ** 2)))
    return rms < threshold
