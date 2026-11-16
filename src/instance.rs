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

/// SIGTERM the running instance and wait for it to exit. Returns its pid if one was running.
pub fn stop() -> Option<i32> {
    let pid = running_pid().filter(|&p| p > 0)?;
    // SAFETY: sending a signal.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    for _ in 0..30 {
        if running_pid().is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Some(pid)
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
}
