# linux-flow

**Wispr Flow-style voice dictation for Linux.** Hold a key, speak, release: your words
appear, punctuated and formatted, wherever your cursor is. Works on X11 and Wayland
(GNOME, KDE, Sway, Hyprland, ...).

linux-flow uses **your own Wispr Flow account** for transcription, so you get the same
quality and formatting as the official Windows/macOS apps, which have no Linux version.

- Single static binary written in Rust: starts instantly, no Python/runtime needed
- Hold-to-talk or toggle, any hotkey (default: hold **Right Ctrl**)
- Types into any app: browser, editor, terminal, chat
- Long dictations (minutes) are streamed to Wispr in chunks, like the official app
- Optional extra polish pass with Claude (off by default)

> **Unofficial and not affiliated with Wispr.** Wispr Flow has no public API. linux-flow
> talks to the same private endpoints the official desktop app uses (documented in
> [docs/WISPR_PROTOCOL.md](docs/WISPR_PROTOCOL.md)). It can break whenever Wispr changes
> their backend, and using a non-official client may be against Wispr's Terms of Service.
> Use at your own risk.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/uzairkabeer1/linux-flow/main/install.sh | bash
```

Then sign in once:

```bash
linux-flow login
```

Your browser opens wisprflow.ai; sign in there (linux-flow never sees your password).
Then **hold Right Ctrl, speak, release.** linux-flow starts automatically when you log in.

The installer:
- puts the `linux-flow` binary in `~/.local/bin` (x86_64 download; other CPUs build from source with Rust)
- installs small desktop helpers: `wl-clipboard`/`xclip`, `xdotool`, `wtype`, `libnotify`, `parec`/`arecord`
- adds a udev rule so you (the person logged in at the screen) can read the keyboard
  for the global hotkey. It applies immediately: **no `input` group, no logout**
- adds an autostart entry, and writes `~/.config/linux-flow/config.toml`

Re-run the same command to upgrade. To remove: `bash uninstall.sh` (add `--purge` to also
delete your config and login).

<details>
<summary>Build from source instead</summary>

```bash
git clone https://github.com/uzairkabeer1/linux-flow && cd linux-flow
bash install.sh          # builds with cargo when run from a clone
```

Or just `cargo build --release` and run `target/release/linux-flow`.
</details>

## Commands

| Command | What it does |
|---|---|
| `linux-flow` / `linux-flow run` | Start dictating (autostart does this for you) |
| `linux-flow login` / `logout` | Sign in to / out of Wispr Flow |
| `linux-flow status` | Show what was detected and anything missing |
| `linux-flow setup` | Fix keyboard permissions (if `status` says so) |
| `linux-flow stop` | Stop the background instance |
| `linux-flow keys` | Print key names as you press them, to pick a hotkey |
| `linux-flow test-mic --transcribe` | Record 4 s, show levels, transcribe |
| `linux-flow transcribe file.wav` | Transcribe a WAV file |

Only one instance runs at a time; a second `linux-flow run` tells you one is already running.

## Configure

Edit `~/.config/linux-flow/config.toml`, then restart it with `linux-flow stop; linux-flow run`.
Some common changes:

```toml
[hotkey]
keys = ["KEY_LEFTCTRL", "KEY_LEFTMETA"]   # Ctrl+Super, like Wispr on Windows
mode = "toggle"                           # press once to start, again to stop

[wispr]
languages = ["en"]                        # skip language auto-detection
formatting = true                         # Wispr's AI formatting (lists, punctuation)

[output]
method = "paste"                          # most robust for emoji and non-Latin text
```

Every option is documented in the file itself.

### Optional: extra cleanup with Claude

Wispr already formats your text. For an extra polish pass, export `ANTHROPIC_API_KEY` and set:

```toml
[cleanup]
enabled = true
model = "claude-opus-5-5"     # or "claude-haiku-5-5" for cheaper/faster
```

`provider = "openai"` with `base_url = "http://localhost:11434/v1"` works with a local Ollama model.

## How text gets typed

| Session | Method |
|---|---|
| X11 | `xdotool type` (Ctrl+Shift+V paste in terminals) |
| Wayland: Sway, Hyprland, river, ... | `wtype` |
| Wayland: GNOME, KDE | clipboard + Ctrl+V through a virtual keyboard; your clipboard is restored afterwards |

Text is always inserted exactly once: if typing fails partway, linux-flow doesn't paste a
second copy. It leaves the full text on your clipboard and shows a notification.

## Troubleshooting

Start with `linux-flow status`; it lists anything missing and how to fix it.

- **Hotkey does nothing**: `linux-flow setup`. `linux-flow keys` should print keys as you press them.
- **Text appears twice**: two copies were running (only possible with the old Python
  version). Re-run the installer; it stops old copies.
- **Nothing is typed on GNOME/KDE Wayland**: `linux-flow setup` (needs `/dev/uinput`), or set
  `[output] method = "clipboard"` and paste yourself.
- **Wispr HTTP 401 / session expired**: `linux-flow login`.
- **Wrong microphone**: set `[audio] device` to a source name from `pactl list short sources`.
- **Logs**: `~/.local/state/linux-flow/log.txt`.

## Privacy and security

- Audio is sent to Wispr's servers only while you hold the hotkey, as with the official app.
- Your Wispr login is stored in `~/.config/linux-flow/wispr_session.json` (mode 600).
- The hotkey needs read access to the keyboard. The udev rule grants it only to the user
  logged in at the physical seat, like the `input` group but without a logout, and it
  lets programs running as you read keystrokes. `uninstall.sh` removes the rule.

## Development

```bash
cargo test              # offline tests: protocol, chunking, token refresh, insertion logic
cargo clippy --all-targets
cargo build --release
```

Layout: `src/app.rs` (main loop), `hotkey.rs` (evdev), `audio.rs` (capture + WAV),
`wispr.rs` (login, refresh, dictation API), `inject.rs` (typing/paste), `setup.rs`
(permissions), `cleanup.rs` (optional LLM pass).

A Python prototype lives in the git history (tag `python-v0.1.0`).

## License

MIT
