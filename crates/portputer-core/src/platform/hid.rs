//! Keys through IOKit's HID system (IOHIDPostEvent), the way a keyboard's
//! keys enter macOS, for the login window. There, Quartz events from
//! Portputer's server either hang inside SkyLight (making an event with an
//! event source) or are dropped (without one). IOHIDPostEvent is deprecated
//! but works for root, and its keys reach loginwindow and VoiceOver like
//! a keyboard's.

use crate::keys::{self, MacKey};
use crate::platform::{Held, Injector};

const NX_KEYDOWN: u32 = 10;
const NX_KEYUP: u32 = 11;
const NX_FLAGSCHANGED: u32 = 12;
const NX_SYSDEFINED: u32 = 14;
const NX_EVENT_DATA_VERSION: u32 = 2;
const SET_GLOBAL_EVENT_FLAGS: u32 = 1;
const HID_PARAM_CONNECT_TYPE: u32 = 1;
/// ANSI.
const KEYBOARD_TYPE: u32 = 40;

const ALPHA_SHIFT: u32 = 0x0001_0000;
const SECONDARY_FN: u32 = 0x0080_0000;
const NON_COALESCED: u32 = 0x0000_0100;

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: i16,
    y: i16,
}

unsafe extern "C" {
    static mach_task_self_: u32;
    fn IOServiceMatching(name: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut std::ffi::c_void) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, kind: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOHIDPostEvent(connect: u32, event_type: u32, location: Point, data: *const u8, version: u32, flags: u32, options: u32) -> i32;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {}

/// A modifier's device-independent flag and its left or right bit.
fn modifier(code: &str) -> Option<(u32, u32)> {
    Some(match code {
        "ShiftLeft" => (0x0002_0000, 0x02),
        "ShiftRight" => (0x0002_0000, 0x04),
        "ControlLeft" => (0x0004_0000, 0x01),
        "ControlRight" => (0x0004_0000, 0x2000),
        "AltLeft" => (0x0008_0000, 0x20),
        "AltRight" => (0x0008_0000, 0x40),
        "MetaLeft" => (0x0010_0000, 0x08),
        "MetaRight" => (0x0010_0000, 0x10),
        "Fn" => (SECONDARY_FN, 0),
        _ => return None,
    })
}

pub struct HidInjector {
    connect: u32,
    /// Modifier keys down, by code.
    modifiers: Vec<String>,
    caps_lock: bool,
    held: Held,
}

impl HidInjector {
    pub fn open() -> Result<HidInjector, String> {
        let name = c"IOHIDSystem";
        let service = unsafe { IOServiceGetMatchingService(0, IOServiceMatching(name.as_ptr())) };
        if service == 0 {
            return Err("no IOHIDSystem".into());
        }
        let mut connect = 0u32;
        let result = unsafe { IOServiceOpen(service, mach_task_self_, HID_PARAM_CONNECT_TYPE, &mut connect) };
        unsafe { IOObjectRelease(service) };
        if result != 0 {
            return Err(format!("IOServiceOpen: {result:#x}"));
        }
        Ok(HidInjector { connect, modifiers: Vec::new(), caps_lock: false, held: Held::default() })
    }

    fn flags(&self) -> u32 {
        let mut flags = NON_COALESCED;
        for code in &self.modifiers {
            if let Some((all, side)) = modifier(code) {
                flags |= all | side;
            }
        }
        if self.caps_lock {
            flags |= ALPHA_SHIFT;
        }
        flags
    }

    fn post_key(&self, event_type: u32, vk: u16, repeat: bool, extra: u32) -> i32 {
        let mut data = [0u8; 128];
        data[2..4].copy_from_slice(&(repeat as i16).to_ne_bytes());
        data[8..10].copy_from_slice(&vk.to_ne_bytes());
        data[16..20].copy_from_slice(&KEYBOARD_TYPE.to_ne_bytes());
        let options = if event_type == NX_FLAGSCHANGED { SET_GLOBAL_EVENT_FLAGS } else { 0 };
        unsafe { IOHIDPostEvent(self.connect, event_type, Point { x: 0, y: 0 }, data.as_ptr(), NX_EVENT_DATA_VERSION, self.flags() | extra, options) }
    }

    /// Media, brightness and eject keys: a system-defined event, subtype 8.
    fn post_system(&self, key: u8, down: bool) -> i32 {
        let state: i32 = if down { 0xA } else { 0xB };
        let mut data = [0u8; 128];
        data[2..4].copy_from_slice(&8i16.to_ne_bytes());
        data[4..8].copy_from_slice(&(((key as i32) << 16) | (state << 8)).to_ne_bytes());
        data[8..12].copy_from_slice(&(-1i32).to_ne_bytes());
        unsafe { IOHIDPostEvent(self.connect, NX_SYSDEFINED, Point { x: 0, y: 0 }, data.as_ptr(), NX_EVENT_DATA_VERSION, 0, 0) }
    }

    fn press(&mut self, code: &str, vk: u16, down: bool, repeat: bool) {
        if modifier(code).is_some() {
            self.modifiers.retain(|c| c != code);
            if down {
                self.modifiers.push(code.to_string());
            }
            self.post_key(NX_FLAGSCHANGED, vk, false, 0);
        } else if code == "CapsLock" {
            if down {
                self.caps_lock = !self.caps_lock;
                self.post_key(NX_FLAGSCHANGED, vk, false, 0);
            }
        } else {
            // Keys a real keyboard marks as function keys.
            let fn_flag = if code.starts_with("Arrow") || matches!(code, "Home" | "End" | "PageUp" | "PageDown" | "Delete" | "Help")
                || (code.starts_with('F') && code[1..].parse::<u8>().is_ok())
            {
                SECONDARY_FN
            } else {
                0
            };
            self.post_key(if down { NX_KEYDOWN } else { NX_KEYUP }, vk, repeat, fn_flag);
        }
    }
}

impl Drop for HidInjector {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.connect) };
    }
}

impl Injector for HidInjector {
    fn key(&mut self, code: &str, down: bool) -> bool {
        let Some(def) = keys::by_code(code) else { return false };
        match def.mac {
            MacKey::Code(vk) => self.press(code, vk, down, false),
            MacKey::System(key) => {
                self.post_system(key, down);
            }
            MacKey::None => return false,
        }
        self.held.note(code, down);
        true
    }

    fn repeat(&mut self, code: &str) -> bool {
        match keys::by_code(code).map(|d| d.mac) {
            Some(MacKey::Code(vk)) => {
                self.press(code, vk, true, true);
                true
            }
            _ => false,
        }
    }

    /// Typing text needs Unicode strings, which HID events don't carry:
    /// characters with a key on a US keyboard are typed as keys.
    fn text(&mut self, text: &str) {
        for c in text.chars() {
            let Some((code, shift)) = keys::us_key_for_char(c) else { continue };
            if shift {
                self.key("ShiftLeft", true);
            }
            self.key(code, true);
            self.key(code, false);
            if shift {
                self.key("ShiftLeft", false);
            }
        }
    }

    fn release_all(&mut self) {
        for code in self.held.take() {
            self.key(&code, false);
        }
        self.modifiers.clear();
    }
}
