"""Wispr Flow backend: talks to the same servers the official desktop app uses.

Protocol notes (reverse-engineered from the Wispr Flow 1.6.x desktop bundle, unofficial):
  * Login: browser device-code flow. We invent a UUID device_code, open
    https://wisprflow.ai/login?device_code=..., then poll /device/poll for tokens.
  * Tokens: WorkOS access JWT (~3 days) + rotating refresh token, refreshed at
    api.workos.com/user_management/authenticate (public client, no secret).
  * Dictation: POST https://api.wisprflow.ai/llm/dictation with JSON {"request": {...}}
    carrying a base64 16 kHz mono WAV (<= 28 s per request, chained via prev_* fields).
This is not an official API and may break whenever Wispr changes their backend.
"""

from __future__ import annotations

import base64
import html
import json
import os
import re
import subprocess
import sys
import threading
import time
import uuid
import webbrowser
from pathlib import Path

import numpy as np

from linux_flow import audio
from linux_flow.backends.base import Backend, TranscriptionError
from linux_flow.config import Config, config_dir

API = "https://api.wisprflow.ai"
WEB = "https://wisprflow.ai"
WORKOS = "https://api.workos.com"
SUPABASE = "https://dodjkfqhwrzqjwkfnthl.supabase.co"
CHUNK_SECONDS = 28
REQUEST_TIMEOUT = 20.0


def session_path() -> Path:
    return config_dir() / "wispr_session.json"


# --------------------------------------------------------------------- session

def _jwt_claims(token: str) -> dict:
    try:
        payload = token.split(".")[1]
        payload += "=" * (-len(payload) % 4)
        return json.loads(base64.urlsafe_b64decode(payload))
    except Exception:
        return {}


def _load_session() -> dict | None:
    p = session_path()
    if not p.exists():
        return None
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def _save_session(s: dict) -> None:
    p = session_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(s, f, indent=2)
    os.replace(tmp, p)


def _session_from_tokens(access: str, refresh: str, provider: str | None, previous: dict | None = None) -> dict:
    c = _jwt_claims(access)
    iss = c.get("iss", "")
    prov = provider or ("workos" if ("workos" in iss or "authkit" in iss) else "supabase")
    prev = previous or {}
    return {
        "provider": prov,
        "access_token": access,
        "refresh_token": refresh,
        "exp": int(c.get("exp", 0)),
        "client_id": c.get("client_id") or prev.get("client_id"),
        "user_id": c.get("urn:wispr:user_external_id") or c.get("sub") or prev.get("user_id"),
        "email": c.get("email") or prev.get("email", ""),
        # Stable per install, like prefs.sessionId in the desktop app.
        "session_id": prev.get("session_id") or str(uuid.uuid4()),
    }


def describe_session() -> str:
    s = _load_session()
    if not s:
        return "not signed in (run `linux-flow login`)"
    left = s.get("exp", 0) - time.time()
    who = s.get("email") or s.get("user_id", "?")
    state = f"token valid {left / 3600:.1f}h" if left > 0 else "token expired (auto-refreshes)"
    return f"signed in as {who} via {s.get('provider')}, {state}"


def logout() -> None:
    try:
        session_path().unlink()
    except FileNotFoundError:
        pass


def _open_url(url: str) -> None:
    for cmd in (["xdg-open", url], ["gio", "open", url]):
        try:
            subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return
        except OSError:
            continue
    webbrowser.open(url)


def interactive_login(timeout_s: int = 600) -> dict:
    """Browser device-code login. Credentials are entered on wisprflow.ai, never here."""
    import httpx

    device_code = str(uuid.uuid4())
    url = (
        f"{WEB}/login?device_code={device_code}&auto_signin=true"
        "&skip_default_trial=true&workos_capable=true"
    )
    print("Opening your browser to sign in to Wispr Flow.")
    print(f"If it doesn't open, visit:\n\n  {url}\n")
    _open_url(url)
    print("Waiting for sign-in to complete", end="", flush=True)
    deadline = time.time() + timeout_s
    with httpx.Client(timeout=10) as client:
        while time.time() < deadline:
            try:
                r = client.get(f"{API}/device/poll", params={"device_code": device_code},
                               headers={"Content-Type": "application/json"})
                data = r.json() if r.status_code == 200 and r.content else {}
            except (httpx.HTTPError, ValueError):
                data = {}
            if data.get("access_token") and data.get("refresh_token"):
                s = _session_from_tokens(data["access_token"], data["refresh_token"],
                                         data.get("provider"), _load_session())
                _save_session(s)
                print(f"\nSigned in as {s.get('email') or s['user_id']}.")
                return s
            if data.get("status") == "timeout":
                break
            print(".", end="", flush=True)
            time.sleep(3)
    raise TranscriptionError("login timed out; run `linux-flow login` again")


