//! Wispr Flow client: talks to the same servers the official desktop app uses.
//!
//! Protocol (reverse-engineered from the Wispr Flow 1.6.x desktop bundle, unofficial;
//! details in docs/WISPR_PROTOCOL.md):
//!  * Login: browser device-code flow. We invent a UUID device_code, open
//!    https://wisprflow.ai/login?device_code=..., then poll /device/poll for tokens.
//!  * Tokens: WorkOS access JWT (~3 days) + rotating refresh token, refreshed at
//!    api.workos.com/user_management/authenticate (public client, no secret).
//!  * Dictation: POST https://api.wisprflow.ai/llm/dictation with JSON {"request": {...}}
//!    carrying a base64 16 kHz mono WAV (<= 28 s per request, chained via prev_* fields).

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::audio::{to_wav, RATE};
use crate::config;
use crate::util::{config_dir, Result};

pub const API: &str = "https://api.wisprflow.ai";
pub const WEB: &str = "https://wisprflow.ai";
pub const WORKOS: &str = "https://api.workos.com";
pub const SUPABASE: &str = "https://dodjkfqhwrzqjwkfnthl.supabase.co";
pub const CHUNK_SECONDS: usize = 28;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// ------------------------------------------------------------------ transport

/// HTTP abstraction so the protocol logic can be tested without a network.
pub trait Transport: Send + Sync {
    /// Returns (status, body). Err only for network-level failures.
    fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<(u16, String)>;
    fn get(&self, url: &str) -> Result<(u16, String)>;
}

pub struct Http(ureq::Agent);

impl Http {
    pub fn new(timeout: Duration) -> Self {
        Http(ureq::AgentBuilder::new().timeout(timeout).redirects(0).build())
    }
}

impl Transport for Http {
    fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<(u16, String)> {
        let mut req = self.0.post(url).set("Accept-Encoding", "identity");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        respond(req.send_json(body))
    }

    fn get(&self, url: &str) -> Result<(u16, String)> {
        respond(self.0.get(url).set("Content-Type", "application/json").call())
    }
}

