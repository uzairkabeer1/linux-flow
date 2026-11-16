//! Global hotkey via evdev (/dev/input): works on X11 and every Wayland compositor.

use evdev::{Device, InputEventKind, Key};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Virtual keyboards that send synthetic keystrokes (ours and ydotool's): never listen to them,
/// so our own Ctrl+V can't trigger the hotkey.
pub const IGNORED_DEVICES: [&str; 2] = ["linux-flow-vkbd", "ydotoold virtual device"];

pub fn key_name(k: Key) -> String {
    format!("{k:?}")
}

pub fn parse_key(name: &str) -> Option<Key> {
    let want = name.trim().to_ascii_uppercase();
    (0u16..0x300).map(Key::new).find(|k| key_name(*k) == want)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Edge {
    Down,
    Up,
}

/// Tracks a key combo: Down when all keys are held, Up when any of them is released.
pub struct Combo {
    keys: Vec<String>,
    held: HashSet<String>,
    active: bool,
}

impl Combo {
    pub fn new(keys: &[String]) -> Self {
        Combo { keys: keys.iter().map(|k| k.to_ascii_uppercase()).collect(), held: HashSet::new(), active: false }
    }

    pub fn press(&mut self, name: &str) -> Option<Edge> {
        if !self.keys.iter().any(|k| k == name) {
            return None;
        }
        self.held.insert(name.to_string());
        if !self.active && self.keys.iter().all(|k| self.held.contains(k)) {
            self.active = true;
            return Some(Edge::Down);
        }
        None
    }

    pub fn release(&mut self, name: &str) -> Option<Edge> {
        if !self.keys.iter().any(|k| k == name) {
            return None;
        }
        self.held.remove(name);
        if self.active {
            self.active = false;
            return Some(Edge::Up);
        }
        None
    }
}

/// What we can see in /dev/input: (paths of readable keyboards having `keys`, readable, total).
pub struct Scan {
    pub matching: Vec<PathBuf>,
    pub readable: usize,
    pub total: usize,
}

pub fn scan(keys: &[Key]) -> Scan {
    let mut s = Scan { matching: vec![], readable: 0, total: 0 };
    let Ok(dir) = std::fs::read_dir("/dev/input") else { return s };
    let mut paths: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("event")))
        .collect();
    paths.sort();
    for p in paths {
        s.total += 1;
        let Ok(dev) = Device::open(&p) else { continue };
        s.readable += 1;
        let ignored = dev.name().is_some_and(|n| IGNORED_DEVICES.contains(&n));
        let has_keys = dev.supported_keys().is_some_and(|ks| keys.iter().all(|k| ks.contains(*k)));
        if has_keys && !ignored {
            s.matching.push(p);
        }
    }
    s
}

pub fn permission_help() -> &'static str {
    "linux-flow can't read the keyboard. Fix it once with:\n\n    linux-flow setup\n\n\
     (asks for your password; no logout needed)"
}

/// Listen on every keyboard that has the hotkey; re-scan every 5 s for new ones.
/// Sends (key name, pressed) for key down/up events of the watched keys.
pub fn spawn_listeners(keys: Vec<Key>, tx: Sender<(String, bool)>) -> Result<usize, String> {
    let first = scan(&keys);
    if first.matching.is_empty() {
        if first.total > 0 && first.readable == 0 {
            return Err(permission_help().into());
        }
        return Err("no keyboard exposes the configured hotkey (run `linux-flow keys`)".into());
    }
    let open: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    let watch: HashSet<u16> = keys.iter().map(|k| k.code()).collect();
    let attach = {
        let open = open.clone();
        let tx = tx.clone();
        move |path: PathBuf| {
            if !open.lock().unwrap().insert(path.clone()) {
                return;
            }
            let (open, tx, watch) = (open.clone(), tx.clone(), watch.clone());
            std::thread::spawn(move || {
                if let Ok(mut dev) = Device::open(&path) {
                    'outer: while let Ok(events) = dev.fetch_events() {
                        for ev in events {
                            if let InputEventKind::Key(k) = ev.kind() {
                                // value 2 = autorepeat: ignore
                                if watch.contains(&k.code()) && ev.value() != 2
                                    && tx.send((key_name(k), ev.value() == 1)).is_err()
                                {
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
                open.lock().unwrap().remove(&path); // unplugged: allow re-attach
            });
        }
    };
    let n = first.matching.len();
    first.matching.into_iter().for_each(&attach);
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        scan(&keys).matching.into_iter().for_each(&attach);
    });
    Ok(n)
}

/// Print key names as they are pressed, to help pick a hotkey.
pub fn watch_keys() -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut n = 0;
    for (path, dev) in evdev::enumerate() {
        if dev.supported_keys().is_none() || dev.name().is_some_and(|n| IGNORED_DEVICES.contains(&n)) {
            continue;
        }
        n += 1;
        let tx = tx.clone();
        let label = dev.name().unwrap_or("?").to_string();
        drop(dev);
        std::thread::spawn(move || {
            if let Ok(mut d) = Device::open(&path) {
                while let Ok(evs) = d.fetch_events() {
                    for ev in evs {
                        if let InputEventKind::Key(k) = ev.kind() {
                            if ev.value() == 1 {
                                let _ = tx.send(format!("  {:<22} ({label})", key_name(k)));
                            }
                        }
                    }
                }
            }
        });
    }
    if n == 0 {
        return Err(permission_help().into());
    }
    println!("Press keys (Ctrl+C to quit):");
    for line in rx {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_edges() {
        let mut c = Combo::new(&["KEY_LEFTCTRL".into(), "KEY_SPACE".into()]);
        let mut ev = vec![];
        ev.extend(c.press("KEY_LEFTCTRL"));
        ev.extend(c.press("KEY_X"));
        ev.extend(c.press("KEY_SPACE"));
        ev.extend(c.press("KEY_SPACE"));
        ev.extend(c.release("KEY_LEFTCTRL"));
        ev.extend(c.release("KEY_SPACE"));
        assert_eq!(ev, vec![Edge::Down, Edge::Up]);
    }

    #[test]
    fn key_names_round_trip() {
        assert_eq!(parse_key("KEY_RIGHTCTRL"), Some(Key::KEY_RIGHTCTRL));
        assert_eq!(parse_key("key_f12"), Some(Key::KEY_F12));
        assert_eq!(parse_key("KEY_NOPE"), None);
    }

    #[test]
    fn own_virtual_keyboard_is_ignored() {
        assert!(IGNORED_DEVICES.contains(&"linux-flow-vkbd"));
    }
}
