//! macOS: key events through Quartz Event Services, media and brightness
//! keys as system-defined events, power through loginwindow's Apple events
//! or, for what needs root, Portputer's helper daemon.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGPreflightPostEventAccess, CGRequestPostEventAccess,
};
use serde::{Deserialize, Serialize};

use super::{Held, Injector};
use crate::keys::{self, MacKey};
use crate::protocol::{PowerAction, SessionKind};

/// The helper daemon's socket, from /Library/LaunchDaemons.
pub const HELPER_SOCKET: &str = "/var/run/portputer-helper.sock";
/// Where the helper keeps the server's identity and paired clients for the
/// server at the login window.
pub const SYSTEM_STORE: &str = "/Library/Application Support/Portputer/server";

pub struct MacInjector {
    source: Option<CFRetained<CGEventSource>>,
    held: Held,
    flags: CGEventFlags,
    session: SessionKind,
}

impl MacInjector {
    pub fn new(session: SessionKind) -> MacInjector {
        MacInjector {
            source: CGEventSource::new(CGEventSourceStateID::HIDSystemState),
            held: Held::default(),
            flags: CGEventFlags::empty(),
            session,
        }
    }

    fn modifier_flag(code: &str) -> Option<CGEventFlags> {
        Some(match code {
            "ShiftLeft" | "ShiftRight" => CGEventFlags::MaskShift,
            "ControlLeft" | "ControlRight" => CGEventFlags::MaskControl,
            "AltLeft" | "AltRight" => CGEventFlags::MaskAlternate,
            "MetaLeft" | "MetaRight" => CGEventFlags::MaskCommand,
            "Fn" => CGEventFlags::MaskSecondaryFn,
            _ => return None,
        })
    }

    /// Flags a real keyboard sets on its own for some keys.
    fn implied_flags(code: &str) -> CGEventFlags {
        match code {
            "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => {
                CGEventFlags::MaskNumericPad | CGEventFlags::MaskSecondaryFn
            }
            "Home" | "End" | "PageUp" | "PageDown" | "Delete" | "Insert" | "Help" => CGEventFlags::MaskSecondaryFn,
            c if c.starts_with('F') && c[1..].parse::<u8>().is_ok() => CGEventFlags::MaskSecondaryFn,
            c if c.starts_with("Numpad") || c == "NumLock" => CGEventFlags::MaskNumericPad,
            _ => CGEventFlags::empty(),
        }
    }

    fn post_code(&mut self, code: &str, vk: u16, down: bool) {
        self.post_code_repeat(code, vk, down, false)
    }

    fn post_code_repeat(&mut self, code: &str, vk: u16, down: bool, repeat: bool) {
        if let Some(flag) = Self::modifier_flag(code) {
            // Both sides share one flag: it stays while either is down.
            let other_down = self.held.0.iter().any(|c| c != code && Self::modifier_flag(c) == Some(flag));
            if down {
                self.flags |= flag;
            } else if !other_down {
                self.flags.remove(flag);
            }
        }
        if code == "CapsLock" && down {
            self.flags.toggle(CGEventFlags::MaskAlphaShift);
        }
        let Some(event) = CGEvent::new_keyboard_event(self.source.as_deref(), vk, down) else { return };
        CGEvent::set_flags(Some(&event), self.flags | Self::implied_flags(code));
        if repeat {
            CGEvent::set_integer_value_field(Some(&event), CGEventField::KeyboardEventAutorepeat, 1);
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }

    /// Posts a media, brightness or eject key the way Apple keyboards do.
    fn post_system(&self, key: u8, down: bool) {
        let state: isize = if down { 0xA } else { 0xB };
        let data1 = ((key as isize) << 16) | (state << 8);
        let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::SystemDefined,
            CGPoint { x: 0.0, y: 0.0 },
            NSEventModifierFlags((state as usize) << 8),
            0.0,
            0,
            None,
            8,
            data1,
            -1,
        );
        if let Some(cg) = event.and_then(|e| e.CGEvent()) {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&cg));
        }
    }
}

impl Injector for MacInjector {
    fn key(&mut self, code: &str, down: bool) -> bool {
        let Some(def) = keys::by_code(code) else { return false };
        match def.mac {
            MacKey::Code(vk) => self.post_code(code, vk, down),
            MacKey::System(key) => self.post_system(key, down),
            MacKey::None => return false,
        }
        self.held.note(code, down);
        true
    }

