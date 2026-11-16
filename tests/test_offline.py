"""Offline tests (no mic, no display, no network). Run: python -m pytest tests -q"""

import base64
import json
import time

import httpx
import numpy as np
import pytest

from linux_flow import audio, config
from linux_flow.backends import wispr
from linux_flow.hotkey import HotkeyListener


def _jwt(claims: dict) -> str:
    enc = lambda d: base64.urlsafe_b64encode(json.dumps(d).encode()).decode().rstrip("=")
    return f"{enc({'alg': 'none'})}.{enc(claims)}.sig"


@pytest.fixture
def cfg(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path))
    return config.load()


def _login(exp_in=3600):
    tok = _jwt({"iss": "https://api.workos.com/user_management/client_X", "client_id": "client_X",
                "exp": int(time.time()) + exp_in, "urn:wispr:user_external_id": "11111111-1111-1111-1111-111111111111",
                "email": "me@example.com"})
    wispr._save_session(wispr._session_from_tokens(tok, "rt1", None))


def test_default_config(cfg):
    assert cfg.general.backend == "wispr"
    assert cfg.wispr.formatting is True
    assert cfg.hotkey.keys == ["KEY_RIGHTCTRL"]


def test_wav_roundtrip(tmp_path):
    s = (np.sin(np.linspace(0, 3000, 32000)) * 9000).astype(np.int16)
    p = tmp_path / "a.wav"
    p.write_bytes(audio.to_wav(s))
    assert np.array_equal(audio.read_wav(str(p)), s)
    assert not audio.is_silent(s) and audio.is_silent(np.zeros(10, np.int16))


def test_hotkey_combo():
    ev = []
    h = HotkeyListener(["KEY_LEFTCTRL", "KEY_SPACE"], lambda: ev.append("d"), lambda: ev.append("u"))
    h._press("KEY_LEFTCTRL"); h._press("KEY_X"); h._press("KEY_SPACE"); h._press("KEY_SPACE")
    h._release("KEY_LEFTCTRL"); h._release("KEY_SPACE")
    assert ev == ["d", "u"]


def test_html_to_text():
    assert wispr.html_to_text("Buy:<ol><li>milk</li><li>eggs</li></ol>") == "Buy:\n\n1. milk\n2. eggs"
    assert wispr.html_to_text("a &amp; b<br>c") == "a & b\nc"
    assert wispr.html_to_text("plain") == "plain"


def test_session_parsing(cfg):
    _login()
    s = wispr._load_session()
    assert s["provider"] == "workos" and s["client_id"] == "client_X"
    assert s["user_id"].startswith("1111") and s["session_id"]
    assert "me@example.com" in wispr.describe_session()


class FakeServer:
    def __init__(self, statuses):
        self.statuses = list(statuses)
        self.calls = []

    def __call__(self, request: httpx.Request):
        body = json.loads(request.content) if request.content else {}
        self.calls.append((request.url.path, request.headers.get("authorization"), body))
        status = self.statuses.pop(0)
        if request.url.path == "/llm/asr":
            return httpx.Response(200, json={"content": "raw words", "detected_language": "en"})
        if status == 401:
            return httpx.Response(401, json={"detail": "expired"})
        if status == "error":
            return httpx.Response(200, json={"status": "error"})
        n = len([c for c in self.calls if c[0] == "/llm/dictation"])
        return httpx.Response(200, json={
            "status": "formatted", "asr_text": f"chunk {n}", "llm_text": f"Chunk {n}.",
            "pasted_text": f"<p>Final {n}.</p>", "detected_language": "en"})


def _backend(cfg, server):
    b = wispr.WisprBackend(cfg)
    b._http = httpx.Client(transport=httpx.MockTransport(server))
    return b


def test_single_request_body(cfg):
    _login()
    srv = FakeServer([200])
    out = _backend(cfg, srv).transcribe(np.ones(16000 * 3, np.int16) * 500, 16000)
    assert out == "Final 1."
    path, auth, body = srv.calls[0]
    req = body["request"]
    assert path == "/llm/dictation" and not auth.startswith("Bearer")
    assert req["pipeline"] == ["transcribe", "align", "format"]
    assert req["user"]["uuid"].startswith("1111")
    wav = base64.b64decode(req["audio"])
    assert wav[:4] == b"RIFF" and len(wav) == 44 + 16000 * 3 * 2


