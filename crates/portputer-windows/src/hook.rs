//! Sending the keyboard to the computer: a low-level keyboard hook sees every
//! key before Windows acts on it, so Windows' own shortcuts, such as Alt-Tab
//! and the Windows key, go to the computer too. Only Control-Alt-Delete and
//! Windows-L can't be taken.
//!
//! - Keys are taken only while Portputer's window is in front.
//! - Control-Windows-Escape gives the keyboard back to Windows, as
//!   Control-Command-Escape does on the Mac.
//! - Keys other programs inject go to Windows.
//! - A key whose press was taken has its release taken too, even after the
//!   keyboard comes back, so the Windows key's release doesn't open Start.
//! - If Portputer stops responding, Windows removes the hook by itself.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use portputer_core::keys;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui;

const VK_ESCAPE: u32 = 0x1B;

pub struct Handlers {
    pub key: fn(&str, bool, bool),
    pub escape: fn(),
}

struct Hook {
    handle: HHOOK,
    capturing: bool,
    handlers: Handlers,
    /// Keys sent down to the computer, by scan code, with their codes.
    sent: HashMap<u32, String>,
    /// Keys whose press was kept from Windows.
    swallowed: HashSet<u32>,
}

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Starts taking the keyboard. False if Windows wouldn't install the hook.
pub fn start(handlers: Handlers) -> bool {
    HOOK.with(|cell| {
        let mut hook = cell.borrow_mut();
        if let Some(h) = hook.as_mut() {
            h.capturing = true;
            return true;
        }
        let Ok(handle) = (unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), Some(ui::hinstance()), 0) }) else {
            return false;
        };
        *hook = Some(Hook { handle, capturing: true, handlers, sent: HashMap::new(), swallowed: HashSet::new() });
        true
    })
}

/// Gives the keyboard back, releasing on the computer whatever is held
/// there. The hook stays until every taken key is released.
pub fn stop() {
    HOOK.with(|cell| {
        let mut hook = cell.borrow_mut();
        if let Some(h) = hook.as_mut() {
            h.capturing = false;
            for (_, code) in h.sent.drain() {
                (h.handlers.key)(&code, false, false);
            }
        }
        remove_if_idle(&mut hook);
    });
}

fn remove_if_idle(hook: &mut Option<Hook>) {
    if hook.as_ref().is_some_and(|h| !h.capturing && h.swallowed.is_empty()) {
        if let Some(h) = hook.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(h.handle);
            }
        }
    }
}

fn code_for(info: &KBDLLHOOKSTRUCT) -> Option<&'static str> {
    if let Some(code) = keys::from_windows_vk(info.vkCode as u16) {
        return Some(code);
    }
    let extended = info.flags.0 & LLKHF_EXTENDED.0 != 0;
    let scan = info.scanCode as u16;
    if extended {
        keys::from_windows_scan(0xE000 | scan).or_else(|| keys::from_windows_scan(scan))
    } else {
        keys::from_windows_scan(scan)
    }
}

fn is_down(vk: i32) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let pass = || unsafe { CallNextHookEx(None, code, wparam, lparam) };
    if code < 0 {
        return pass();
    }
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if info.flags.0 & LLKHF_INJECTED.0 != 0 {
        return pass();
    }
    let up = info.flags.0 & LLKHF_UP.0 != 0;
    let id = info.scanCode | if info.flags.0 & LLKHF_EXTENDED.0 != 0 { 0x100 } else { 0 } | (info.vkCode << 16);
    let taken = HOOK.with(|cell| {
        let Ok(mut hook) = cell.try_borrow_mut() else { return false };
        let Some(h) = hook.as_mut() else { return false };
        if up {
            let was_swallowed = h.swallowed.remove(&id);
            if let Some(name) = h.sent.remove(&id) {
                (h.handlers.key)(&name, false, false);
            }
            remove_if_idle(&mut hook);
            return was_swallowed;
        }
        let in_front = unsafe { GetForegroundWindow() } == ui::main_window();
        if !h.capturing || !in_front {
            return false;
        }
        // Control-Windows-Escape.
        if info.vkCode == VK_ESCAPE && is_down(0x11) && (is_down(0x5B) || is_down(0x5C)) {
            h.swallowed.insert(id);
            let escape = h.handlers.escape;
            ui::run_on_ui(escape);
            return true;
        }
        let Some(name) = code_for(info) else {
            h.swallowed.insert(id);
            return true;
        };
        let repeat = h.sent.contains_key(&id);
        h.sent.insert(id, name.to_string());
        h.swallowed.insert(id);
        (h.handlers.key)(name, true, repeat);
        true
    });
    if taken { LRESULT(1) } else { pass() }
}