fn respond(r: std::result::Result<ureq::Response, ureq::Error>) -> Result<(u16, String)> {
    match r {
        Ok(resp) => Ok((resp.status(), resp.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(code, resp)) => Ok((code, resp.into_string().unwrap_or_default())),
        Err(e) => Err(format!("network error: {e}")),
    }
}

// -------------------------------------------------------------------- session

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Session {
    pub provider: String,
    pub access_token: String,
    pub refresh_token: String,
    pub exp: i64,
    pub client_id: Option<String>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supabase_anon_key: Option<String>,
}

pub fn jwt_claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|p| URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

pub fn session_from_tokens(access: &str, refresh: &str, provider: Option<&str>, prev: Option<&Session>) -> Session {
    let c = jwt_claims(access);
    let s = |k: &str| c.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(String::from);
    let iss = s("iss").unwrap_or_default();
    let provider = provider.map(String::from).unwrap_or_else(|| {
        if iss.contains("workos") || iss.contains("authkit") { "workos".into() } else { "supabase".into() }
    });
    let prev = prev.cloned().unwrap_or_default();
    Session {
        provider,
        access_token: access.into(),
        refresh_token: refresh.into(),
        exp: c.get("exp").and_then(Value::as_i64).unwrap_or(0),
        client_id: s("client_id").or(prev.client_id),
        user_id: s("urn:wispr:user_external_id").or_else(|| s("sub")).or(prev.user_id),
        email: s("email").or(prev.email),
        // Stable per install, like prefs.sessionId in the desktop app.
        session_id: prev.session_id.or_else(|| Some(uuid::Uuid::new_v4().to_string())),
        supabase_anon_key: prev.supabase_anon_key,
    }
}

#[derive(Clone)]
pub struct SessionStore {
    pub path: PathBuf,
}

impl Default for SessionStore {
    fn default() -> Self {
        SessionStore { path: config_dir().join("wispr_session.json") }
    }
}

impl SessionStore {
    pub fn load(&self) -> Option<Session> {
        serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()
    }

    pub fn save(&self, s: &Session) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let dir = self.path.parent().ok_or("bad session path")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(serde_json::to_string_pretty(s).unwrap().as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().ok();
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    pub fn describe(&self) -> String {
        let Some(s) = self.load() else {
            return "not signed in (run `linux-flow login`)".into();
        };
        let left = s.exp - now();
        let who = s.email.clone().or(s.user_id.clone()).unwrap_or_else(|| "?".into());
        let state = if left > 0 {
            format!("token valid {:.1}h", left as f64 / 3600.0)
        } else {
            "token expired (auto-refreshes)".into()
        };
        format!("signed in as {who} via {}, {state}", s.provider)
    }
}

fn open_url(url: &str) {
    for cmd in [&["xdg-open"][..], &["gio", "open"][..]] {
        let mut args: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        args.push(url.into());
        if crate::util::have(&args[0]) {
            crate::util::spawn_detached(args);
            return;
        }
    }
}

/// Browser device-code login. Credentials are entered on wisprflow.ai, never here.
pub fn interactive_login(store: &SessionStore, http: &dyn Transport, timeout: Duration) -> Result<Session> {
    use std::io::Write;
    let device_code = uuid::Uuid::new_v4().to_string();
    let url = format!(
        "{WEB}/login?device_code={device_code}&auto_signin=true&skip_default_trial=true&workos_capable=true"
    );
    println!("Opening your browser to sign in to Wispr Flow.");
    println!("If it doesn't open, visit:\n\n  {url}\n");
    open_url(&url);
    print!("Waiting for sign-in to complete");
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let data: Value = match http.get(&format!("{API}/device/poll?device_code={device_code}")) {
            Ok((200, body)) => serde_json::from_str(&body).unwrap_or(Value::Null),
            _ => Value::Null,
        };
        let tok = |k: &str| data.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
        if let (Some(a), Some(r)) = (tok("access_token"), tok("refresh_token")) {
            let s = session_from_tokens(a, r, tok("provider"), store.load().as_ref());
            store.save(&s)?;
            println!("\nSigned in as {}.", s.email.clone().or(s.user_id.clone()).unwrap_or_default());
            return Ok(s);
        }
        if tok("status") == Some("timeout") {
            break;
        }
        print!(".");
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_secs(3));
    }
    Err("login timed out; run `linux-flow login` again".into())
}

// --------------------------------------------------------------------- client

pub struct Wispr {
    pub cfg: config::Wispr,
    pub languages: Vec<String>,
    pub store: SessionStore,
    pub http: Box<dyn Transport>,
}

#[derive(Default)]
struct Prev {
    asr: Vec<String>,
    llm: String,
    lang: Option<String>,
    status: Option<String>,
}

impl Wispr {
    pub fn new(cfg: &config::Config) -> Self {
        let languages = if !cfg.wispr.languages.is_empty() {
            cfg.wispr.languages.clone()
        } else if !cfg.general.language.is_empty() {
            vec![cfg.general.language.clone()]
        } else {
            vec![]
        };
        Wispr {
            cfg: cfg.wispr.clone(),
            languages,
            store: SessionStore::default(),
            http: Box::new(Http::new(Duration::from_secs(20))),
        }
    }

    pub fn fresh_session(&self, force: bool) -> Result<Session> {
        let s = self.store.load().ok_or("not signed in to Wispr Flow; run `linux-flow login`")?;
        if force || s.exp - 300 <= now() {
            return self.refresh(&s);
        }
        Ok(s)
    }