def test_chunking_threads_prev_text(cfg):
    _login()
    srv = FakeServer([200, 200, 200])
    out = _backend(cfg, srv).transcribe(np.ones(16000 * 60, np.int16) * 500, 16000)  # 60 s -> 3 chunks
    reqs = [c[2]["request"] for c in srv.calls]
    assert len(reqs) == 3
    assert reqs[0]["pipeline"] == ["transcribe", "align"]          # no format mid-session
    assert reqs[2]["pipeline"][-1] == "format"
    assert reqs[2]["prev_asr_text"] == "chunk 1 chunk 2"
    assert len({r["metadata"]["transcript_entity_uuid"] for r in reqs}) == 1
    assert out == "Final 3."


def test_401_refreshes_once(cfg, monkeypatch):
    _login()
    refreshed = []

    def fake_refresh(s):
        refreshed.append(1)
        new = dict(s, access_token=s["access_token"] + "x")
        wispr._save_session(new)
        return new

    monkeypatch.setattr(wispr, "_refresh", fake_refresh)
    srv = FakeServer([401, 200])
    assert _backend(cfg, srv).transcribe(np.ones(16000, np.int16) * 500, 16000) == "Final 2."
    assert refreshed == [1] and srv.calls[1][1].endswith("x")


def test_error_status_falls_back_to_asr(cfg):
    _login()
    srv = FakeServer(["error", 200])
    assert _backend(cfg, srv).transcribe(np.ones(16000, np.int16) * 500, 16000) == "raw words"
    assert srv.calls[1][0] == "/llm/asr"


def test_expired_token_refreshes_before_request(cfg, monkeypatch):
    _login(exp_in=-10)
    called = []
    monkeypatch.setattr(wispr, "_refresh", lambda s: called.append(1) or s)
    wispr.fresh_session()
    assert called == [1]


# --- text is inserted exactly once ------------------------------------------

def _fake_typer(monkeypatch, returncode=0, stderr=b"", timeout=False):
    import subprocess

    from linux_flow import inject

    calls = {"typed": 0, "pasted": 0}

    def fake_run(args, **kw):
        calls["typed"] += 1
        if timeout:
            raise subprocess.TimeoutExpired(args, 1)
        return subprocess.CompletedProcess(args, returncode, b"", stderr)

    monkeypatch.setattr(inject, "_have", lambda c: True)
    monkeypatch.setattr(inject.subprocess, "run", fake_run)
    monkeypatch.setattr(inject, "clipboard_set", lambda t: True)
    monkeypatch.setattr(inject.Injector, "_paste", lambda self, t: calls.__setitem__("pasted", calls["pasted"] + 1) or True)
    return inject.Injector("xdotool"), calls


def test_typing_success_does_not_paste(monkeypatch):
    inj, calls = _fake_typer(monkeypatch)
    assert inj.insert("hello") == "xdotool" and calls == {"typed": 1, "pasted": 0}


def test_typing_failure_midway_does_not_paste_again(monkeypatch):
    inj, calls = _fake_typer(monkeypatch, returncode=1, stderr=b"XGetWindowProperty failed")
    assert inj.insert("hello") == "xdotool-partial" and calls["pasted"] == 0


def test_typing_timeout_does_not_paste_again(monkeypatch):
    inj, calls = _fake_typer(monkeypatch, timeout=True)
    assert inj.insert("hello") == "xdotool-partial" and calls["pasted"] == 0


def test_typer_that_typed_nothing_falls_back_to_paste(monkeypatch):
    inj, calls = _fake_typer(monkeypatch, returncode=1, stderr=b"Compositor does not support the virtual keyboard protocol")
    assert inj.insert("hello") == "paste" and calls["pasted"] == 1


# --- only one instance can run ----------------------------------------------

def test_single_instance(tmp_path, monkeypatch):
    pytest.importorskip("fcntl")
    from linux_flow import instance

    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path))
    monkeypatch.setattr(instance, "_held", None)
    assert instance.running_pid() is None
    instance.acquire()
    import os
    import subprocess
    import sys

    # a second process must see us and refuse to start
    code = "from linux_flow import instance; print(instance.running_pid()); instance.acquire()"
    r = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True,
                       env={**os.environ, "XDG_RUNTIME_DIR": str(tmp_path)})
    assert r.stdout.strip() == str(os.getpid())
    assert r.returncode != 0 and "already running" in r.stderr
    instance._held.close()
    assert instance.running_pid() is None


def test_hotkey_ignores_own_virtual_keyboard():
    from linux_flow.hotkey import IGNORED_DEVICES
    assert "linux-flow-vkbd" in IGNORED_DEVICES
