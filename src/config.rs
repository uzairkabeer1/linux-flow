//! Config file at $XDG_CONFIG_HOME/linux-flow/config.toml (written on first run).

use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::util::{config_dir, Result};

pub const DEFAULT_CONFIG: &str = r#"# linux-flow configuration

[general]
# Language hint, e.g. "en". Empty = auto-detect.
language = ""

[hotkey]
# evdev key names (see `linux-flow keys`). All keys in the list must be held.
keys = ["KEY_RIGHTCTRL"]
# "hold"   = talk while held, release to transcribe (push-to-talk)
# "toggle" = press once to start, press again to stop
mode = "hold"
# Ignore presses shorter than this (ms) in hold mode
min_hold_ms = 250

[audio]
# Capture device name (parec --device / arecord -D); empty = system default
device = ""
# Stop automatically after this many seconds
max_seconds = 360
# Keep recording this long after you release the key, so the last word isn't cut off
tail_ms = 150

[output]
# "auto" picks per session: xdotool on X11, wtype on wlroots Wayland, paste on GNOME/KDE.
# "paste" copies to clipboard and sends Ctrl+V (most reliable for unicode).
# Or force one of: "xdotool", "wtype", "ydotool", "clipboard" (copy only)
method = "auto"
# Restore previous clipboard contents after paste
restore_clipboard = true
# Append a trailing space after inserted text
trailing_space = true

[feedback]
sounds = true
notifications = true

[wispr]
# Language codes, e.g. ["en"]. Empty = auto-detect.
languages = []
# Let Wispr's server-side AI format the text (punctuation, lists, filler removal)
formatting = true
# Context hint for formatting: "other", "email", "ai", "terminal"
app_type = "other"
# Reported to Wispr's servers. "win32" mirrors the official Windows app (safest).
client_platform = "win32"
client_version = "1.6.1483"

[cleanup]
# Optional LLM polish pass on the final text (Wispr already formats server-side).
enabled = false
# "anthropic" (Claude) or "openai" (any OpenAI-compatible server, e.g. local Ollama)
provider = "anthropic"
# Claude model id. claude-haiku-5-5 is cheaper/faster if you prefer.
model = "claude-opus-5-5"
# For provider = "openai": e.g. "http://localhost:11434/v1"
base_url = ""
# The API key is read from this environment variable
api_key_env = "ANTHROPIC_API_KEY"
"#;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub hotkey: Hotkey,
    pub audio: Audio,
    pub output: Output,
    pub feedback: Feedback,
    pub wispr: Wispr,
    pub cleanup: Cleanup,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct General {
    pub language: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Hotkey {
    pub keys: Vec<String>,
    pub mode: String,
    pub min_hold_ms: u64,
}
impl Default for Hotkey {
    fn default() -> Self {
        Self { keys: vec!["KEY_RIGHTCTRL".into()], mode: "hold".into(), min_hold_ms: 250 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub device: String,
    pub max_seconds: u64,
    pub tail_ms: u64,
}
impl Default for Audio {
    fn default() -> Self {
        Self { device: String::new(), max_seconds: 360, tail_ms: 150 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Output {
    pub method: String,
    pub restore_clipboard: bool,
    pub trailing_space: bool,
}
impl Default for Output {
    fn default() -> Self {
        Self { method: "auto".into(), restore_clipboard: true, trailing_space: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Feedback {
    pub sounds: bool,
    pub notifications: bool,
}
impl Default for Feedback {
    fn default() -> Self {
        Self { sounds: true, notifications: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Wispr {
    pub languages: Vec<String>,
    pub formatting: bool,
    pub app_type: String,
    pub client_platform: String,
    pub client_version: String,
}
impl Default for Wispr {
    fn default() -> Self {
        Self {
            languages: vec![],
            formatting: true,
            app_type: "other".into(),
            client_platform: "win32".into(),
            client_version: "1.6.1483".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Cleanup {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key_env: String,
}
impl Default for Cleanup {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "anthropic".into(),
            model: "claude-opus-5-5".into(),
            base_url: String::new(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
        }
    }
}

pub fn default_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Load config, writing the default file first if none exists.
pub fn load(path: Option<&Path>) -> Result<Config> {
    let path = path.map(Path::to_path_buf).unwrap_or_else(default_path);
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, DEFAULT_CONFIG).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    if cfg.hotkey.keys.is_empty() {
        return Err("hotkey.keys must not be empty".into());
    }
    if !matches!(cfg.hotkey.mode.as_str(), "hold" | "toggle") {
        return Err(format!("hotkey.mode must be \"hold\" or \"toggle\", not {:?}", cfg.hotkey.mode));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_defaults() {
        let c = parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(c.hotkey.keys, vec!["KEY_RIGHTCTRL"]);
        assert_eq!(c.wispr.client_platform, "win32");
        assert!(c.wispr.formatting && !c.cleanup.enabled);
        assert_eq!(c.audio.max_seconds, Audio::default().max_seconds);
    }

    #[test]
    fn python_era_config_still_loads() {
        // Old keys (backend, [local], sample_rate) are ignored, missing ones defaulted.
        let c = parse("[general]\nbackend = \"wispr\"\n[local]\nmodel = \"base\"\n[audio]\nsample_rate = 16000\n").unwrap();
        assert_eq!(c.output.method, "auto");
        assert_eq!(c.wispr.client_platform, "win32");
    }

    #[test]
    fn rejects_bad_mode() {
        assert!(parse("[hotkey]\nmode = \"tap\"\n").is_err());
    }
}