# -------------------------------------------------------------------- refresh

_refresh_lock = threading.Lock()


def _refresh(s: dict) -> dict:
    import httpx

    if s.get("provider") == "workos":
        if not s.get("client_id"):
            raise TranscriptionError("session missing WorkOS client_id; run `linux-flow login`")
        r = httpx.post(
            f"{WORKOS}/user_management/authenticate",
            timeout=15,
            headers={"Content-Type": "application/json", "Accept": "application/json"},
            json={"grant_type": "refresh_token", "client_id": s["client_id"],
                  "refresh_token": s["refresh_token"]},
        )
    else:
        anon = s.get("supabase_anon_key") or os.environ.get("WISPR_SUPABASE_ANON_KEY")
        if not anon:
            raise TranscriptionError("legacy Supabase session can't refresh; run `linux-flow login`")
        r = httpx.post(
            f"{SUPABASE}/auth/v1/token", params={"grant_type": "refresh_token"}, timeout=15,
            headers={"apikey": anon, "Authorization": f"Bearer {anon}"},
            json={"refresh_token": s["refresh_token"]},
        )
    if r.status_code >= 400:
        if 400 <= r.status_code < 500:
            raise TranscriptionError("Wispr session expired; run `linux-flow login`")
        raise TranscriptionError(f"token refresh failed: HTTP {r.status_code}")
    d = r.json()
    new = _session_from_tokens(d["access_token"], d.get("refresh_token") or s["refresh_token"],
                               s.get("provider"), s)
    _save_session(new)
    return new


def fresh_session(force: bool = False) -> dict:
    with _refresh_lock:
        s = _load_session()
        if not s:
            raise TranscriptionError("not signed in to Wispr Flow; run `linux-flow login`")
        if force or s.get("exp", 0) - 300 <= time.time():
            s = _refresh(s)
        return s


# ----------------------------------------------------------------- formatting

def html_to_text(s: str) -> str:
    """pasted_text may contain light HTML (lists, breaks). Flatten to plain text."""
    if "<" not in s:
        return html.unescape(s)
    counters: list[int | None] = []  # one entry per open list: int for <ol>, None for <ul>

    def tag(m: re.Match) -> str:
        closing, name = bool(m.group(1)), m.group(2).lower()
        if name in ("ol", "ul"):
            if closing:
                if counters:
                    counters.pop()
            else:
                counters.append(0 if name == "ol" else None)
            return "\n"
        if name == "li" and not closing:
            if counters and counters[-1] is not None:
                counters[-1] += 1
                return f"\n{counters[-1]}. "
            return "\n- "
        if name == "br" or (closing and name in ("p", "div")):
            return "\n"
        return ""

    out = re.sub(r"<\s*(/)?\s*([a-zA-Z0-9]+)[^>]*>", tag, s)
    out = html.unescape(out)
    out = re.sub(r"\n{3,}", "\n\n", out)
    return out.strip("\n")


# -------------------------------------------------------------------- backend

