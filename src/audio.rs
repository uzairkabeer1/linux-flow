//! Microphone capture (via parec/arecord, so the binary links no audio libraries) and WAV helpers.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::util::{have, Result};

pub const RATE: u32 = 16_000;

/// The capture command for the first available tool: 16 kHz mono s16le raw on stdout.
pub fn capture_command(device: &str) -> Option<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if have("parec") {
        // PulseAudio or PipeWire (pipewire-pulse); the default on most desktops
        let mut c = s(&["parec", "--format=s16le", "--rate=16000", "--channels=1", "--latency-msec=20"]);
        if !device.is_empty() {
            c.push(format!("--device={device}"));
        }
        return Some(c);
    }
    if have("arecord") {
        let mut c = s(&["arecord", "-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"]);
        if !device.is_empty() {
            c.extend(s(&["-D", device]));
        }
        return Some(c);
    }
    None
}

pub struct Recorder {
    device: String,
    child: Option<Child>,
    buf: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    started: Option<Instant>,
}

impl Recorder {
    pub fn new(device: &str) -> Self {
        Self { device: device.to_string(), child: None, buf: Arc::default(), reader: None, started: None }
    }

    pub fn recording(&self) -> bool {
        self.child.is_some()
    }

    pub fn start(&mut self) -> Result<()> {
        if self.recording() {
            return Ok(());
        }
        let cmd = capture_command(&self.device)
            .ok_or("no audio capture tool: install pulseaudio-utils (parec) or alsa-utils (arecord)")?;
        let mut child = Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", cmd[0]))?;
        let mut out = child.stdout.take().ok_or("capture tool has no stdout")?;
        self.buf.lock().unwrap().clear();
        let buf = Arc::clone(&self.buf);
        self.reader = Some(std::thread::spawn(move || {
            let mut chunk = [0u8; 3200];
            while let Ok(n) = out.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                buf.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        }));
        self.child = Some(child);
        self.started = Some(Instant::now());
        Ok(())
    }

    pub fn elapsed_secs(&self) -> f64 {
        match (self.recording(), self.started) {
            (true, Some(t)) => t.elapsed().as_secs_f64(),
            _ => 0.0,
        }
    }

    /// RMS of the last ~100 ms, 0..1.
    pub fn level(&self) -> f32 {
        let b = self.buf.lock().unwrap();
        let tail = &b[b.len().saturating_sub(3200) & !1..];
        rms(&bytes_to_i16(tail))
    }

    pub fn stop(&mut self) -> Vec<i16> {
        if let Some(mut child) = self.child.take() {
            // SAFETY: signalling our own child process.
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
            let _ = child.wait();
        }
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        self.started = None;
        bytes_to_i16(&std::mem::take(&mut *self.buf.lock().unwrap()))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn bytes_to_i16(b: &[u8]) -> Vec<i16> {
    b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

pub fn rms(s: &[i16]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let sum: f64 = s.iter().map(|&x| (x as f64 / 32768.0).powi(2)).sum();
    (sum / s.len() as f64).sqrt() as f32
}

pub fn is_silent(s: &[i16]) -> bool {
    rms(s) < 0.004
}

pub fn to_wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// Read a 16-bit PCM WAV, mix to mono and resample to 16 kHz.
pub fn read_wav(bytes: &[u8]) -> Result<Vec<i16>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut channels, mut rate, mut bits, mut data) = (0u16, 0u32, 0u16, None);
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
        let body = &bytes[i + 8..(i + 8 + len).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            let fmt = u16::from_le_bytes([body[0], body[1]]);
            if fmt != 1 && fmt != 0xFFFE {
                return Err("only PCM WAV is supported".into());
            }
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            data = Some(body);
        }
        i += 8 + len + (len & 1);
    }
    let data = data.ok_or("WAV has no data chunk")?;
    if bits != 16 || channels == 0 || rate == 0 {
        return Err(format!("need 16-bit PCM WAV (got {bits}-bit, {channels} ch)"));
    }
    let ch = channels as usize;
    let mono: Vec<i16> = bytes_to_i16(data)
        .chunks_exact(ch)
        .map(|f| (f.iter().map(|&x| x as i32).sum::<i32>() / ch as i32) as i16)
        .collect();
    Ok(resample(&mono, rate, RATE))
}

pub fn resample(s: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || s.is_empty() {
        return s.to_vec();
    }
    let n = (s.len() as u64 * to as u64 / from as u64) as usize;
    let step = (s.len() - 1) as f64 / (n.max(2) - 1) as f64;
    (0..n)
        .map(|i| {
            let x = i as f64 * step;
            let (j, frac) = (x as usize, x.fract());
            let a = s[j] as f64;
            let b = *s.get(j + 1).unwrap_or(&s[j]) as f64;
            (a + (b - a) * frac).round() as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip() {
        let s: Vec<i16> = (0..32000).map(|i| ((i as f64 / 10.0).sin() * 9000.0) as i16).collect();
        let w = to_wav(&s, RATE);
        assert_eq!(w.len(), 44 + s.len() * 2);
        assert_eq!(read_wav(&w).unwrap(), s);
        assert!(!is_silent(&s) && is_silent(&[0; 10]) && is_silent(&[]));
    }

    #[test]
    fn stereo_48k_becomes_mono_16k() {
        let mut w = to_wav(&vec![1000i16; 48000 * 2], 48000);
        w[22] = 2; // channels = 2
        let out = read_wav(&w).unwrap();
        assert_eq!(out.len(), 16000);
        assert!(out.iter().all(|&x| x == 1000));
    }
}