    fn refresh(&self, s: &Session) -> Result<Session> {
        let (code, body) = if s.provider == "workos" {
            let client_id = s.client_id.as_deref().ok_or("session missing WorkOS client_id; run `linux-flow login`")?;
            self.http.post_json(
                &format!("{WORKOS}/user_management/authenticate"),
                &[("Content-Type", "application/json"), ("Accept", "application/json")],
                &json!({"grant_type": "refresh_token", "client_id": client_id, "refresh_token": s.refresh_token}),
            )?
        } else {
            let anon = s
                .supabase_anon_key
                .clone()
                .or_else(|| std::env::var("WISPR_SUPABASE_ANON_KEY").ok())
                .ok_or("legacy Supabase session can't refresh; run `linux-flow login`")?;
            let bearer = format!("Bearer {anon}");
            self.http.post_json(
                &format!("{SUPABASE}/auth/v1/token?grant_type=refresh_token"),
                &[("apikey", &anon), ("Authorization", &bearer)],
                &json!({"refresh_token": s.refresh_token}),
            )?
        };
        if (400..500).contains(&code) {
            return Err("Wispr session expired; run `linux-flow login`".into());
        }
        if code >= 500 {
            return Err(format!("token refresh failed: HTTP {code}"));
        }
        let d: Value = serde_json::from_str(&body).map_err(|_| "token refresh returned non-JSON")?;
        let access = d.get("access_token").and_then(Value::as_str).ok_or("token refresh returned no access_token")?;
        let refresh = d.get("refresh_token").and_then(Value::as_str).unwrap_or(&s.refresh_token);
        let new = session_from_tokens(access, refresh, Some(&s.provider), Some(s));
        self.store.save(&new)?;
        Ok(new)
    }

    fn request_body(&self, s: &Session, wav_b64: &str, final_chunk: bool, prev: &Prev, entity: &str) -> Value {
        let mut pipeline = vec!["transcribe", "align"];
        if final_chunk && self.cfg.formatting {
            pipeline.push("format");
        }
        json!({"request": {
            "access_token": s.access_token,
            "audio": wav_b64,
            "audio_packets": null,
            "pipeline": pipeline,
            "context": {
                "app": {"name": "", "bundle_id": "", "type": self.cfg.app_type, "url": ""},
                "ax_context": [], "ocr_context": [], "variable_names": [], "file_names": [],
                "dictionary_context": [], "user_first_name": "", "user_last_name": "",
                "textbox_contents": null, "content_text": "",
            },
            "personalization_style_settings": {
                "personalizationAppType": "unset", "personalizationStyle": "default",
            },
            "language": self.languages,
            "app": self.cfg.app_type,
            "user": {"uuid": s.user_id},
            "metadata": {
                "session_id": s.session_id, "environment": "production",
                "client_platform": self.cfg.client_platform, "client_version": self.cfg.client_version,
                "transcript_entity_uuid": entity,
            },
            "prev_asr_text": prev.asr.join(" "),
            "prev_llm_text": prev.llm,
            "prev_detected_language": prev.lang,
            "last_nonempty_status": prev.status,
            "debug_mode": false,
            "test_mode": false,
        }})
    }

    /// POST /llm/dictation, refreshing the token once on 401/403.
    fn post(&self, mut s: Session, mut body: Value) -> Result<(Value, Session)> {
        for attempt in 0..2 {
            body["request"]["access_token"] = json!(s.access_token);
            let (code, text) = self.http.post_json(
                &format!("{API}/llm/dictation"),
                &[("Authorization", &s.access_token), ("Content-Type", "application/json")],
                &body,
            )?;
            if (code == 401 || code == 403) && attempt == 0 {
                s = self.fresh_session(true)?;
                continue;
            }
            if code >= 400 {
                return Err(format!("Wispr HTTP {code}: {}", detail(&text)));
            }
            let v = serde_json::from_str(&text).map_err(|_| "Wispr returned a non-JSON response")?;
            return Ok((v, s));
        }
        Err("Wispr rejected the session; run `linux-flow login`".into())
    }

    fn asr_fallback(&self, s: &Session, wav_b64: &str) -> Result<Value> {
        let lang = if self.languages.len() == 1 { self.languages[0].as_str() } else { "" };
        let (code, text) = self.http.post_json(
            &format!("{API}/llm/asr"),
            &[("Authorization", &s.access_token), ("Content-Type", "application/json")],
            &json!({"audio": wav_b64, "language": lang, "prompt": ""}),
        )?;
        if code >= 400 {
            return Err(format!("Wispr ASR fallback HTTP {code}: {}", detail(&text)));
        }
        let d: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok(json!({"status": "raw_transcript", "asr_text": d.get("content").cloned().unwrap_or(json!("")),
                  "detected_language": d.get("detected_language").cloned().unwrap_or(Value::Null)}))
    }