    fn repeat(&mut self, code: &str) -> bool {
        match keys::by_code(code).map(|d| d.mac) {
            Some(MacKey::Code(vk)) => {
                self.post_code_repeat(code, vk, true, true);
                true
            }
            _ => self.key(code, true),
        }
    }

    fn text(&mut self, text: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        // Quartz takes at most 20 UTF-16 units per event.
        for chunk in units.chunks(20) {
            for down in [true, false] {
                let Some(event) = CGEvent::new_keyboard_event(self.source.as_deref(), 0, down) else { continue };
                CGEvent::set_flags(Some(&event), CGEventFlags::empty());
                unsafe { CGEvent::keyboard_set_unicode_string(Some(&event), chunk.len() as _, chunk.as_ptr()) };
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn release_all(&mut self) {
        for code in self.held.take() {
            if let Some(def) = keys::by_code(&code) {
                match def.mac {
                    MacKey::Code(vk) => self.post_code(&code, vk, false),
                    MacKey::System(key) => self.post_system(key, false),
                    MacKey::None => {}
                }
            }
        }
        self.flags.remove(
            CGEventFlags::MaskShift
                | CGEventFlags::MaskControl
                | CGEventFlags::MaskAlternate
                | CGEventFlags::MaskCommand
                | CGEventFlags::MaskSecondaryFn,
        );
        let _ = self.session;
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Posting key events is allowed by the Accessibility permission. Both
/// checks are asked: the post-event preflight can stay false for the life of
/// the process when permission is given after it starts, while posting works.
pub fn input_allowed(ask: bool) -> bool {
    if unsafe { AXIsProcessTrusted() } || CGPreflightPostEventAccess() {
        return true;
    }
    if ask {
        return CGRequestPostEventAccess();
    }
    false
}

/// What each check says, for the log.
pub fn input_permission_detail() -> String {
    format!("accessibility={} post_event={}", unsafe { AXIsProcessTrusted() }, CGPreflightPostEventAccess())
}

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn helper_available() -> bool {
    std::path::Path::new(HELPER_SOCKET).exists()
}

pub fn power_actions(session: SessionKind) -> Vec<PowerAction> {
    use PowerAction::*;
    let privileged = is_root() || helper_available();
    let mut actions = vec![Restart, Shutdown, Sleep];
    if session == SessionKind::User {
        actions.extend([LogOut, Lock]);
    }
    if privileged {
        actions.extend([ForceRestart, UnlockedRestart]);
    }
    actions
}

fn loginwindow_event(code: &str) -> Result<(), String> {
    // kAERestart, kAEShutDown and kAEReallyLogOut: no confirmation dialog;
    // apps are asked to quit and can still refuse.
    let script = format!("tell application \"loginwindow\" to «event aevt{code}»");
    super::run("/usr/bin/osascript", &["-e", &script])
}

pub fn power(session: SessionKind, action: PowerAction, user: Option<&str>, password: Option<&str>) -> Result<(), String> {
    use PowerAction::*;
    match action {
        Sleep => super::run("/usr/bin/pmset", &["sleepnow"]),
        Restart if session == SessionKind::User => loginwindow_event("rest"),
        Shutdown if session == SessionKind::User => loginwindow_event("shut"),
        LogOut if session == SessionKind::User => loginwindow_event("rlgo"),
        Lock => Err("lock is sent as keys".into()),
        LogOut => Err("nobody is logged in".into()),
        _ if is_root() => privileged_power(action, user, password),
        _ => helper(&HelperRequest { action: Some(action), user: user.map(String::from), password: password.map(String::from), ..Default::default() }),
    }
}

/// Power actions that need root, run by the helper or by the server at the
/// login window, which runs as root.
pub fn privileged_power(action: PowerAction, user: Option<&str>, password: Option<&str>) -> Result<(), String> {
    use PowerAction::*;
    match action {
        Restart | ForceRestart => super::run("/sbin/shutdown", &["-r", "now"]),
        Shutdown => super::run("/sbin/shutdown", &["-h", "now"]),
        Sleep => super::run("/usr/bin/pmset", &["sleepnow"]),
        UnlockedRestart => {
            if !filevault_on() {
                return super::run("/sbin/shutdown", &["-r", "now"]);
            }
            let (Some(user), Some(password)) = (user, password) else {
                return Err("FileVault is on: the restart needs a user name and password".into());
            };
            authrestart(user, password)
        }
        LogOut | Lock => Err("not available here".into()),
    }
}

fn filevault_on() -> bool {
    std::process::Command::new("/usr/bin/fdesetup")
        .arg("isactive")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Restarts once past the FileVault unlock screen. The password goes to
/// fdesetup on its standard input, never on a command line.
fn authrestart(user: &str, password: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Username</key><string>{}</string><key>Password</key><string>{}</string></dict></plist>\n",
        xml_escape(user),
        xml_escape(password)
    );
    let mut child = Command::new("/usr/bin/fdesetup")
        .args(["authrestart", "-inputplist"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().unwrap().write_all(plist.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("fdesetup: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

pub fn console_user() -> String {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata("/dev/console") else { return String::new() };
    if meta.uid() == 0 {
        return String::new();
    }
    unsafe {
        let pw = libc::getpwuid(meta.uid());
        if pw.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
    }
}

/// A request to the helper daemon, one JSON line each way.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct HelperRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<PowerAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Copies the server's identity and paired clients for the server at the
    /// login window: the contents of identity.json and paired.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<(String, String)>,
    /// With `sync`: the relay settings (relay.json), or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct HelperReply {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
}

pub fn helper(request: &HelperRequest) -> Result<(), String> {
    let mut stream = UnixStream::connect(HELPER_SOCKET)
        .map_err(|_| "Portputer's system components aren't installed".to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    let mut line = serde_json::to_string(request).map_err(|e| e.to_string())?;
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(|e| e.to_string())?;
    let reply: HelperReply = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    if reply.ok { Ok(()) } else { Err(reply.error) }
}

/// Mutes the computer's speakers while its sound is captured, by setting the
/// mute behaviour of the capture tap: the tap still gets the sound. Taps are
/// private to the process that made them; cpal names its own after the
/// process ID.
pub fn set_speakers_muted(muted: bool) -> Result<(), String> {
    use std::ptr::NonNull;

    use objc2::rc::Retained;
    use objc2_core_audio::{
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
        AudioObjectSetPropertyData, CATapDescription, CATapMuteBehavior, kAudioHardwarePropertyTapList,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
        kAudioTapPropertyDescription,
    };

    let address = |selector| AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let list = address(kAudioHardwarePropertyTapList);
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(kAudioObjectSystemObject as AudioObjectID, NonNull::from(&list), 0, std::ptr::null(), NonNull::from(&mut size))
    };
    if status != 0 {
        return Err(format!("Core Audio error {status} listing taps"));
    }
    let mut taps = vec![0 as AudioObjectID; size as usize / std::mem::size_of::<AudioObjectID>()];
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&list),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(taps.as_mut_ptr()).unwrap().cast(),
        )
    };
    if status != 0 {
        return Err(format!("Core Audio error {status} listing taps"));
    }
    let prefix = format!("cpal output recorder {}.", std::process::id());
    let description = address(kAudioTapPropertyDescription);
    let mut changed = 0;
    for tap in taps {
        let mut object: *mut CATapDescription = std::ptr::null_mut();
        let mut size = std::mem::size_of::<*mut CATapDescription>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(tap, NonNull::from(&description), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut object).cast())
        };
        // The description comes back retained, for the caller to release.
        let Some(desc) = (status == 0).then(|| unsafe { Retained::from_raw(object) }).flatten() else { continue };
        if !unsafe { desc.name() }.to_string().starts_with(&prefix) {
            continue;
        }
        unsafe { desc.setMuteBehavior(if muted { CATapMuteBehavior::Muted } else { CATapMuteBehavior::Unmuted }) };
        let pointer: *const CATapDescription = Retained::as_ptr(&desc);
        let status = unsafe {
            AudioObjectSetPropertyData(
                tap,
                NonNull::from(&description),
                0,
                std::ptr::null(),
                std::mem::size_of::<*const CATapDescription>() as u32,
                NonNull::from(&pointer).cast(),
            )
        };
        if status != 0 {
            return Err(format!("Core Audio error {status} changing the tap"));
        }
        changed += 1;
    }
    if changed == 0 {
        return Err("no audio capture is running".into());
    }
    Ok(())
}
