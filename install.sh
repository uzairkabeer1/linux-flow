#!/usr/bin/env bash
# linux-flow installer
#
#   curl -fsSL https://raw.githubusercontent.com/uzairkabeer1/linux-flow/main/install.sh | bash
#
# Installs the linux-flow binary to ~/.local/bin, the small desktop tools it uses
# (clipboard, typing, mic capture), a one-time keyboard permission rule, and an
# autostart entry. Safe to re-run: it upgrades in place and keeps your config/login.
#
# Options (environment variables):
#   LINUX_FLOW_VERSION=v0.2.0   install a specific release (default: latest)
#   LINUX_FLOW_BIN=/path/bin    install this binary instead of downloading
#   LINUX_FLOW_NO_PACKAGES=1    don't install system packages
set -euo pipefail

REPO="uzairkabeer1/linux-flow"
APP="linux-flow"
BIN_DIR="$HOME/.local/bin"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/$APP"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/$APP"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/$APP"

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

# Everything runs inside main(), so under `curl | bash` the whole script is read before
# anything executes (commands below can't swallow the rest of it from stdin).
main() {
[ "$(id -u)" -ne 0 ] || die "Run as your normal user, not root (sudo is used only where needed)."
[ "$(uname -s)" = Linux ] || die "linux-flow is for Linux."

# Running from a clone? Then we can build from source.
SRC_DIR=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
  SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi
cd "$HOME"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------- stop old copies
WAS_RUNNING=0
if command -v "$APP" >/dev/null 2>&1 && "$APP" stop 2>/dev/null | grep -q Stopped; then WAS_RUNNING=1; fi
if pgrep -f "$DATA_DIR/venv/bin/python" >/dev/null 2>&1; then    # the old Python version
  pkill -f "$DATA_DIR/venv/bin/python" || true
  WAS_RUNNING=1
fi

# -------------------------------------------------------------------- binary
case "$(uname -m)" in
  x86_64|amd64)  TARGET=x86_64-unknown-linux-musl ;;
  aarch64|arm64) TARGET=aarch64-unknown-linux-musl ;;
  *)             TARGET="" ;;
esac

