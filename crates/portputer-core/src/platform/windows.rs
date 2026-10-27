//! Windows: key events through SendInput, by scan code so that they're the
//! same physical keys whatever the keyboard layout; power through the
//! shutdown command and the Win32 calls for lock and sleep.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, SendInput,
};

use super::{Held, Injector};
use crate::keys::{self, WinKey};
use crate::protocol::{PowerAction, SessionKind};

pub struct WinInjector {
    held: Held,
}

impl WinInjector {
    pub fn new(_session: SessionKind) -> WinInjector {
        WinInjector { held: Held::default() }
    }
}

fn send(inputs: &[KEYBDINPUT]) {
    let inputs: Vec<INPUT> = inputs
        .iter()
        .map(|ki| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: *ki } })
        .collect();
    unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
}

fn key_input(vk: u16, scan: u16, flags: u32) -> KEYBDINPUT {
    KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 }
}

/// Virtual keys Windows treats as extended whatever their scan code.
fn vk_extended(vk: u16) -> bool {
    matches!(vk, 0x90 | 0x2C | 0xA6..=0xB7)
}

fn post(def: &keys::KeyDef, down: bool) -> bool {
    let up = if down { 0 } else { KEYEVENTF_KEYUP };
    match def.win {
        WinKey::Scan(scan) => {
            let extended = if scan & 0xFF00 == 0xE000 { KEYEVENTF_EXTENDEDKEY } else { 0 };
            send(&[key_input(0, scan & 0xFF, KEYEVENTF_SCANCODE | extended | up)]);
            true
        }
        WinKey::Vk(vk) => {
            let extended = if vk_extended(vk) { KEYEVENTF_EXTENDEDKEY } else { 0 };
            send(&[key_input(vk, 0, extended | up)]);
            true
        }
        WinKey::None => false,
    }
}

impl Injector for WinInjector {
    fn key(&mut self, code: &str, down: bool) -> bool {
        let Some(def) = keys::by_code(code) else { return false };
        if !post(def, down) {
            return false;
        }
        self.held.note(code, down);
        true
    }

    fn text(&mut self, text: &str) {
        let mut inputs = vec![];
        for unit in text.encode_utf16() {
            inputs.push(key_input(0, unit, KEYEVENTF_UNICODE));
            inputs.push(key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        for chunk in inputs.chunks(64) {
            send(chunk);
        }
    }

    fn release_all(&mut self) {
        for code in self.held.take() {
            if let Some(def) = keys::by_code(&code) {
                post(def, false);
            }
        }
    }
}

pub fn power_actions(session: SessionKind) -> Vec<PowerAction> {
    use PowerAction::*;
    match session {
        SessionKind::User => vec![Restart, Shutdown, Sleep, LogOut, Lock, ForceRestart],
        SessionKind::LoginWindow => vec![Restart, Shutdown, Sleep, ForceRestart],
    }
}

pub fn power(_session: SessionKind, action: PowerAction, _user: Option<&str>, _password: Option<&str>) -> Result<(), String> {
    use PowerAction::*;
    match action {
        Restart => super::run("shutdown.exe", &["/r", "/t", "0"]),
        ForceRestart => super::run("shutdown.exe", &["/r", "/f", "/t", "0"]),
        Shutdown => super::run("shutdown.exe", &["/s", "/t", "0"]),
        LogOut => super::run("shutdown.exe", &["/l"]),
        Lock => {
            let ok = unsafe { windows_sys::Win32::System::Shutdown::LockWorkStation() };
            if ok != 0 { Ok(()) } else { Err("LockWorkStation failed".into()) }
        }
        Sleep => {
            let ok = unsafe { windows_sys::Win32::System::Power::SetSuspendState(false, false, false) };
            if ok { Ok(()) } else { Err("SetSuspendState failed".into()) }
        }
        UnlockedRestart => Err("not available on Windows yet".into()),
    }
}
