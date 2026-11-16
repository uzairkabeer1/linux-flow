# linux-flow

Hold-to-talk voice dictation for Linux, powered by **your Wispr Flow account**.
Hold a key, speak, release: the transcribed and AI-formatted text is typed into
whatever window has focus. Works on X11 and Wayland (GNOME, KDE, Sway, Hyprland, ...).

> **Unofficial.** Wispr Flow has no Linux app and no public API. linux-flow talks to
> the same private endpoints the official Windows/macOS app uses (worked out by reading
> the desktop app's bundle; see [docs/WISPR_PROTOCOL.md](docs/WISPR_PROTOCOL.md)).
> That can break whenever Wispr changes their backend, and using a non-official client
> may be against Wispr's Terms of Service. Use at your own risk. An offline engine
> (`backend = "local"`, faster-whisper) is included as a fallback.

## Install

### Over the network (from the machine that has this folder)

On the machine holding this folder (Windows is fine), start the install server:

```bash
python serve.py
```

It prints a command with this machine's IP. Run that on the Linux machine:

```bash
curl -fsSL http://<ip>:8765 | bash
curl -fsSL http://<ip>:8765 | LINUX_FLOW_EXTRAS=local,cleanup bash   # + offline Whisper + Claude cleanup
```

The bundle includes `config.toml` from this folder. Edit it before serving and it becomes
your Linux config (any existing one is saved as `config.toml.bak`). Stop the server with
Ctrl+C when done. If Linux can't connect, allow Python through the firewall.

### From a copy of the folder

```bash
bash install.sh                                   # Wispr backend only
LINUX_FLOW_EXTRAS=local,cleanup bash install.sh   # + offline Whisper + Claude cleanup
```

The installer:
- installs system packages (PortAudio, xdotool, xclip, wl-clipboard, notify-send, wtype where available)
- adds you to the `input` group and adds a udev rule for `/dev/uinput` (global hotkeys + paste on Wayland)
- creates a venv in `~/.local/share/linux-flow`, links `~/.local/bin/linux-flow`
- adds an autostart entry so it runs at login
- writes `~/.config/linux-flow/config.toml` and pins `client_platform = "win32"`

**Log out and back in** once afterwards, so the new `input` group membership applies.

## Use

```bash
linux-flow login      # opens wisprflow.ai in your browser; sign in there
linux-flow test-mic --transcribe   # 4 s mic check + transcription
linux-flow run        # start dictating (autostart does this at login)
```

Default hotkey: **hold Right Ctrl**, speak, release. A tap shorter than 250 ms is ignored.
You'll hear a rising tone when recording starts and a falling one when it stops.

Other commands: `status` (show detected session/output/login, and whether it's running), `stop`, `keys` (print key
names as you press them), `transcribe file.wav`, `logout`.

## Configure

`~/.config/linux-flow/config.toml` is created on first run. Common tweaks:

```toml
[hotkey]
keys = ["KEY_LEFTCTRL", "KEY_LEFTMETA"]   # Ctrl+Super, like Wispr on Windows
mode = "toggle"                           # press once to start, again to stop

[general]
fallback_backend = "local"                # use offline Whisper if Wispr fails

[wispr]
languages = ["en"]
formatting = true                         # Wispr's server-side AI formatting

[output]
method = "paste"                          # most robust for emoji/non-ASCII
```

Run `linux-flow keys` to find the name of any key.

### Optional: extra cleanup with Claude

Wispr already formats text server-side. For an extra polish pass, install with
`LINUX_FLOW_EXTRAS=cleanup`, export `ANTHROPIC_API_KEY`, and set `[cleanup] enabled = true`.
The default model is `claude-opus-5-5` at low effort. Set `model = "claude-haiku-5-5"`
for cheaper and faster runs, or use `provider = "openai"` with a local Ollama `base_url`.

## How text gets typed

| Session | Default method |
|---|---|
| X11 | `xdotool type` (falls back to clipboard + Ctrl+V) |
| Wayland on wlroots (Sway, Hyprland, river) | `wtype` |
| Wayland on GNOME / KDE | clipboard + Ctrl+V through a virtual keyboard (`/dev/uinput`); your old clipboard is restored afterwards |

In terminals on X11, Ctrl+Shift+V is used automatically. If nothing can type, the
text is left on your clipboard and a notification tells you.

## Troubleshooting

- **Hotkey does nothing**: run `id`, and check that `input` is listed. If not, log out and back in. `linux-flow keys` should print keys as you press them.
- **Text not inserted on Wayland**: check `ls -l /dev/uinput` shows group `input`, or install `ydotool`. Setting `method = "clipboard"` copies the text only.
- **`Wispr HTTP 4xx`**: run `linux-flow login` again. linux-flow reports itself as the Windows app (`client_platform = "win32"`, enforced by the installer), so the platform isn't the cause.
- **No sound / mic errors**: run `linux-flow test-mic`, and set `[audio] device` to a name or index from `python -m sounddevice`.
- **Text appears twice**: two copies were running (autostart + `linux-flow run`). Since 0.1.0's fix only one can run; `linux-flow status` shows it and `linux-flow stop` stops it. Re-running the installer also stops old copies.
- **Logs** for the autostart instance are in `~/.local/state/linux-flow/log.txt`.

## Development

```bash
pip install -e '.[local]' pytest && pytest -q    # offline tests, no network/mic needed
```

Layout: `linux_flow/app.py` (main loop), `hotkey.py` (evdev/pynput), `audio.py`,
`inject.py` (X11/Wayland typing), `backends/wispr.py`, `backends/local.py`, `cleanup.py`.