build_from_source() {
  command -v cargo >/dev/null 2>&1 || die "No prebuilt binary for $(uname -m). Install Rust (https://rustup.rs) and re-run."
  if [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/Cargo.toml" ]; then
    say "Building from $SRC_DIR (cargo build --release)"
    (cd "$SRC_DIR" && cargo build --release --locked)
    cp "$SRC_DIR/target/release/$APP" "$TMP/$APP"
  else
    say "Building from source (cargo install)"
    cargo install --locked --git "https://github.com/$REPO" --root "$TMP/root" "$APP"
    cp "$TMP/root/bin/$APP" "$TMP/$APP"
  fi
}

if [ -n "${LINUX_FLOW_BIN:-}" ]; then
  cp "$LINUX_FLOW_BIN" "$TMP/$APP"
elif [ -n "$SRC_DIR" ] && [ -f "$SRC_DIR/Cargo.toml" ] && command -v cargo >/dev/null 2>&1; then
  build_from_source
elif [ -n "$TARGET" ]; then
  VERSION="${LINUX_FLOW_VERSION:-latest}"
  if [ "$VERSION" = latest ]; then BASE="https://github.com/$REPO/releases/latest/download"
  else BASE="https://github.com/$REPO/releases/download/$VERSION"; fi
  say "Downloading $APP ($TARGET, $VERSION)"
  if curl -fsSL "$BASE/$APP-$TARGET" -o "$TMP/$APP" && curl -fsSL "$BASE/$APP-$TARGET.sha256" -o "$TMP/sum"; then
    (cd "$TMP" && echo "$(cut -d' ' -f1 sum)  $APP" | sha256sum -c --quiet -) || die "Checksum mismatch; aborting."
  else
    warn "No prebuilt binary for $TARGET in this release."
    build_from_source
  fi
else
  build_from_source
fi

mkdir -p "$BIN_DIR"
rm -f "$BIN_DIR/$APP"                       # may be the old Python symlink
install -m 755 "$TMP/$APP" "$BIN_DIR/$APP"
rm -rf "$DATA_DIR/venv" "$DATA_DIR/src"     # old Python version, if any
say "Installed $("$BIN_DIR/$APP" --version) to $BIN_DIR/$APP"

# ---------------------------------------------------------- system packages
if [ "${LINUX_FLOW_NO_PACKAGES:-0}" != 1 ]; then
  say "Installing desktop helpers (clipboard, typing, mic capture, notifications)"
  if command -v apt-get >/dev/null; then
    sudo apt-get install -y wl-clipboard xclip xdotool libnotify-bin pulseaudio-utils alsa-utils >/dev/null ||
      { sudo apt-get update -qq && sudo apt-get install -y wl-clipboard xclip xdotool libnotify-bin pulseaudio-utils alsa-utils >/dev/null; }
    sudo apt-get install -y wtype >/dev/null 2>&1 || true
  elif command -v dnf >/dev/null; then
    sudo dnf install -y wl-clipboard xclip xdotool libnotify pulseaudio-utils alsa-utils >/dev/null
    sudo dnf install -y wtype >/dev/null 2>&1 || true
  elif command -v pacman >/dev/null; then
    sudo pacman -S --needed --noconfirm wl-clipboard xclip xdotool libnotify libpulse alsa-utils wtype >/dev/null
  elif command -v zypper >/dev/null; then
    sudo zypper --non-interactive install wl-clipboard xclip xdotool libnotify-tools pulseaudio-utils alsa-utils >/dev/null
  else
    warn "Unknown package manager. Install: wl-clipboard (Wayland) or xclip + xdotool (X11), libnotify, pulseaudio-utils or alsa-utils"
  fi
fi

# ----------------------------------------------------------------- permissions
say "Keyboard permission (one time; no logout needed)"
"$BIN_DIR/$APP" setup || warn "Permission setup didn't finish; run '$APP setup' later."

# ---------------------------------------------------------------------- config
"$BIN_DIR/$APP" status >/dev/null 2>&1 || true      # writes the default config if missing
CFG="$CONFIG_DIR/config.toml"
if [ -f "$CFG" ] && grep -q '^client_platform' "$CFG"; then
  sed -i 's/^client_platform *=.*/client_platform = "win32"/' "$CFG"   # mirror the official Windows app
fi

# ------------------------------------------------------------------- autostart
AUTO="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
mkdir -p "$AUTO"
cat > "$AUTO/$APP.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=linux-flow
Comment=Hold-to-talk dictation
Exec=sh -c 'mkdir -p "\${XDG_STATE_HOME:-\$HOME/.local/state}/$APP"; exec "$BIN_DIR/$APP" run >>"\${XDG_STATE_HOME:-\$HOME/.local/state}/$APP/log.txt" 2>&1'
X-GNOME-Autostart-enabled=true
NoDisplay=true
EOF

# --------------------------------------------------------- start it right away
SIGNED_IN=0
[ -f "$CONFIG_DIR/wispr_session.json" ] && SIGNED_IN=1
STARTED=0
if [ "$SIGNED_IN" = 1 ] && { [ -n "${WAYLAND_DISPLAY:-}" ] || [ -n "${DISPLAY:-}" ]; }; then
  mkdir -p "$STATE_DIR"
  setsid -f "$BIN_DIR/$APP" run >>"$STATE_DIR/log.txt" 2>&1 </dev/null && STARTED=1
  sleep 1
  "$BIN_DIR/$APP" status 2>/dev/null | grep -q '^running: *yes' || { STARTED=0; warn "It didn't start; see $STATE_DIR/log.txt"; }
fi

echo
say "Done."
[ "$SIGNED_IN" = 1 ] || echo "  1. Sign in:   $APP login"
if [ "$STARTED" = 1 ]; then
  echo "  linux-flow is running. Hold Right Ctrl, speak, release."
else
  echo "  Start:        $APP run      (it also starts automatically when you log in)"
fi
echo "  Check setup:  $APP status"
echo "  Config:       $CONFIG_DIR/config.toml"
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) warn "$BIN_DIR is not on your PATH (open a new terminal, or add it)";; esac
}

main "$@"
