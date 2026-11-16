from __future__ import annotations

from abc import ABC, abstractmethod

import numpy as np


class TranscriptionError(RuntimeError):
    pass


class Backend(ABC):
    name = "base"

    def warmup(self) -> None:
        """Optional: load models / refresh tokens before the first dictation."""

    @abstractmethod
    def transcribe(self, samples: np.ndarray, sample_rate: int) -> str:
        """Return final text for int16 mono samples."""
