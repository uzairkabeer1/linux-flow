//! Audible cues (tiny generated WAVs played with pw-play/paplay/aplay) and notifications.

use std::path::PathBuf;

use crate::audio::to_wav;
use crate::util::{cache_dir, have, spawn_detached};

fn tone(freqs: &[f64], dur: f64) -> Vec<i16> {
    let rate = 44_100.0;
    let n = (rate * dur) as usize;
    freqs
        .iter()
        .flat_map(|&f| {
            (0..n).map(move |i| {
                let t = i as f64 / rate;
                let env = (t.min(dur - t) / 0.01).min(1.0); // 10 ms fade in/out
                ((2.0 * std::f64::consts::PI * f * t).sin() * env * 0.18 * 32767.0) as i16
            })
        })
        .collect()
}

pub struct Feedback {
    sounds: bool,
    notifications: bool,
    player: Option<Vec<String>>,
    files: [PathBuf; 3],
}

impl Feedback {
    pub fn new(sounds: bool, notifications: bool) -> Self {
        let dir = cache_dir();
        let files = [dir.join("start.wav"), dir.join("stop.wav"), dir.join("error.wav")];
        let player = ["pw-play", "paplay", "aplay"].iter().find(|p| have(p)).map(|p| {
            let mut v = vec![p.to_string()];
            if *p == "aplay" {
                v.push("-q".into());
            }
            v
        });
        if sounds && player.is_some() {
            let _ = std::fs::create_dir_all(&dir);
            let waves = [tone(&[660.0, 990.0], 0.07), tone(&[990.0, 660.0], 0.07), tone(&[300.0, 220.0], 0.12)];
            for (f, w) in files.iter().zip(waves) {
                if !f.exists() {
                    let _ = std::fs::write(f, to_wav(&w, 44_100));
                }
            }
        }
        Feedback { sounds, notifications, player, files }
    }

    fn play(&self, i: usize) {
        if let (true, Some(p)) = (self.sounds, &self.player) {
            let mut args = p.clone();
            args.push(self.files[i].to_string_lossy().into_owned());
            spawn_detached(args);
        }
    }

    pub fn started(&self) {
        self.play(0);
    }

    pub fn stopped(&self) {
        self.play(1);
    }

    pub fn error(&self, msg: &str) {
        self.play(2);
        self.notify("linux-flow", msg, "critical");
    }

    pub fn notify(&self, title: &str, body: &str, urgency: &str) {
        if self.notifications && have("notify-send") {
            spawn_detached(
                ["notify-send", "-a", "linux-flow", "-u", urgency, "-t", "4000", title, body]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            );
        }
    }
}
