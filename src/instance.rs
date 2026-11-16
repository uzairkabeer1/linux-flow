//! Single-instance guard, so autostart + a manual `linux-flow run` can't both dictate
//! (which would insert every transcript twice).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::util::{state_dir, Result};

pub fn lock_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("linux-flow.lock"),
        _ => state_dir().join("run.lock"),
    }
}

fn try_lock(f: &File) -> bool {
    // SAFETY: flock on a valid fd.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

fn read_pid(f: &mut File) -> Option<i32> {
    let mut s = String::new();
    f.rewind().ok()?;
    f.read_to_string(&mut s).ok()?;
    s.trim().parse().ok()
}

/// PID of the running instance, or None.
pub fn running_pid() -> Option<i32> {
    running_pid_at(&lock_path())
}

fn running_pid_at(p: &Path) -> Option<i32> {
    let mut f = OpenOptions::new().read(true).write(true).open(p).ok()?;
    if try_lock(&f) {
        return None; // nobody holds it (closing the file releases our probe lock)
    }
    Some(read_pid(&mut f).unwrap_or(-1))
}

/// Take the lock for the life of the process (keep the returned File alive).
pub fn acquire() -> Result<File> {
    acquire_at(&lock_path())
}

fn acquire_at(p: &Path) -> Result<File> {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(p)
        .map_err(|e| format!("{}: {e}", p.display()))?;
    if !try_lock(&f) {
        let pid = read_pid(&mut f).map(|p| p.to_string()).unwrap_or_else(|| "?".into());
        return Err(format!(
            "linux-flow is already running (pid {pid}), probably from autostart.\n\
             Just use the hotkey, or run `linux-flow stop` first to restart it here."
        ));
    }
    f.set_len(0).ok();
    f.rewind().ok();
    write!(f, "{}", std::process::id()).ok();
    f.flush().ok();
    Ok(f)
}

/// Is this command line a dictation daemon (any linux-flow version, Rust or the old Python)?
pub fn is_daemon_cmdline(args: &[String]) -> bool {
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    let Some(i) = args.iter().position(|a| base(a) == "linux-flow" || a == "linux_flow") else {
        return false;
    };
    // `linux-flow` alone or `linux-flow [--config X] run`
    let rest: Vec<&str> = args[i + 1..].iter().map(String::as_str).filter(|a| !a.is_empty()).collect();
    match rest.as_slice() {
        [] => true,
        ["--config", _, tail @ ..] => tail.is_empty() || tail[0] == "run",
        [first, ..] => *first == "run",
    }
}

/// Other dictation daemons running as this user, found via /proc. This catches copies the
/// lock can't see, such as the old Python version, which would type every transcript again.
pub fn other_daemons() -> Vec<(i32, String)> {
    let me = std::process::id() as i32;
    // SAFETY: getuid never fails.
    let uid = unsafe { libc::getuid() };
    let mut out = vec![];
    let Ok(dir) = std::fs::read_dir("/proc") else { return out };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        use std::os::unix::fs::MetadataExt;
        if e.metadata().map(|m| m.uid() != uid).unwrap_or(true) {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else { continue };
        let args: Vec<String> = raw.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        if is_daemon_cmdline(&args) {
            out.push((pid, args.join(" ").trim().to_string()));
        }
    }
    out
}

/// Stop every running dictation daemon (ours via the lock, plus any strays).
/// Returns the pids that were signalled.
pub fn stop() -> Vec<i32> {
    let mut pids: Vec<i32> = other_daemons().into_iter().map(|(p, _)| p).collect();
    if let Some(p) = running_pid().filter(|&p| p > 0) {
        if !pids.contains(&p) {
            pids.push(p);
        }
    }
    for &pid in &pids {
        // SAFETY: sending a signal.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    for _ in 0..30 {
        if running_pid().is_none() && other_daemons().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_instance() {
        let p = std::env::temp_dir().join(format!("lf-lock-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        assert_eq!(running_pid_at(&p), None);
        let held = acquire_at(&p).unwrap();
        assert_eq!(running_pid_at(&p), Some(std::process::id() as i32));
        // flock is per open file, so a second open in this process behaves like a second process
        assert!(acquire_at(&p).unwrap_err().contains("already running"));
        drop(held);
        assert_eq!(running_pid_at(&p), None);
        assert!(acquire_at(&p).is_ok());
    }

    #[test]
    fn recognises_daemons_of_any_version() {
        let v = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        // Rust binary, autostart and manual
        assert!(is_daemon_cmdline(&v("/home/u/.local/bin/linux-flow run")));
        assert!(is_daemon_cmdline(&v("linux-flow")));
        assert!(is_daemon_cmdline(&v("linux-flow --config /x.toml run")));
        // old Python version (venv script and -m)
        assert!(is_daemon_cmdline(&v("/home/u/.local/share/linux-flow/venv/bin/python3 /home/u/.local/bin/linux-flow run")));
        assert!(is_daemon_cmdline(&v("python3 -m linux_flow run")));
        // not daemons
        assert!(!is_daemon_cmdline(&v("linux-flow status")));
        assert!(!is_daemon_cmdline(&v("linux-flow stop")));
        assert!(!is_daemon_cmdline(&v("vim /home/u/linux-flow/README.md")));
        assert!(!is_daemon_cmdline(&v("sh -c exec \"/home/u/.local/bin/linux-flow\" run >>log")));
    }
}
