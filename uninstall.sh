#!/usr/bin/env bash
# Remove linux-flow. Keeps your config and Wispr login unless you pass --purge.
set -euo pipefail
APP=linux-flow
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/$APP"

command -v "$APP" >/dev/null 2>&1 && "$APP" stop || true
rm -f "$HOME/.local/bin/$APP" "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$APP.desktop"
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/$APP" "${XDG_CACHE_HOME:-$HOME/.cache}/$APP"

if [ -f /etc/udev/rules.d/70-linux-flow.rules ] || [ -f /etc/modules-load.d/linux-flow-uinput.conf ]; then
  echo "Removing the keyboard permission rule (sudo)"
  sudo rm -f /etc/udev/rules.d/70-linux-flow.rules /etc/udev/rules.d/80-linux-flow-uinput.rules \
    /etc/modules-load.d/linux-flow-uinput.conf
  sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=input --action=change || true
fi

if [ "${1:-}" = --purge ]; then
  rm -rf "$CONFIG_DIR" "${XDG_STATE_HOME:-$HOME/.local/state}/$APP"
  echo "Removed linux-flow, its config and your stored Wispr login."
else
  echo "Removed linux-flow. Config and login kept in $CONFIG_DIR (use --purge to delete them)."
fi
