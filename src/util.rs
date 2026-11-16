//! Small shared helpers: XDG paths, logging, running external commands.

use std::env;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, String>;

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("linux-flow")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("linux-flow")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("linux-flow")
}

/// Local wall-clock time as HH:MM:SS.
pub fn now_hms() -> String {
    // SAFETY: plain libc calls on stack values.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { println!("[{}] {}", $crate::util::now_hms(), format!($($arg)*)) };
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths).map(|d| d.join(cmd)).find(|p| {
        p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

pub fn have(cmd: &str) -> bool {
    which(cmd).is_some()
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Ok,
    Failed { code: i32, stderr: String },
    Timeout,
    NotStarted(String),
}

/// Run a command with optional stdin, a timeout, and captured stderr.
///
/// Tools like wl-copy/xclip fork a background process that inherits stderr, so we
/// never block on stderr EOF: it's read on a thread and collected with a short wait.
pub fn run_cmd(args: &[&str], input: Option<&[u8]>, timeout: Duration) -> RunOutcome {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunOutcome::NotStarted(e.to_string()),
    };
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = stdin.write_all(data);
    } // stdin dropped here -> EOF
    let (tx, rx) = mpsc::channel();
    if let Some(mut err) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            let _ = tx.send(s);
        });
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return RunOutcome::Ok;
                }
                let stderr = rx.recv_timeout(Duration::from_millis(300)).unwrap_or_default();
                return RunOutcome::Failed { code: status.code().unwrap_or(-1), stderr: stderr.trim().to_string() };
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return RunOutcome::Timeout;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return RunOutcome::NotStarted(e.to_string()),
        }
    }
}

/// Run a command, return its stdout if it succeeded within the timeout.
pub fn output_of(args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let mut child = Command::new(args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let buf = match rx.recv_timeout(timeout) {
        Ok(b) => b,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let status = child.wait().ok()?;
    status.success().then_some(buf)
}

/// Fire-and-forget a command, reaping it on a thread so no zombies pile up.
pub fn spawn_detached(args: Vec<String>) {
    std::thread::spawn(move || {
        if let Ok(mut c) = Command::new(&args[0])
            .args(&args[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let _ = c.wait();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_cmd_reports_outcomes() {
        assert_eq!(run_cmd(&["true"], None, Duration::from_secs(5)), RunOutcome::Ok);
        match run_cmd(&["sh", "-c", "echo oops >&2; exit 3"], None, Duration::from_secs(5)) {
            RunOutcome::Failed { code, stderr } => assert_eq!((code, stderr.as_str()), (3, "oops")),
            o => panic!("{o:?}"),
        }
        assert_eq!(run_cmd(&["sleep", "5"], None, Duration::from_millis(100)), RunOutcome::Timeout);
        assert!(matches!(run_cmd(&["no-such-binary-xyz"], None, Duration::from_secs(1)), RunOutcome::NotStarted(_)));
    }

    #[test]
    fn stdin_is_delivered() {
        assert_eq!(output_of(&["cat"], Duration::from_secs(2)), Some(vec![]));
        assert_eq!(run_cmd(&["sh", "-c", "[ \"$(cat)\" = hi ]"], Some(b"hi"), Duration::from_secs(2)), RunOutcome::Ok);
    }
}
