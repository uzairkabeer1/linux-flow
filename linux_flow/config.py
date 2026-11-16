"""Config loading. Lives at $XDG_CONFIG_HOME/linux-flow/config.toml."""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass, field, fields
from pathlib import Path

if sys.version_info >= (3, 11):
    import tomllib
else:  # pragma: no cover
    import tomli as tomllib


def config_dir() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return Path(base) / "linux-flow"


def state_dir() -> Path:
    base = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
    return Path(base) / "linux-flow"


DEFAULT_CONFIG = """\
# linux-flow configuration

[general]
# Speech engine: "wispr" (your Wispr Flow account) or "local" (faster-whisper, offline)
backend = "wispr"
# If the primary backend fails, try this one instead ("" to disable)
fallback_backend = ""
# Language hint, e.g. "en". Empty = auto-detect.
language = ""

[hotkey]
# evdev key names (see `linux-flow keys`). All keys in the list must be held.
keys = ["KEY_RIGHTCTRL"]
# "hold"   = talk while held, release to transcribe (push-to-talk)
# "toggle" = press once to start, press again to stop
mode = "hold"
# Ignore presses shorter than this (ms) in hold mode
min_hold_ms = 250

[audio]
sample_rate = 16000
# sounddevice input device name or index; empty = system default
device = ""
# Stop automatically after this many seconds
max_seconds = 360

[output]
# "auto" picks per session: xdotool on X11, wtype/ydotool on Wayland.
# "paste" copies to clipboard and sends Ctrl+V (most reliable for unicode).
# Or force one of: "xdotool", "wtype", "ydotool", "clipboard" (copy only)
method = "auto"
# Restore previous clipboard contents after paste
restore_clipboard = true
# Append a trailing space after inserted text
trailing_space = true

[feedback]
sounds = true
notifications = true

[wispr]
# Language codes, e.g. ["en"]. Empty = auto-detect.
languages = []
# Let Wispr's server-side AI format the text (punctuation, lists, filler removal)
formatting = true
# Context hint for formatting: "other", "email", "ai", "terminal"
app_type = "other"
# Reported to Wispr's servers. "win32" mirrors the official Windows app (safest).
client_platform = "win32"
client_version = "1.6.1483"

[local]
# faster-whisper model: tiny, base, small, medium, large-v3, distil-large-v3 ...
model = "base"
device = "auto"         # "cpu", "cuda", or "auto"
compute_type = "auto"   # "int8", "float16", ...

[cleanup]
# Optional LLM polish pass on the final text (Wispr already formats server-side).
enabled = false
# "anthropic" (Claude, needs: pip install 'linux-flow[cleanup]')
# or "openai" (any OpenAI-compatible server, e.g. local Ollama)
provider = "anthropic"
# Claude model id. claude-haiku-5-5 is cheaper/faster if you prefer.
model = "claude-opus-5-5"
# For provider = "openai": e.g. "http://localhost:11434/v1"
base_url = ""
# Read from this env var (never store keys in this file if you can avoid it)
api_key_env = "ANTHROPIC_API_KEY"
"""


@dataclass
class General:
    backend: str = "wispr"
    fallback_backend: str = ""
    language: str = ""


@dataclass
class Hotkey:
    keys: list[str] = field(default_factory=lambda: ["KEY_RIGHTCTRL"])
    mode: str = "hold"
    min_hold_ms: int = 250


@dataclass
class Audio:
    sample_rate: int = 16000
    device: str = ""
    max_seconds: int = 360


@dataclass
class Output:
    method: str = "auto"
    restore_clipboard: bool = True
    trailing_space: bool = True


@dataclass
class Feedback:
    sounds: bool = True
    notifications: bool = True


@dataclass
class Wispr:
    languages: list[str] = field(default_factory=list)
    formatting: bool = True
    app_type: str = "other"
    client_platform: str = "win32"
    client_version: str = "1.6.1483"


@dataclass
class Local:
    model: str = "base"
    device: str = "auto"
    compute_type: str = "auto"


@dataclass
class Cleanup:
    enabled: bool = False
    provider: str = "anthropic"
    model: str = "claude-opus-5-5"
    base_url: str = ""
    api_key_env: str = "ANTHROPIC_API_KEY"


@dataclass
class Config:
    general: General = field(default_factory=General)
    hotkey: Hotkey = field(default_factory=Hotkey)
    audio: Audio = field(default_factory=Audio)
    output: Output = field(default_factory=Output)
    feedback: Feedback = field(default_factory=Feedback)
    wispr: Wispr = field(default_factory=Wispr)
    local: Local = field(default_factory=Local)
    cleanup: Cleanup = field(default_factory=Cleanup)


def _apply(section_obj, data: dict) -> None:
    known = {f.name for f in fields(section_obj)}
    for k, v in data.items():
        if k in known:
            setattr(section_obj, k, v)
        else:
            print(f"[config] ignoring unknown key: {k}", file=sys.stderr)


def load(path: Path | None = None) -> Config:
    path = path or config_dir() / "config.toml"
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(DEFAULT_CONFIG, encoding="utf-8")
    raw = tomllib.loads(path.read_text(encoding="utf-8"))
    cfg = Config()
    for f in fields(cfg):
        if f.name in raw:
            _apply(getattr(cfg, f.name), raw[f.name])
    return cfg