class WisprBackend(Backend):
    name = "wispr"

    def __init__(self, cfg: Config):
        self.cfg = cfg
        w = cfg.wispr
        self.languages = list(w.languages) or ([cfg.general.language] if cfg.general.language else [])
        self.client_platform = w.client_platform
        self.client_version = w.client_version
        self.formatting = w.formatting
        self.app_type = w.app_type
        self._http = None

    def _client(self):
        import httpx

        if self._http is None:
            self._http = httpx.Client(timeout=REQUEST_TIMEOUT, follow_redirects=False)
        return self._http

    def warmup(self) -> None:
        fresh_session()
        self._client()

    def _request_body(self, s: dict, wav_b64: str, final: bool, prev: dict, entity: str) -> dict:
        pipeline = ["transcribe", "align"] + (["format"] if (final and self.formatting) else [])
        return {"request": {
            "access_token": s["access_token"],
            "audio": wav_b64,
            "audio_packets": None,
            "pipeline": pipeline,
            "context": {
                "app": {"name": "", "bundle_id": "", "type": self.app_type, "url": ""},
                "ax_context": [], "ocr_context": [], "variable_names": [], "file_names": [],
                "dictionary_context": [], "user_first_name": "", "user_last_name": "",
                "textbox_contents": None, "content_text": "",
            },
            "personalization_style_settings": {
                "personalizationAppType": "unset", "personalizationStyle": "default",
            },
            "language": self.languages,
            "app": self.app_type,
            "user": {"uuid": s["user_id"]},
            "metadata": {
                "session_id": s["session_id"], "environment": "production",
                "client_platform": self.client_platform, "client_version": self.client_version,
                "transcript_entity_uuid": entity,
            },
            "prev_asr_text": " ".join(prev["asr"]),
            "prev_llm_text": prev["llm"],
            "prev_detected_language": prev["lang"],
            "last_nonempty_status": prev["status"],
            "debug_mode": False,
            "test_mode": False,
        }}

    def _post(self, s: dict, body: dict) -> tuple[dict, dict]:
        """POST /llm/dictation, refreshing the token once on 401/403."""
        import httpx

        for attempt in range(2):
            body["request"]["access_token"] = s["access_token"]
            try:
                r = self._client().post(
                    f"{API}/llm/dictation", json=body,
                    headers={"Authorization": s["access_token"], "Accept-Encoding": "identity"},
                )
            except httpx.HTTPError as e:
                raise TranscriptionError(f"network error: {e}") from e
            if r.status_code in (401, 403) and attempt == 0:
                s = fresh_session(force=True)
                continue
            if r.status_code >= 400:
                raise TranscriptionError(f"Wispr HTTP {r.status_code}: {_detail(r)}")
            try:
                return r.json(), s
            except ValueError as e:
                raise TranscriptionError("Wispr returned a non-JSON response") from e
        raise TranscriptionError("Wispr rejected the session; run `linux-flow login`")

    def _asr_fallback(self, s: dict, wav_b64: str) -> dict:
        r = self._client().post(
            f"{API}/llm/asr",
            headers={"Authorization": s["access_token"], "Content-Type": "application/json"},
            json={"audio": wav_b64, "language": self.languages[0] if len(self.languages) == 1 else "",
                  "prompt": ""},
        )
        if r.status_code >= 400:
            raise TranscriptionError(f"Wispr ASR fallback HTTP {r.status_code}: {_detail(r)}")
        d = r.json()
        return {"status": "raw_transcript", "asr_text": d.get("content") or "",
                "detected_language": d.get("detected_language")}

    def transcribe(self, samples: np.ndarray, sample_rate: int) -> str:
        if sample_rate != 16000:
            n = int(len(samples) * 16000 / sample_rate)
            samples = np.interp(np.linspace(0, len(samples) - 1, n), np.arange(len(samples)), samples).astype(np.int16)
        s = fresh_session()
        chunk = CHUNK_SECONDS * 16000
        offsets = list(range(0, max(len(samples), 1), chunk))
        prev = {"asr": [], "llm": "", "lang": None, "status": None}
        entity = str(uuid.uuid4())
        last: dict = {}
        for i, off in enumerate(offsets):
            wav_b64 = base64.b64encode(audio.to_wav(samples[off:off + chunk], 16000)).decode()
            body = self._request_body(s, wav_b64, i == len(offsets) - 1, prev, entity)
            last, s = self._post(s, body)
            status = last.get("status")
            if status == "error":
                last = self._asr_fallback(s, wav_b64)
                status = "raw_transcript"
            elif status not in ("formatted", "raw_transcript", "empty", "fallback"):
                print(f"[wispr] unexpected status {status!r}", file=sys.stderr)
            if last.get("asr_text"):
                prev["asr"].append(last["asr_text"])
            if last.get("llm_text"):
                prev["llm"] = last["llm_text"]
            lang = last.get("detected_language")
            if lang and lang != "xx":
                prev["lang"] = lang
            if status in ("formatted", "raw_transcript"):
                prev["status"] = status
        # Same rule as the desktop app: the final (formatted) response covers the whole
        # session thanks to prev_asr_text; fall back to the stitched raw transcript.
        text = last.get("pasted_text") or last.get("llm_text") or " ".join(prev["asr"])
        return html_to_text(text).strip()


def _detail(r) -> str:
    try:
        d = r.json()
    except ValueError:
        return r.text[:200]
    det = d.get("detail", d) if isinstance(d, dict) else d
    if isinstance(det, dict):
        det = det.get("message") or det.get("detail") or det.get("code") or det
    return str(det)[:200]
