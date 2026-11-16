#!/usr/bin/env python3
"""Serve linux-flow over the local network so a Linux machine can install it in one line.

Run on this machine (Windows, macOS or Linux, Python 3.8+, no dependencies):

    python serve.py              # port 8765
    python serve.py --port 9000

Then on the Linux machine:

    curl -fsSL http://<this-ip>:8765 | bash

Routes:
    /                    bootstrap script (downloads the bundle, runs install.sh)
    /linux-flow.tar.gz   the project bundle, including config.toml if present

Only the project's source files are served; nothing from your Wispr session or
home directory. Stop with Ctrl+C once the Linux side has finished installing.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import socket
import tarfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent
INCLUDE = ["linux_flow", "tests", "docs", "pyproject.toml", "install.sh", "README.md", "config.toml"]
SKIP_DIRS = {"__pycache__", ".pytest_cache", ".mypy_cache"}
TEXT_EXT = {".py", ".sh", ".toml", ".md", ".txt", ".cfg", ".desktop"}

BOOTSTRAP = r"""#!/usr/bin/env bash
# linux-flow network installer (served by serve.py)
set -euo pipefail
BASE="__BASE__"
SHA="__SHA__"
DEST="${XDG_DATA_HOME:-$HOME/.local/share}/linux-flow/src"
cd "$HOME"   # DEST gets replaced; don't run from inside it
echo "==> Downloading linux-flow from $BASE"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
if command -v curl >/dev/null; then
  curl -fsSL "$BASE/linux-flow.tar.gz" -o "$TMP/lf.tgz"
else
  wget -qO "$TMP/lf.tgz" "$BASE/linux-flow.tar.gz"
fi
GOT="$(sha256sum "$TMP/lf.tgz" | cut -d' ' -f1)"
[ "$GOT" = "$SHA" ] || { echo "Checksum mismatch (got $GOT); aborting." >&2; exit 1; }
rm -rf "$DEST"
mkdir -p "$DEST"
tar -xzf "$TMP/lf.tgz" -C "$DEST"
echo "==> Unpacked to $DEST"
# stdin is the curl pipe; give the installer the terminal so sudo can prompt.
if { : </dev/tty; } 2>/dev/null; then
  bash "$DEST/install.sh" </dev/tty
else
  bash "$DEST/install.sh"
fi
"""


def build_bundle() -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name in INCLUDE:
            p = ROOT / name
            if not p.exists():
                continue
            files = [p] if p.is_file() else sorted(
                f for f in p.rglob("*")
                if f.is_file() and not SKIP_DIRS.intersection(f.relative_to(ROOT).parts)
                and not any(part.endswith(".egg-info") for part in f.parts)
            )
            for f in files:
                data = f.read_bytes()
                if f.suffix in TEXT_EXT:  # Windows checkouts may have CRLF; bash/python on Linux want LF
                    data = data.replace(b"\r\n", b"\n")
                info = tarfile.TarInfo(f.relative_to(ROOT).as_posix())
                info.size = len(data)
                info.mtime = int(time.time())
                info.mode = 0o755 if f.suffix == ".sh" else 0o644
                tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def lan_ips() -> list[str]:
    """Most likely LAN address first (the one the default route uses), then the rest."""
    primary = []
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.connect(("192.0.2.1", 9))  # UDP connect sends nothing; it just picks the outbound interface
        primary.append(s.getsockname()[0])
        s.close()
    except OSError:
        pass
    others = set()
    try:
        for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
            others.add(info[4][0])
    except OSError:
        pass
    usable = lambda ip: not ip.startswith(("127.", "169.254.", "0."))
    return [ip for ip in primary if usable(ip)] + sorted(ip for ip in others - set(primary) if usable(ip))


def make_handler(bundle: bytes, sha: str):
    class Handler(BaseHTTPRequestHandler):
        server_version = "linux-flow-serve"

        def _send(self, body: bytes, ctype: str) -> None:
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)

        def do_GET(self) -> None:
            path = self.path.split("?", 1)[0]
            if path in ("/", "/install", "/install.sh"):
                host = self.headers.get("Host") or f"{self.server.server_address[0]}:{self.server.server_address[1]}"
                script = BOOTSTRAP.replace("__BASE__", f"http://{host}").replace("__SHA__", sha)
                self._send(script.encode(), "text/x-shellscript; charset=utf-8")
            elif path == "/linux-flow.tar.gz":
                self._send(bundle, "application/gzip")
            else:
                self.send_error(404)

        do_HEAD = do_GET

        def log_message(self, fmt, *args) -> None:
            print(f"  {self.client_address[0]}  {fmt % args}")

    return Handler


def main() -> None:
    import sys
    sys.stdout.reconfigure(line_buffering=True)  # show output even when not on a terminal
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--bind", default="0.0.0.0", help="address to listen on (default: all interfaces)")
    args = ap.parse_args()

    bundle = build_bundle()
    sha = hashlib.sha256(bundle).hexdigest()
    cfg = ROOT / "config.toml"
    print(f"linux-flow bundle: {len(bundle) // 1024} KB, sha256 {sha[:16]}...")
    print(f"config.toml: {'included (installed as your Linux config)' if cfg.exists() else 'not present, defaults will be used'}")

    httpd = ThreadingHTTPServer((args.bind, args.port), make_handler(bundle, sha))
    ips = lan_ips() or ["<this-machine-ip>"]
    print(f"\nServing on port {args.port}. On your Linux machine run:\n")
    print(f"    curl -fsSL http://{ips[0]}:{args.port} | bash")
    print("\nWith extras (offline Whisper, Claude cleanup):")
    print(f"    curl -fsSL http://{ips[0]}:{args.port} | LINUX_FLOW_EXTRAS=local,cleanup bash")
    if len(ips) > 1:
        print(f"\nIf {ips[0]} isn't reachable, try: {', '.join(ips[1:])}")
    print("\nIf the Linux machine can't connect, allow Python through the firewall when prompted.")
    print("Press Ctrl+C to stop.\n")
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nStopped.")


if __name__ == "__main__":
    main()
