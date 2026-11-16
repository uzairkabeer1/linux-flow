//! linux-flow: hold-to-talk voice dictation for Linux using your Wispr Flow account.

mod app;
mod audio;
mod cleanup;
mod config;
mod feedback;
mod hotkey;
mod inject;
mod instance;
mod setup;
mod util;
mod wispr;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use util::{have, Result};

const HELP: &str = "\
linux-flow - hold-to-talk dictation for Linux, powered by your Wispr Flow account

USAGE: linux-flow [--config FILE] [COMMAND]

COMMANDS:
  run              start dictating (default; autostart runs this at login)
  login            sign in to Wispr Flow (opens your browser)
  logout           forget the stored Wispr session
  setup            one-time keyboard permission fix (asks for your password, no logout)
  status           show what was detected and what's missing
  stop             stop the running instance
  keys             print key names as you press them (to choose a hotkey)
  test-mic [--seconds N] [--out FILE.wav] [--transcribe]
                   record a few seconds, show levels, optionally transcribe
  transcribe FILE  transcribe a 16-bit WAV file
  version          print the version

Hold Right Ctrl, speak, release: the text is typed where your cursor is.
Config: ~/.config/linux-flow/config.toml   Logs: ~/.local/state/linux-flow/log.txt";

fn main() {
    let code = match real_main() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    std::process::exit(code);
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    args.iter().position(|a| a == flag).map(|i| args.remove(i)).is_some()
}

fn take_opt(args: &mut Vec<String>, flag: &str) -> Result<Option<String>> {
    match args.iter().position(|a| a == flag) {
        Some(i) if i + 1 < args.len() => {
            args.remove(i);
            Ok(Some(args.remove(i)))
        }
        Some(_) => Err(format!("{flag} needs a value")),
        None => Ok(None),
    }
}

fn real_main() -> Result<i32> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cfg_path = take_opt(&mut args, "--config")?.map(PathBuf::from);
    let cmd = if args.is_empty() { "run".to_string() } else { args.remove(0) };
    let cfg = || config::load(cfg_path.as_deref());

    match cmd.as_str() {
        "-h" | "--help" | "help" => println!("{HELP}"),
        "-V" | "--version" | "version" => println!("linux-flow {}", env!("CARGO_PKG_VERSION")),
        "run" => app::run(cfg()?)?,
        "login" => {
            let store = wispr::SessionStore::default();
            wispr::interactive_login(&store, &wispr::Http::new(Duration::from_secs(10)), Duration::from_secs(600))?;
        }
        "logout" => {
            wispr::SessionStore::default().clear();
            println!("Signed out.");
        }
        "setup" => setup::run(&hotkeys(&cfg()?)?)?,
        "status" => status(&cfg()?)?,
        "stop" => {
            let pids = instance::stop();
            if pids.is_empty() {
                println!("linux-flow is not running.");
            } else {
                let list: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
                println!("Stopped linux-flow (pid {}).", list.join(", "));
            }
        }
        "keys" => hotkey::watch_keys()?,
        "test-mic" => test_mic(&cfg()?, &mut args)?,
        "transcribe" => {
            let file = args.first().ok_or("usage: linux-flow transcribe FILE.wav")?;
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            transcribe_and_print(&cfg()?, &audio::read_wav(&bytes)?)?;
        }
        other => return Err(format!("unknown command {other:?}; see `linux-flow --help`")),
    }
    Ok(0)
}

fn hotkeys(cfg: &config::Config) -> Result<Vec<evdev::Key>> {
    cfg.hotkey
        .keys
        .iter()
        .map(|k| hotkey::parse_key(k).ok_or_else(|| format!("unknown key name {k:?}")))
        .collect()
}

fn yes_no(ok: bool, yes: &str, no: &str) -> String {
    if ok { format!("ok   {yes}") } else { format!("MISSING  {no}") }
}

