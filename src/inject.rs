//! Insert text into the focused window on X11 or Wayland, exactly once.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::util::{have, output_of, run_cmd, RunOutcome};

const TERMINAL_HINTS: [&str; 16] = [
    "terminal", "konsole", "kitty", "alacritty", "wezterm", "foot", "xterm", "urxvt", "tilix",
    "terminator", "ghostty", "warp", "st-256color", "rio", "ptyxis", "blackbox",
];

pub fn session_type() -> &'static str {
    let t = std::env::var("XDG_SESSION_TYPE").unwrap_or_default().to_lowercase();
    if t == "x11" {
        return "x11";
    }
    if t == "wayland" || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return "wayland";
    }
    if std::env::var_os("DISPLAY").is_some() {
        return "x11";
    }
    "unknown"
}

pub fn desktop() -> String {
    std::env::var("XDG_CURRENT_DESKTOP")
        .or_else(|_| std::env::var("DESKTOP_SESSION"))
        .unwrap_or_default()
        .to_lowercase()
}

// --- clipboard --------------------------------------------------------------

pub fn clipboard_get() -> Option<String> {
    let t = Duration::from_secs(2);
    let out = if session_type() == "wayland" && have("wl-paste") {
        output_of(&["wl-paste", "--no-newline", "--type", "text"], t)
    } else if have("xclip") {
        output_of(&["xclip", "-selection", "clipboard", "-o"], t)
    } else if have("xsel") {
        output_of(&["xsel", "--clipboard", "--output"], t)
    } else {
        None
    }?;
    String::from_utf8(out).ok()
}

pub fn clipboard_set(text: &str) -> bool {
    let t = Duration::from_secs(3);
    let data = Some(text.as_bytes());
    if session_type() == "wayland" && have("wl-copy") {
        return run_cmd(&["wl-copy", "--type", "text/plain;charset=utf-8"], data, t) == RunOutcome::Ok;
    }
    if have("xclip") {
        return run_cmd(&["xclip", "-selection", "clipboard", "-i"], data, t) == RunOutcome::Ok;
    }
    if have("xsel") {
        return run_cmd(&["xsel", "--clipboard", "--input"], data, t) == RunOutcome::Ok;
    }
    eprintln!("[inject] no clipboard tool (install wl-clipboard or xclip)");
    false
}

// --- key chords -------------------------------------------------------------

fn active_window_is_terminal() -> bool {
    if session_type() != "x11" || !have("xdotool") {
        return false;
    }
    output_of(&["xdotool", "getactivewindow", "getwindowclassname"], Duration::from_secs(1))
        .map(|o| {
            let cls = String::from_utf8_lossy(&o).to_lowercase();
            TERMINAL_HINTS.iter().any(|h| cls.contains(h))
        })
        .unwrap_or(false)
}

static VKBD: OnceLock<Mutex<Option<evdev::uinput::VirtualDevice>>> = OnceLock::new();

/// Send Ctrl(+Shift)+V through a virtual keyboard (/dev/uinput).
fn uinput_paste(shift: bool) -> bool {
    use evdev::{uinput::VirtualDeviceBuilder, AttributeSet, EventType, InputEvent, Key};
    let lock = VKBD.get_or_init(|| Mutex::new(None));
    let mut guard = lock.lock().unwrap();
    if guard.is_none() {
        let keys = AttributeSet::from_iter([Key::KEY_LEFTCTRL, Key::KEY_LEFTSHIFT, Key::KEY_V]);
        match VirtualDeviceBuilder::new().and_then(|b| b.name("linux-flow-vkbd").with_keys(&keys)).and_then(|b| b.build()) {
            Ok(d) => {
                *guard = Some(d);
                std::thread::sleep(Duration::from_millis(200)); // let the compositor pick it up
            }
            Err(e) => {
                eprintln!("[inject] uinput unavailable: {e} (run `linux-flow setup`)");
                return false;
            }
        }
    }
    let dev = guard.as_mut().unwrap();
    let mut mods = vec![Key::KEY_LEFTCTRL];
    if shift {
        mods.push(Key::KEY_LEFTSHIFT);
    }
    let ev = |k: Key, v: i32| InputEvent::new(EventType::KEY, k.code(), v);
    let mut down: Vec<_> = mods.iter().map(|&m| ev(m, 1)).collect();
    down.push(ev(Key::KEY_V, 1));
    let mut up = vec![ev(Key::KEY_V, 0)];
    up.extend(mods.iter().rev().map(|&m| ev(m, 0)));
    let ok = dev.emit(&down).is_ok();
    std::thread::sleep(Duration::from_millis(12));
    ok && dev.emit(&up).is_ok()
}

