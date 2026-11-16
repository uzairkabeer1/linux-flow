"""Insert text into the focused window on X11 or Wayland."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import threading
import time

TERMINAL_HINTS = (
    "terminal", "konsole", "kitty", "alacritty", "wezterm", "foot", "xterm", "urxvt",
    "tilix", "terminator", "ghostty", "warp", "st-256color", "rio", "ptyxis",
)


def session_type() -> str:
    t = os.environ.get("XDG_SESSION_TYPE", "").lower()
    if t in ("x11", "wayland"):
        return t
    if os.environ.get("WAYLAND_DISPLAY"):
        return "wayland"
    if os.environ.get("DISPLAY"):
        return "x11"
    return "unknown"


def desktop() -> str:
    return (os.environ.get("XDG_CURRENT_DESKTOP") or os.environ.get("DESKTOP_SESSION") or "").lower()


def _have(cmd: str) -> bool:
    return shutil.which(cmd) is not None


def _run(args: list[str], input_text: str | None = None, timeout: float = 10) -> bool:
    try:
        r = subprocess.run(
            args,
            input=input_text.encode() if input_text is not None else None,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        print(f"[inject] {args[0]} failed: {e}", file=sys.stderr)
        return False
    if r.returncode != 0:
        print(f"[inject] {args[0]} exit {r.returncode}: {r.stderr.decode(errors='ignore').strip()}", file=sys.stderr)
    return r.returncode == 0


# --- clipboard --------------------------------------------------------------

def clipboard_get() -> str | None:
    st = session_type()
    try:
        if st == "wayland" and _have("wl-paste"):
            r = subprocess.run(["wl-paste", "--no-newline", "--type", "text"], capture_output=True, timeout=2)
        elif _have("xclip"):
            r = subprocess.run(["xclip", "-selection", "clipboard", "-o"], capture_output=True, timeout=2)
        elif _have("xsel"):
            r = subprocess.run(["xsel", "--clipboard", "--output"], capture_output=True, timeout=2)
        else:
            return None
    except (OSError, subprocess.TimeoutExpired):
        return None
    return r.stdout.decode(errors="ignore") if r.returncode == 0 else None


def clipboard_set(text: str) -> bool:
    st = session_type()
    if st == "wayland" and _have("wl-copy"):
        # wl-copy forks to serve the selection; don't wait on it
        try:
            p = subprocess.Popen(["wl-copy", "--type", "text/plain;charset=utf-8"], stdin=subprocess.PIPE)
            p.stdin.write(text.encode())
            p.stdin.close()
            p.wait(timeout=2)
            return True
        except (OSError, subprocess.TimeoutExpired):
            return True  # still serving in background
    if _have("xclip"):
        return _run(["xclip", "-selection", "clipboard", "-i"], text)
    if _have("xsel"):
        return _run(["xsel", "--clipboard", "--input"], text)
    print("[inject] no clipboard tool (install wl-clipboard or xclip)", file=sys.stderr)
    return False


# --- key chords -------------------------------------------------------------

def _active_window_is_terminal() -> bool:
    if session_type() != "x11" or not _have("xdotool"):
        return False
    try:
        r = subprocess.run(["xdotool", "getactivewindow", "getwindowclassname"], capture_output=True, timeout=1)
    except (OSError, subprocess.TimeoutExpired):
        return False
    cls = r.stdout.decode(errors="ignore").lower()
    return any(h in cls for h in TERMINAL_HINTS)


_uinput = None
_uinput_lock = threading.Lock()


def _uinput_paste(shift: bool) -> bool:
    """Send Ctrl(+Shift)+V via a virtual keyboard (needs write access to /dev/uinput)."""
    global _uinput
    try:
        from evdev import UInput, ecodes as e
    except ImportError:
        return False
    with _uinput_lock:
        try:
            if _uinput is None:
                _uinput = UInput(
                    {e.EV_KEY: [e.KEY_LEFTCTRL, e.KEY_LEFTSHIFT, e.KEY_V]}, name="linux-flow-vkbd"
                )
                time.sleep(0.15)  # let the compositor pick up the new device
            ui = _uinput
            mods = [e.KEY_LEFTCTRL] + ([e.KEY_LEFTSHIFT] if shift else [])
            for m in mods:
                ui.write(e.EV_KEY, m, 1)
            ui.write(e.EV_KEY, e.KEY_V, 1)
            ui.syn()
            ui.write(e.EV_KEY, e.KEY_V, 0)
            for m in reversed(mods):
                ui.write(e.EV_KEY, m, 0)
            ui.syn()
            return True
        except (OSError, PermissionError) as ex:
            print(f"[inject] uinput unavailable: {ex}", file=sys.stderr)
            return False


def send_paste_chord(shift: bool = False) -> bool:
    st = session_type()
    if st == "x11" and _have("xdotool"):
        return _run(["xdotool", "key", "--clearmodifiers", "ctrl+shift+v" if shift else "ctrl+v"])
    if _uinput_paste(shift):
        return True
    if _have("ydotool"):
        # 29=LEFTCTRL 42=LEFTSHIFT 47=V
        seq = ["29:1"] + (["42:1"] if shift else []) + ["47:1", "47:0"] + (["42:0"] if shift else []) + ["29:0"]
        return _run(["ydotool", "key", *seq])
    if st == "wayland" and _have("wtype"):
        args = ["wtype", "-M", "ctrl"] + (["-M", "shift"] if shift else []) + ["v", "-m", "ctrl"]
        if shift:
            args += ["-m", "shift"]
        return _run(args)
    return False


# --- typing tools -----------------------------------------------------------

TYPERS = {
    "xdotool": lambda t: ["xdotool", "type", "--clearmodifiers", "--delay", "1", "--", t],
    "wtype": lambda t: ["wtype", "--", t],
    "ydotool": lambda t: ["ydotool", "type", "--", t],
}

# stderr fragments meaning the tool failed before typing anything
_NOTHING_TYPED = (
    "virtual keyboard",           # wtype on GNOME/KDE
    "failed to connect",          # ydotool without ydotoold
    "can't open display", "cannot open display",  # xdotool without X
    "no such file", "permission denied",
)


def _type_with(method: str, text: str) -> str:
    """Run a typing tool. Returns "ok", "nothing" (safe to fall back) or "partial"."""
    args = TYPERS[method](text)
    if not _have(args[0]):
        return "nothing"
    timeout = 30 + len(text) * 0.05  # generous: never kill it mid-sentence
    try:
        r = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=timeout)
    except OSError as e:
        print(f"[inject] {args[0]} failed to start: {e}", file=sys.stderr)
        return "nothing"
    except subprocess.TimeoutExpired:
        print(f"[inject] {args[0]} timed out after {timeout:.0f}s", file=sys.stderr)
        return "partial"
    if r.returncode == 0:
        return "ok"
    err = r.stderr.decode(errors="ignore").strip()
    print(f"[inject] {args[0]} exit {r.returncode}: {err}", file=sys.stderr)
    return "nothing" if any(s in err.lower() for s in _NOTHING_TYPED) else "partial"


# --- public API -------------------------------------------------------------

class Injector:
    def __init__(self, method: str = "auto", restore_clipboard: bool = True):
        self.method = method
        self.restore_clipboard = restore_clipboard

    def resolve_method(self) -> str:
        if self.method != "auto":
            return self.method
        st = session_type()
        if st == "x11" and _have("xdotool"):
            return "xdotool"
        if st == "wayland":
            # wtype is great on wlroots compositors (sway, Hyprland, river...) but
            # GNOME/KDE lack the virtual-keyboard protocol it needs -> paste instead.
            if _have("wtype") and not any(d in desktop() for d in ("gnome", "kde", "plasma")):
                return "wtype"
            return "paste"
        return "paste"

    def insert(self, text: str) -> str:
        """Insert text exactly once. Returns the method used.

        A typing tool that fails may already have typed part of the text, so we only
        fall back to pasting when it provably typed nothing (tool missing, can't
        connect, compositor unsupported). Otherwise pasting would duplicate the text.
        """
        if not text:
            return "none"
        method = self.resolve_method()
        if method == "clipboard":
            return "clipboard" if clipboard_set(text) else "clipboard-only"
        if method in TYPERS:
            result = _type_with(method, text)
            if result == "ok":
                return method
            if result == "partial":
                clipboard_set(text)
                return f"{method}-partial"
            # result == "nothing": safe to try pasting instead
        if self._paste(text):
            return "paste"
        clipboard_set(text)
        return "clipboard-only"

    def _paste(self, text: str) -> bool:
        previous = clipboard_get() if self.restore_clipboard else None
        if not clipboard_set(text):
            return False
        time.sleep(0.05)
        ok = send_paste_chord(shift=_active_window_is_terminal())
        if ok and previous is not None:
            def restore():
                time.sleep(0.6)  # give the target app time to read the clipboard
                clipboard_set(previous)
            threading.Thread(target=restore, daemon=True).start()
        return ok