    /// Transcribe 16 kHz mono samples. Long recordings are sent in 28 s chunks.
    pub fn transcribe(&self, samples: &[i16]) -> Result<String> {
        let mut s = self.fresh_session(false)?;
        let chunk = CHUNK_SECONDS * RATE as usize;
        let n_chunks = samples.len().div_ceil(chunk).max(1);
        let entity = uuid::Uuid::new_v4().to_string();
        let mut prev = Prev::default();
        let mut last = Value::Null;
        for i in 0..n_chunks {
            let part = &samples[(i * chunk).min(samples.len())..((i + 1) * chunk).min(samples.len())];
            let wav_b64 = STANDARD.encode(to_wav(part, RATE));
            let body = self.request_body(&s, &wav_b64, i == n_chunks - 1, &prev, &entity);
            (last, s) = self.post(s, body)?;
            let mut status = last.get("status").and_then(Value::as_str).unwrap_or("").to_string();
            if status == "error" {
                last = self.asr_fallback(&s, &wav_b64)?;
                status = "raw_transcript".into();
            } else if !matches!(status.as_str(), "formatted" | "raw_transcript" | "empty" | "fallback") {
                eprintln!("[wispr] unexpected status {status:?}");
            }
            let get = |k: &str| last.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(String::from);
            if let Some(a) = get("asr_text") {
                prev.asr.push(a);
            }
            if let Some(l) = get("llm_text") {
                prev.llm = l;
            }
            if let Some(lang) = get("detected_language").filter(|l| l != "xx") {
                prev.lang = Some(lang);
            }
            if status == "formatted" || status == "raw_transcript" {
                prev.status = Some(status);
            }
        }
        // Same rule as the desktop app: the final (formatted) response covers the whole
        // session thanks to prev_asr_text; fall back to the stitched raw transcript.
        let get = |k: &str| last.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).map(String::from);
        let text = get("pasted_text").or_else(|| get("llm_text")).unwrap_or_else(|| prev.asr.join(" "));
        Ok(html_to_text(&text).trim().to_string())
    }
}

fn detail(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return body.chars().take(200).collect();
    };
    let det = v.get("detail").cloned().unwrap_or(v);
    let det = match &det {
        Value::Object(o) => ["message", "detail", "code"].iter().find_map(|k| o.get(*k).cloned()).unwrap_or(det.clone()),
        _ => det,
    };
    let s = match det {
        Value::String(s) => s,
        other => other.to_string(),
    };
    s.chars().take(200).collect()
}

// ----------------------------------------------------------------- formatting