fn status(cfg: &config::Config) -> Result<()> {
    let keys = hotkeys(cfg)?;
    let scan = hotkey::scan(&keys);
    let injector = inject::Injector { method: cfg.output.method.clone(), restore_clipboard: true };
    let st = inject::session_type();
    println!("linux-flow {}", env!("CARGO_PKG_VERSION"));
    let mut daemons = instance::other_daemons();
    if let Some(p) = instance::running_pid().filter(|p| !daemons.iter().any(|d| d.0 == *p)) {
        daemons.push((p, "linux-flow run".into()));
    }
    match daemons.len() {
        0 => println!("running:   no"),
        1 => println!("running:   yes (pid {})", daemons[0].0),
        n => {
            println!("running:   {n} COPIES - each dictation will be typed {n} times! Fix: linux-flow stop");
            for (p, c) in &daemons {
                println!("             pid {p}: {c}");
            }
        }
    }
    println!("config:    {}", config::default_path().display());
    println!("session:   {st} ({})", if inject::desktop().is_empty() { "unknown desktop".into() } else { inject::desktop() });
    println!("output:    {}", injector.resolve_method());
    println!("hotkey:    {} ({})", cfg.hotkey.keys.join("+"), cfg.hotkey.mode);
    println!("wispr:     {}", wispr::SessionStore::default().describe());
    println!("\nchecks:");
    println!(
        "  keyboard   {}",
        yes_no(!scan.matching.is_empty(), &format!("{} keyboard(s) with the hotkey", scan.matching.len()), "can't read the keyboard: run `linux-flow setup`")
    );
    if st == "wayland" && injector.resolve_method() == "paste" {
        println!("  uinput     {}", yes_no(setup::uinput_writable(), "virtual keyboard for Ctrl+V", "can't paste: run `linux-flow setup`"));
    }
    let mic = audio::capture_command("").map(|c| c[0].clone());
    println!("  microphone {}", yes_no(mic.is_some(), &mic.unwrap_or_default(), "install pulseaudio-utils (parec) or alsa-utils"));
    let clip = if st == "wayland" { have("wl-copy") } else { have("xclip") || have("xsel") };
    println!("  clipboard  {}", yes_no(clip, if st == "wayland" { "wl-copy" } else { "xclip/xsel" }, if st == "wayland" { "install wl-clipboard" } else { "install xclip" }));
    if st == "x11" {
        println!("  typing     {}", yes_no(have("xdotool"), "xdotool", "install xdotool"));
    }
    Ok(())
}

fn test_mic(cfg: &config::Config, args: &mut Vec<String>) -> Result<()> {
    use std::io::Write;
    let secs: f64 = take_opt(args, "--seconds")?.map(|s| s.parse().map_err(|_| "bad --seconds")).transpose()?.unwrap_or(4.0);
    let out = take_opt(args, "--out")?;
    let transcribe = take_flag(args, "--transcribe");
    let mut rec = audio::Recorder::new(&cfg.audio.device);
    println!("Recording {secs}s... speak now");
    rec.start()?;
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        let bar = "#".repeat(((rec.level() * 300.0) as usize).min(50));
        print!("\r  level |{bar:<50}|");
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(50));
    }
    let samples = rec.stop();
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    println!("\ncaptured {:.1}s, peak {peak}/32767", samples.len() as f64 / audio::RATE as f64);
    if let Some(path) = out {
        std::fs::write(&path, audio::to_wav(&samples, audio::RATE)).map_err(|e| e.to_string())?;
        println!("saved {path}");
    }
    if transcribe {
        transcribe_and_print(cfg, &samples)?;
    }
    Ok(())
}

fn transcribe_and_print(cfg: &config::Config, samples: &[i16]) -> Result<()> {
    let t0 = Instant::now();
    let text = wispr::Wispr::new(cfg).transcribe(samples)?;
    println!("[wispr, {:.2}s] {text}", t0.elapsed().as_secs_f64());
    Ok(())
}
