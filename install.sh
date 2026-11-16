#!/usr/bin/env bash
# linux-flow installer: system deps, venv, permissions, autostart.
set -euo pipefail

APP=linux-flow
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${XDG_DATA_HOME:-$HOME/.local/share}/$APP"
VENV="$PREFIX/venv"
BIN="$HOME/.local/bin"
EXTRAS="${LINUX_FLOW_EXTRAS:-}"   # e.g. LINUX_FLOW_EXTRAS=local,cleanup ./install.sh

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*"; }

[ "$(id -u)" -eq 0 ] && { echo "Run as your normal user (sudo is used where needed)."; exit 1; }

# ---------------------------------------------------------------- system deps
say "Installing system packages (sudo)"
if command -v apt-get >/dev/null; then
  sudo apt-get update -qq
  sudo apt-get install -y python3 python3-venv python3-dev libportaudio2 portaudio19-dev \
    xdotool xclip wl-clipboard libnotify-bin build-essential
  sudo apt-get install -y wtype 2>/dev/null || warn "wtype not available (fine on GNOME/KDE)"
elif command -v dnf >/dev/null; then
  sudo dnf install -y python3 python3-devel portaudio portaudio-devel xdotool xclip \
    wl-clipboard libnotify gcc
  sudo dnf install -y wtype 2>/dev/null || warn "wtype not available (fine on GNOME/KDE)"
elif command -v pacman >/dev/null; then
  sudo pacman -S --needed --noconfirm python portaudio xdotool xclip wl-clipboard libnotify wtype base-devel
elif command -v zypper >/dev/null; then
  sudo zypper install -y python3 python3-devel portaudio portaudio-devel xdotool xclip \
    wl-clipboard libnotify-tools gcc
else
  warn "Unknown distro. Install manually: python3 + venv, portaudio, xdotool, xclip, wl-clipboard, notify-send"
fi

# ------------------------------------------------------------- permissions
# Global hotkeys on Wayland need /dev/input (group 'input'); pasting needs /dev/uinput.
say "Granting keyboard access (input group + uinput rule)"
if ! id -nG "$USER" | grep -qw input; then
  sudo usermod -aG input "$USER"
  NEED_RELOGIN=1
fi
RULE=/etc/udev/rules.d/80-linux-flow-uinput.rules
if [ ! -f "$RULE" ]; then
  echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' | sudo tee "$RULE" >/dev/null
  echo uinput | sudo tee /etc/modules-load.d/linux-flow-uinput.conf >/dev/null
  sudo modprobe uinput || true
  sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=uinput || true
fi

# ------------------------------------------------- stop running copies (upgrade)
if pgrep -f "$VENV/bin/$APP" >/dev/null 2>&1; then
  say "Stopping running linux-flow"
  pkill -f "$VENV/bin/$APP" || true
  sleep 1
  WAS_RUNNING=1
fi

# --------------------------------------------------------------------- venv
say "Creating virtualenv at $VENV"
mkdir -p "$PREFIX" "$BIN"
python3 -m venv "$VENV"
"$VENV/bin/pip" install -q --upgrade pip
if [ -n "$EXTRAS" ]; then
  "$VENV/bin/pip" install -q "$SRC[$EXTRAS]"
else
  "$VENV/bin/pip" install -q "$SRC"
fi
ln -sf "$VENV/bin/$APP" "$BIN/$APP"

# ---------------------------------------------------------------- autostart
say "Adding autostart entry"
AUTO="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
mkdir -p "$AUTO"
cat > "$AUTO/$APP.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=linux-flow
Comment=Hold-to-talk dictation
Exec=sh -c 'mkdir -p "\${XDG_STATE_HOME:-\$HOME/.local/state}/$APP"; exec "$BIN/$APP" run >>"\${XDG_STATE_HOME:-\$HOME/.local/state}/$APP/log.txt" 2>&1'
X-GNOME-Autostart-enabled=true
NoDisplay=true
EOF

# ------------------------------------------------------------------- config
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/$APP"
CFG="$CFG_DIR/config.toml"
mkdir -p "$CFG_DIR"
if [ -f "$SRC/config.toml" ]; then
  if [ -f "$CFG" ] && ! cmp -s "$SRC/config.toml" "$CFG"; then
    cp "$CFG" "$CFG.bak"
    warn "Existing config backed up to $CFG.bak"
  fi
  say "Installing bundled config to $CFG"
  cp "$SRC/config.toml" "$CFG"
fi
"$VENV/bin/python" -c "from linux_flow import config; config.load()"   # writes default if missing
# Always identify as the official Windows client; Wispr may reject "linux".
if grep -q '^client_platform' "$CFG"; then
  sed -i 's/^client_platform *=.*/client_platform = "win32"/' "$CFG"
else
  warn "client_platform not set in $CFG; built-in default (win32) applies"
fi

# Restart in the background (same as autostart) if it was running before
if [ "${WAS_RUNNING:-0}" = 1 ] && [ "${NEED_RELOGIN:-0}" != 1 ] && { [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; }; then
  say "Restarting linux-flow in the background"
  LOG="${XDG_STATE_HOME:-$HOME/.local/state}/$APP/log.txt"
  mkdir -p "$(dirname "$LOG")"
  setsid -f "$BIN/$APP" run >>"$LOG" 2>&1 </dev/null || warn "Couldn't restart; log out and back in, or run: $APP run"
fi

cat <<EOF

Installed.
  1. Sign in:       $APP login
  2. Use it:        hold Right Ctrl, speak, release. It starts automatically at login;
                    don't also run '$APP run' while that copy is running ('$APP status' shows it).
  Config:           ${XDG_CONFIG_HOME:-$HOME/.config}/$APP/config.toml
  Logs (autostart): ${XDG_STATE_HOME:-$HOME/.local/state}/$APP/log.txt
EOF
case ":$PATH:" in *":$BIN:"*) ;; *) warn "$BIN is not on your PATH";; esac
[ "${NEED_RELOGIN:-0}" = 1 ] && warn "You were added to the 'input' group: log out and back in before running."
exit 0