fn send_paste_chord(shift: bool) -> bool {
    let t = Duration::from_secs(5);
    if session_type() == "x11" && have("xdotool") {
        let chord = if shift { "ctrl+shift+v" } else { "ctrl+v" };
        return run_cmd(&["xdotool", "key", "--clearmodifiers", chord], None, t) == RunOutcome::Ok;
    }
    if uinput_paste(shift) {
        return true;
    }
    if have("ydotool") {
        // 29=LEFTCTRL 42=LEFTSHIFT 47=V
        let seq: &[&str] = if shift {
            &["ydotool", "key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]
        } else {
            &["ydotool", "key", "29:1", "47:1", "47:0", "29:0"]
        };
        return run_cmd(seq, None, t) == RunOutcome::Ok;
    }
    if session_type() == "wayland" && have("wtype") {
        let args: &[&str] = if shift {
            &["wtype", "-M", "ctrl", "-M", "shift", "v", "-m", "shift", "-m", "ctrl"]
        } else {
            &["wtype", "-M", "ctrl", "v", "-m", "ctrl"]
        };
        return run_cmd(args, None, t) == RunOutcome::Ok;
    }
    false
}

// --- typing tools -----------------------------------------------------------

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Typed {
    Ok,
    /// Failed before typing anything: safe to fall back to pasting.
    Nothing,
    /// May have typed part of the text: pasting now would duplicate it.
    Partial,
}

/// stderr fragments meaning the tool failed before typing anything
const NOTHING_TYPED: [&str; 6] = [
    "virtual keyboard",  // wtype on GNOME/KDE
    "failed to connect", // ydotool without ydotoold
    "can't open display",
    "cannot open display",
    "no such file",
    "permission denied",
];

pub fn classify(o: &RunOutcome) -> Typed {
    match o {
        RunOutcome::Ok => Typed::Ok,
        RunOutcome::NotStarted(_) => Typed::Nothing,
        RunOutcome::Timeout => Typed::Partial,
        RunOutcome::Failed { stderr, .. } => {
            let e = stderr.to_lowercase();
            if NOTHING_TYPED.iter().any(|s| e.contains(s)) { Typed::Nothing } else { Typed::Partial }
        }
    }
}

fn typer_args(method: &str, text: &str) -> Option<Vec<String>> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).chain([text.to_string()]).collect();
    match method {
        "xdotool" => Some(v(&["xdotool", "type", "--clearmodifiers", "--delay", "1", "--"])),
        "wtype" => Some(v(&["wtype", "--"])),
        "ydotool" => Some(v(&["ydotool", "type", "--"])),
        _ => None,
    }
}

fn type_with(method: &str, text: &str) -> Typed {
    let Some(args) = typer_args(method, text) else { return Typed::Nothing };
    if !have(&args[0]) {
        return Typed::Nothing;
    }
    // generous timeout: never kill it mid-sentence
    let timeout = Duration::from_secs_f64(30.0 + text.chars().count() as f64 * 0.05);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_cmd(&refs, None, timeout);
    if out != RunOutcome::Ok {
        eprintln!("[inject] {} failed: {out:?}", args[0]);
    }
    classify(&out)
}

/// Core decision logic, separated for testing. Returns the method actually used.
pub fn insert_with(
    text: &str,
    method: &str,
    typer: impl Fn(&str, &str) -> Typed,
    paste: impl Fn(&str) -> bool,
    clip: impl Fn(&str) -> bool,
) -> String {
    if text.is_empty() {
        return "none".into();
    }
    if method == "clipboard" {
        return if clip(text) { "clipboard".into() } else { "clipboard-only".into() };
    }
    if typer_args(method, "").is_some() {
        match typer(method, text) {
            Typed::Ok => return method.into(),
            Typed::Partial => {
                clip(text);
                return format!("{method}-partial");
            }
            Typed::Nothing => {} // try pasting instead
        }
    }
    if paste(text) {
        return "paste".into();
    }
    clip(text);
    "clipboard-only".into()
}

pub struct Injector {
    pub method: String,
    pub restore_clipboard: bool,
}

impl Injector {
    pub fn resolve_method(&self) -> String {
        if self.method != "auto" {
            return self.method.clone();
        }
        match session_type() {
            "x11" if have("xdotool") => "xdotool".into(),
            // wtype is great on wlroots compositors (sway, Hyprland, river...) but GNOME/KDE
            // lack the virtual-keyboard protocol it needs -> paste instead.
            "wayland" if have("wtype") && !["gnome", "kde", "plasma"].iter().any(|d| desktop().contains(d)) => {
                "wtype".into()
            }
            _ => "paste".into(),
        }
    }

    pub fn insert(&self, text: &str) -> String {
        insert_with(text, &self.resolve_method(), type_with, |t| self.paste(t), clipboard_set)
    }

    fn paste(&self, text: &str) -> bool {
        let previous = if self.restore_clipboard { clipboard_get() } else { None };
        if !clipboard_set(text) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(60));
        let ok = send_paste_chord(active_window_is_terminal());
        if let (true, Some(prev)) = (ok, previous) {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(600)); // let the app read the clipboard
                clipboard_set(&prev);
            });
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn run(method: &str, typed: Typed) -> (String, u32, u32) {
        let (typer_calls, pastes) = (Cell::new(0), Cell::new(0));
        let m = insert_with(
            "hello",
            method,
            |_, _| {
                typer_calls.set(typer_calls.get() + 1);
                typed
            },
            |_| {
                pastes.set(pastes.get() + 1);
                true
            },
            |_| true,
        );
        (m, typer_calls.get(), pastes.get())
    }

    #[test]
    fn typing_success_does_not_paste() {
        assert_eq!(run("xdotool", Typed::Ok), ("xdotool".into(), 1, 0));
    }

    #[test]
    fn partial_typing_never_pastes_again() {
        assert_eq!(run("xdotool", Typed::Partial), ("xdotool-partial".into(), 1, 0));
    }

    #[test]
    fn typer_that_typed_nothing_falls_back_to_paste() {
        assert_eq!(run("wtype", Typed::Nothing), ("paste".into(), 1, 1));
    }

    #[test]
    fn paste_method_pastes_once() {
        assert_eq!(run("paste", Typed::Ok), ("paste".into(), 0, 1));
    }

    #[test]
    fn classify_outcomes() {
        let failed = |s: &str| RunOutcome::Failed { code: 1, stderr: s.into() };
        assert_eq!(classify(&RunOutcome::Timeout), Typed::Partial);
        assert_eq!(classify(&RunOutcome::NotStarted("x".into())), Typed::Nothing);
        assert_eq!(classify(&failed("Compositor does not support the virtual keyboard protocol")), Typed::Nothing);
        assert_eq!(classify(&failed("XGetWindowProperty failed")), Typed::Partial);
    }
}
