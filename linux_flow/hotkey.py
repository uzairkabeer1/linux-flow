"""Global hotkey listener.

Primary: evdev (reads /dev/input directly, works on X11 *and* Wayland; user must be
in the `input` group). Fallback: pynput (X11 only).
"""

from __future__ import annotations

import selectors
import sys
import threading
import time
from typing import Callable

Callback = Callable[[], None]

# Virtual keyboards that send synthetic keystrokes (ours and ydotool's)
IGNORED_DEVICES = {"linux-flow-vkbd", "ydotoold virtual device"}


class HotkeyListener:
    def __init__(self, keys: list[str], on_down: Callback, on_up: Callback):
        if not keys:
            raise ValueError("hotkey.keys must not be empty")
        self.keys = [k.upper() for k in keys]
        self.on_down = on_down
        self.on_up = on_up
        self._held: set[str] = set()
        self._active = False
        self._stop = threading.Event()

    # --- shared combo logic -------------------------------------------------
    def _press(self, name: str) -> None:
        if name not in self.keys:
            return
        self._held.add(name)
        if not self._active and all(k in self._held for k in self.keys):
            self._active = True
            self.on_down()

    def _release(self, name: str) -> None:
        if name not in self.keys:
            return
        self._held.discard(name)
        if self._active:
            self._active = False
            self.on_up()

    def stop(self) -> None:
        self._stop.set()

    # --- backends -----------------------------------------------------------
    def run(self) -> None:
        try:
            self._run_evdev()
        except _EvdevUnavailable as e:
            print(f"[hotkey] evdev unavailable ({e}); trying pynput (X11 only)", file=sys.stderr)
            self._run_pynput()

    def _run_evdev(self) -> None:
        try:
            import evdev
            from evdev import ecodes
        except ImportError as e:
            raise _EvdevUnavailable("python-evdev not installed") from e

        wanted_codes = set()
        for k in self.keys:
            code = ecodes.ecodes.get(k)
            if code is None:
                raise ValueError(f"unknown evdev key name: {k} (run `linux-flow keys`)")
            wanted_codes.add(code)

        sel = selectors.DefaultSelector()
        open_paths: dict[str, object] = {}

        def rescan() -> None:
            for path in evdev.list_devices():
                if path in open_paths:
                    continue
                try:
                    dev = evdev.InputDevice(path)
                except (PermissionError, OSError):
                    continue
                caps = dev.capabilities().get(ecodes.EV_KEY, [])
                synthetic = dev.name in IGNORED_DEVICES  # our own Ctrl+V must never trigger the hotkey
                if wanted_codes.issubset(caps) and not synthetic:
                    sel.register(dev, selectors.EVENT_READ)
                    open_paths[path] = dev
                else:
                    dev.close()

        rescan()
        if not open_paths:
            if not evdev.list_devices():
                raise _EvdevUnavailable("no readable /dev/input devices (add yourself to the 'input' group)")
            raise _EvdevUnavailable("no keyboard exposes the configured keys")

        last_scan = time.monotonic()
        while not self._stop.is_set():
            for key, _ in sel.select(timeout=1.0):
                dev = key.fileobj
                try:
                    for ev in dev.read():
                        if ev.type != ecodes.EV_KEY:
                            continue
                        name = ecodes.KEY.get(ev.code) or ecodes.BTN.get(ev.code)
                        if isinstance(name, list):
                            name = next((n for n in name if n in self.keys), name[0])
                        if ev.value == 1:
                            self._press(name)
                        elif ev.value == 0:
                            self._release(name)
                except OSError:  # unplugged
                    sel.unregister(dev)
                    open_paths.pop(dev.path, None)
            if time.monotonic() - last_scan > 5:
                rescan()
                last_scan = time.monotonic()

    def _run_pynput(self) -> None:
        try:
            from pynput import keyboard
        except ImportError:
            raise SystemExit(
                "No hotkey backend available. Either add your user to the 'input' group "
                "(sudo usermod -aG input $USER, then log out/in) or `pip install pynput` on X11."
            )

        mapping = _pynput_map(keyboard)

        def name_of(key) -> str | None:
            return mapping.get(key) or (
                f"KEY_{key.char.upper()}" if getattr(key, "char", None) else None
            )

        def on_press(key):
            n = name_of(key)
            if n:
                self._press(n)

        def on_release(key):
            n = name_of(key)
            if n:
                self._release(n)

        with keyboard.Listener(on_press=on_press, on_release=on_release) as listener:
            while not self._stop.is_set() and listener.running:
                time.sleep(0.2)


class _EvdevUnavailable(RuntimeError):
    pass


def _pynput_map(keyboard) -> dict:
    K = keyboard.Key
    pairs = {
        "ctrl_r": "KEY_RIGHTCTRL", "ctrl_l": "KEY_LEFTCTRL", "ctrl": "KEY_LEFTCTRL",
        "alt_r": "KEY_RIGHTALT", "alt_gr": "KEY_RIGHTALT", "alt_l": "KEY_LEFTALT", "alt": "KEY_LEFTALT",
        "shift_r": "KEY_RIGHTSHIFT", "shift_l": "KEY_LEFTSHIFT", "shift": "KEY_LEFTSHIFT",
        "cmd": "KEY_LEFTMETA", "cmd_l": "KEY_LEFTMETA", "cmd_r": "KEY_RIGHTMETA",
        "space": "KEY_SPACE", "caps_lock": "KEY_CAPSLOCK", "menu": "KEY_COMPOSE",
        "pause": "KEY_PAUSE", "scroll_lock": "KEY_SCROLLLOCK", "insert": "KEY_INSERT",
        **{f"f{i}": f"KEY_F{i}" for i in range(1, 25)},
    }
    out = {}
    for attr, name in pairs.items():
        k = getattr(K, attr, None)
        if k is not None and k not in out:
            out[k] = name
    return out


def watch_keys() -> None:
    """Print key names as they are pressed, to help pick a hotkey."""
    try:
        import evdev
        from evdev import ecodes
    except ImportError:
        raise SystemExit("python-evdev is not installed")
    devs = []
    for p in evdev.list_devices():
        try:
            d = evdev.InputDevice(p)
        except (PermissionError, OSError):
            continue
        if ecodes.EV_KEY in d.capabilities():
            devs.append(d)
    if not devs:
        raise SystemExit("No readable input devices. Add yourself to the 'input' group and re-login.")
    print("Press keys (Ctrl+C to quit):")
    sel = selectors.DefaultSelector()
    for d in devs:
        sel.register(d, selectors.EVENT_READ)
    try:
        while True:
            for key, _ in sel.select():
                for ev in key.fileobj.read():
                    if ev.type == ecodes.EV_KEY and ev.value == 1:
                        name = ecodes.KEY.get(ev.code) or ecodes.BTN.get(ev.code)
                        print(f"  {name}    ({key.fileobj.name})")
    except KeyboardInterrupt:
        pass
