"""Offline fallback using faster-whisper."""

from __future__ import annotations

import numpy as np

from linux_flow.backends.base import Backend, TranscriptionError
from linux_flow.config import Config


class LocalWhisperBackend(Backend):
    name = "local"

    def __init__(self, cfg: Config):
        self.cfg = cfg
        self._model = None

    def warmup(self) -> None:
        if self._model is not None:
            return
        try:
            from faster_whisper import WhisperModel
        except ImportError as e:
            raise TranscriptionError("local backend needs: pip install 'linux-flow[local]'") from e
        lc = self.cfg.local
        device = lc.device
        if device == "auto":
            try:
                import ctranslate2

                device = "cuda" if ctranslate2.get_cuda_device_count() > 0 else "cpu"
            except Exception:
                device = "cpu"
        compute = lc.compute_type
        if compute == "auto":
            compute = "float16" if device == "cuda" else "int8"
        self._model = WhisperModel(lc.model, device=device, compute_type=compute)

    def transcribe(self, samples: np.ndarray, sample_rate: int) -> str:
        self.warmup()
        if sample_rate != 16000:
            n = int(len(samples) * 16000 / sample_rate)
            samples = np.interp(np.linspace(0, len(samples) - 1, n), np.arange(len(samples)), samples)
        audio = samples.astype(np.float32) / 32768.0
        segments, _ = self._model.transcribe(
            audio,
            language=self.cfg.general.language or None,
            vad_filter=True,
            beam_size=5,
        )
        return " ".join(s.text.strip() for s in segments).strip()
