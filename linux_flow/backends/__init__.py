from __future__ import annotations

from linux_flow.backends.base import Backend, TranscriptionError
from linux_flow.config import Config


def make_backend(name: str, cfg: Config) -> Backend:
    name = (name or "").lower()
    if name == "wispr":
        from linux_flow.backends.wispr import WisprBackend

        return WisprBackend(cfg)
    if name == "local":
        from linux_flow.backends.local import LocalWhisperBackend

        return LocalWhisperBackend(cfg)
    raise ValueError(f"unknown backend: {name!r} (expected 'wispr' or 'local')")


__all__ = ["Backend", "TranscriptionError", "make_backend"]
