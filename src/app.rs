//! Main dictation loop: hotkey -> record -> transcribe -> (cleanup) -> type.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::{self, Recorder, RATE};
use crate::cleanup::Cleaner;
use crate::config::Config;
use crate::feedback::Feedback;
use crate::hotkey::{self, Combo, Edge};
use crate::inject::{session_type, Injector};
use crate::util::Result;
use crate::wispr::Wispr;
use crate::{instance, log};

pub fn run(cfg: Config) -> Result<()> {
    let _lock = instance::acquire()?; // held until exit; refuses a second copy
    let keys = cfg
        .hotkey
        .keys
        .iter()
        .map(|k| hotkey::parse_key(k).ok_or_else(|| format!("unknown key name {k:?} (run `linux-flow keys`)")))
        .collect::<Result<Vec<_>>>()?;

    let wispr = Wispr::new(&cfg);
    wispr.fresh_session(false)?; // fail fast if not signed in
    let injector = Injector { method: cfg.output.method.clone(), restore_clipboard: cfg.output.restore_clipboard };
    log!("session={} output={} hotkey={} ({})", session_type(), injector.resolve_method(), cfg.hotkey.keys.join("+"), cfg.hotkey.mode);

    let (key_tx, key_rx) = mpsc::channel();
    let n = hotkey::spawn_listeners(keys, key_tx)?;
    let feedback = Arc::new(Feedback::new(cfg.feedback.sounds, cfg.feedback.notifications));

    let (job_tx, job_rx) = mpsc::channel::<Vec<i16>>();
    {
        let cfg = cfg.clone();
        let feedback = feedback.clone();
        let cleaner = cfg.cleanup.enabled.then(|| Cleaner::new(&cfg.cleanup));
        std::thread::spawn(move || {
            for samples in job_rx {
                if let Err(e) = process(&cfg, &wispr, cleaner.as_ref(), &injector, &feedback, &samples) {
                    log!("error: {e}");
                    feedback.error(&e.chars().take(200).collect::<String>());
                }
            }
        });
    }

    let verb = if cfg.hotkey.mode == "hold" { "Hold" } else { "Press" };
    log!("ready ({n} keyboard{}). {verb} {} to dictate. Ctrl+C to quit.", if n == 1 { "" } else { "s" }, cfg.hotkey.keys.join("+"));

    let mut combo = Combo::new(&cfg.hotkey.keys);
    let mut rec = Recorder::new(&cfg.audio.device);
    let mut pressed_at = Instant::now();
    let toggle = cfg.hotkey.mode == "toggle";

    let begin = |rec: &mut Recorder, pressed_at: &mut Instant| {
        if rec.recording() {
            return;
        }
        match rec.start() {
            Ok(()) => {
                *pressed_at = Instant::now();
                feedback.started();
                log!("recording...");
            }
            Err(e) => {
                log!("mic error: {e}");
                feedback.error(&format!("Microphone error: {e}"));
            }
        }
    };
    let finish = |rec: &mut Recorder| {
        std::thread::sleep(Duration::from_millis(cfg.audio.tail_ms)); // don't clip the last word
        let samples = rec.stop();
        feedback.stopped();
        log!("captured {:.1}s", samples.len() as f64 / RATE as f64);
        let _ = job_tx.send(samples);
    };

    loop {
        match key_rx.recv_timeout(Duration::from_millis(500)) {
            Ok((name, down)) => {
                let edge = if down { combo.press(&name) } else { combo.release(&name) };
                match edge {
                    Some(Edge::Down) if toggle => {
                        if rec.recording() { finish(&mut rec) } else { begin(&mut rec, &mut pressed_at) }
                    }
                    Some(Edge::Down) => begin(&mut rec, &mut pressed_at),
                    Some(Edge::Up) if !toggle && rec.recording() => {
                        let held = pressed_at.elapsed().as_millis() as u64;
                        if held < cfg.hotkey.min_hold_ms {
                            rec.stop();
                            log!("tap ({held} ms) ignored");
                        } else {
                            finish(&mut rec);
                        }
                    }
                    _ => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if rec.recording() && rec.elapsed_secs() >= cfg.audio.max_seconds as f64 {
                    log!("max duration reached, stopping");
                    finish(&mut rec);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Err("keyboard listeners stopped".into()),
        }
    }
}

fn process(cfg: &Config, wispr: &Wispr, cleaner: Option<&Cleaner>, injector: &Injector, fb: &Feedback, samples: &[i16]) -> Result<()> {
    if audio::is_silent(samples) {
        log!("silence, skipped");
        return Ok(());
    }
    let t0 = Instant::now();
    let mut text = wispr.transcribe(samples)?;
    let took = t0.elapsed().as_secs_f64();
    if let (Some(c), false) = (cleaner, text.is_empty()) {
        match c.clean(&text) {
            Ok(t) => text = t,
            Err(e) => log!("cleanup failed, using Wispr text: {e}"),
        }
    }
    if text.is_empty() {
        log!("empty transcript");
        return Ok(());
    }
    if cfg.output.trailing_space && !text.ends_with([' ', '\n']) {
        text.push(' ');
    }
    let method = injector.insert(&text);
    log!("[{took:.2}s, {method}] {}", text.trim());
    if method == "clipboard-only" {
        fb.notify("linux-flow", "Couldn't type into the window; the text is on your clipboard.", "normal");
    } else if method.ends_with("-partial") {
        fb.notify("linux-flow", "Typing may be incomplete; the full text is on your clipboard.", "normal");
    }
    Ok(())
}
