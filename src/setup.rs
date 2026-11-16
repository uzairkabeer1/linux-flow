//! One-time permission setup: lets the logged-in user read keyboards (global hotkey)
//! and write /dev/uinput (paste on Wayland), without joining the `input` group or
//! logging out.

use std::process::Command;
use std::time::Duration;

use evdev::Key;

use crate::hotkey;
use crate::util::Result;

/// `uaccess` makes systemd-logind grant an ACL to whoever is logged in at the physical
/// seat, and it applies immediately on `udevadm trigger`. Must sort before 73-seat-late.
pub const UDEV_RULES: &str = "\
# linux-flow: let the logged-in desktop user read keyboards (global hotkey) and
# use /dev/uinput (paste on Wayland). Applies live, no logout needed.
SUBSYSTEM==\"input\", KERNEL==\"event*\", ENV{ID_INPUT_KEYBOARD}==\"1\", TAG+=\"uaccess\"
KERNEL==\"uinput\", SUBSYSTEM==\"misc\", TAG+=\"uaccess\", OPTIONS+=\"static_node=uinput\"
";
pub const RULES_PATH: &str = "/etc/udev/rules.d/70-linux-flow.rules";

pub fn uinput_writable() -> bool {
    let p = std::ffi::CString::new("/dev/uinput").unwrap();
    // SAFETY: access() on a valid C string.
    unsafe { libc::access(p.as_ptr(), libc::W_OK) == 0 }
}

pub fn keyboard_readable(keys: &[Key]) -> bool {
    !hotkey::scan(keys).matching.is_empty()
}

fn sudo_sh(script: &str) -> Result<()> {
    let ok = Command::new("sudo").args(["sh", "-c", script]).status().map_err(|e| format!("sudo: {e}"))?.success();
    if ok { Ok(()) } else { Err("the setup command failed (wrong password or no sudo?)".into()) }
}

pub fn run(keys: &[Key]) -> Result<()> {
    if keyboard_readable(keys) && uinput_writable() {
        println!("Permissions already OK: keyboard readable, /dev/uinput writable.");
        return Ok(());
    }
    println!("Granting keyboard access (needs your password once)...");
    let script = format!(
        "set -e
cat > {RULES_PATH} <<'EOF'
{UDEV_RULES}EOF
echo uinput > /etc/modules-load.d/linux-flow-uinput.conf
modprobe uinput 2>/dev/null || true
udevadm control --reload-rules
udevadm trigger --subsystem-match=input --action=change
udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
udevadm settle 2>/dev/null || true
"
    );
    sudo_sh(&script)?;
    for _ in 0..20 {
        if keyboard_readable(keys) && uinput_writable() {
            println!("Done. linux-flow can read the keyboard now; no logout needed.");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    // No logind seat ACLs (e.g. some minimal or non-systemd setups): fall back to the group.
    let user = std::env::var("USER").unwrap_or_default();
    if !user.is_empty() {
        sudo_sh(&format!("usermod -aG input '{user}'"))?;
    }
    println!(
        "Your system didn't apply the access rule live, so you were added to the 'input' group.\n\
         Log out and back in once (or start it now with: sg input -c 'linux-flow run')."
    );
    Ok(())
}