/// pasted_text may contain light HTML (lists, breaks). Flatten to plain text.
pub fn html_to_text(s: &str) -> String {
    if !s.contains('<') {
        return unescape(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut lists: Vec<Option<u32>> = vec![]; // Some(n) for <ol>, None for <ul>
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let tag = rest[start + 1..start + end].trim();
        rest = &rest[start + end + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("ol" | "ul", true) => {
                lists.pop();
                out.push('\n');
            }
            ("ol", false) => {
                lists.push(Some(0));
                out.push('\n');
            }
            ("ul", false) => {
                lists.push(None);
                out.push('\n');
            }
            ("li", false) => match lists.last_mut() {
                Some(Some(n)) => {
                    *n += 1;
                    out.push_str(&format!("\n{n}. "));
                }
                _ => out.push_str("\n- "),
            },
            ("br", _) | ("p" | "div", true) => out.push('\n'),
            _ => {}
        }
    }
    out.push_str(rest);
    let mut text = unescape(&out);
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    text.trim_matches('\n').to_string()
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| c == ';' || c == '&' || c.is_whitespace()).map(|e| e + 1);
        let decoded = end.filter(|&e| rest.as_bytes().get(e) == Some(&b';')).and_then(|e| {
            let ent = &rest[1..e];
            let ch = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                    u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32)
                }
                _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            ch.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    pub fn jwt(claims: Value) -> String {
        let enc = |v: &Value| URL_SAFE_NO_PAD.encode(v.to_string());
        format!("{}.{}.sig", enc(&json!({"alg": "none"})), enc(&claims))
    }

    fn login(store: &SessionStore, exp_in: i64) {
        let tok = jwt(json!({"iss": "https://api.workos.com/user_management/client_X", "client_id": "client_X",
            "exp": now() + exp_in, "urn:wispr:user_external_id": "11111111-1111-1111-1111-111111111111",
            "email": "me@example.com"}));
        store.save(&session_from_tokens(&tok, "rt1", None, None)).unwrap();
    }

    fn temp_store(name: &str) -> SessionStore {
        let dir = std::env::temp_dir().join(format!("lf-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        SessionStore { path: dir.join("wispr_session.json") }
    }

    /// Fake Wispr server. `script` lists what /llm/dictation answers, in order.
    type Calls = std::sync::Arc<Mutex<Vec<(String, String, Value)>>>;

    struct Fake {
        script: Mutex<Vec<&'static str>>,
        calls: Calls,
    }

    impl Transport for Fake {
        fn post_json(&self, url: &str, headers: &[(&str, &str)], body: &Value) -> Result<(u16, String)> {
            let auth = headers.iter().find(|h| h.0 == "Authorization").map(|h| h.1.to_string()).unwrap_or_default();
            let path = url.splitn(4, '/').nth(3).map(|p| format!("/{p}")).unwrap_or_default();
            self.calls.lock().unwrap().push((path.clone(), auth, body.clone()));
            if path == "/llm/asr" {
                return Ok((200, json!({"content": "raw words", "detected_language": "en"}).to_string()));
            }
            if path == "/user_management/authenticate" {
                let tok = jwt(json!({"iss": "workos", "client_id": "client_X", "exp": now() + 3600,
                                     "urn:wispr:user_external_id": "1111"}));
                return Ok((200, json!({"access_token": tok, "refresh_token": "rt2"}).to_string()));
            }
            let n = self.calls.lock().unwrap().iter().filter(|c| c.0 == "/llm/dictation").count();
            match self.script.lock().unwrap().remove(0) {
                "401" => Ok((401, json!({"detail": "expired"}).to_string())),
                "error" => Ok((200, json!({"status": "error"}).to_string())),
                _ => Ok((200, json!({"status": "formatted", "asr_text": format!("chunk {n}"),
                    "llm_text": format!("Chunk {n}."), "pasted_text": format!("<p>Final {n}.</p>"),
                    "detected_language": "en"}).to_string())),
            }
        }
        fn get(&self, _url: &str) -> Result<(u16, String)> {
            Ok((404, String::new()))
        }
    }

    fn client(store: SessionStore, script: Vec<&'static str>) -> (Wispr, Calls) {
        let calls = Calls::default();
        let mut w = Wispr::new(&config::Config::default());
        w.store = store;
        w.http = Box::new(Fake { script: Mutex::new(script), calls: calls.clone() });
        (w, calls)
    }

    fn take(c: &Calls) -> Vec<(String, String, Value)> {
        c.lock().unwrap().clone()
    }

    #[test]
    fn html_flattening() {
        assert_eq!(html_to_text("Buy:<ol><li>milk</li><li>eggs</li></ol>"), "Buy:\n\n1. milk\n2. eggs");
        assert_eq!(html_to_text("a &amp; b<br>c"), "a & b\nc");
        assert_eq!(html_to_text("<ul><li>x</li></ul>"), "- x");
        assert_eq!(html_to_text("plain &lt;3 &#x1F600;"), "plain <3 \u{1F600}");
        assert_eq!(html_to_text("AT&T"), "AT&T");
    }

    #[test]
    fn session_parsing_and_storage() {
        let store = temp_store("sess");
        login(&store, 3600);
        let s = store.load().unwrap();
        assert_eq!((s.provider.as_str(), s.client_id.as_deref()), ("workos", Some("client_X")));
        assert!(s.user_id.unwrap().starts_with("1111") && s.session_id.is_some());
        assert!(store.describe().contains("me@example.com"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn python_session_file_is_compatible() {
        let store = temp_store("py");
        std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        std::fs::write(&store.path, r#"{"provider": "workos", "access_token": "a", "refresh_token": "r",
            "exp": 1, "client_id": "c", "user_id": "u", "email": "", "session_id": "s"}"#).unwrap();
        let s = store.load().unwrap();
        assert_eq!((s.client_id.as_deref(), s.session_id.as_deref()), (Some("c"), Some("s")));
    }

    #[test]
    fn single_request_body() {
        let store = temp_store("body");
        login(&store, 3600);
        let (w, c) = client(store, vec!["ok"]);
        assert_eq!(w.transcribe(&vec![500i16; 16000 * 3]).unwrap(), "Final 1.");
        let (path, auth, body) = take(&c)[0].clone();
        let req = &body["request"];
        assert_eq!(path, "/llm/dictation");
        assert!(!auth.starts_with("Bearer") && !auth.is_empty());
        assert_eq!(req["pipeline"], json!(["transcribe", "align", "format"]));
        assert_eq!(req["metadata"]["client_platform"], "win32");
        assert!(req["user"]["uuid"].as_str().unwrap().starts_with("1111"));
        let wav = STANDARD.decode(req["audio"].as_str().unwrap()).unwrap();
        assert_eq!((&wav[..4], wav.len()), (&b"RIFF"[..], 44 + 16000 * 3 * 2));
    }

    #[test]
    fn chunking_threads_prev_text() {
        let store = temp_store("chunk");
        login(&store, 3600);
        let (w, c) = client(store, vec!["ok", "ok", "ok"]);
        assert_eq!(w.transcribe(&vec![500i16; 16000 * 60]).unwrap(), "Final 3.");
        let reqs: Vec<Value> = take(&c).into_iter().map(|c| c.2["request"].clone()).collect();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0]["pipeline"], json!(["transcribe", "align"]));
        assert_eq!(reqs[2]["pipeline"][2], "format");
        assert_eq!(reqs[2]["prev_asr_text"], "chunk 1 chunk 2");
        let ids: std::collections::HashSet<_> = reqs.iter().map(|r| r["metadata"]["transcript_entity_uuid"].to_string()).collect();
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn refreshes_once_on_401() {
        let store = temp_store("401");
        login(&store, 3600);
        let (w, c) = client(store.clone(), vec!["401", "ok"]);
        assert_eq!(w.transcribe(&vec![500i16; 16000]).unwrap(), "Final 2.");
        let c = take(&c);
        assert_eq!(c[1].0, "/user_management/authenticate");
        assert_ne!(c[0].1, c[2].1, "retried with the new token");
        assert_eq!(store.load().unwrap().refresh_token, "rt2");
    }

    #[test]
    fn error_status_falls_back_to_asr() {
        let store = temp_store("asr");
        login(&store, 3600);
        let (w, c) = client(store, vec!["error"]);
        assert_eq!(w.transcribe(&vec![500i16; 16000]).unwrap(), "raw words");
        assert_eq!(take(&c)[1].0, "/llm/asr");
    }

    #[test]
    fn expired_token_refreshes_before_request() {
        let store = temp_store("exp");
        login(&store, -10);
        let (w, c) = client(store, vec!["ok"]);
        w.transcribe(&vec![500i16; 16000]).unwrap();
        assert_eq!(take(&c)[0].0, "/user_management/authenticate");
    }

    #[test]
    fn not_signed_in_is_a_clear_error() {
        let (w, _) = client(temp_store("none"), vec![]);
        assert!(w.transcribe(&[1, 2, 3]).unwrap_err().contains("linux-flow login"));
    }
}
