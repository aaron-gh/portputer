//! macOS: key events through Quartz Event Services, media and brightness
//! keys as system-defined events, power through loginwindow's Apple events
//! or, for what needs root, Portputer's helper daemon.

use std::io::{BufRead, BufReader, Read, Write};
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
        MacInjector::with_source(session, true)
    }

    /// Without an event source, events are made with none.
    pub fn with_source(session: SessionKind, source: bool) -> MacInjector {
        MacInjector {
            source: if source { CGEventSource::new(CGEventSourceStateID::HIDSystemState) } else { None },
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

/// A key command for the key process at the login window, one JSON line
/// each; it answers each with a line once it's done.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum InjectCommand {
    Key { code: String, down: bool },
    Repeat { code: String },
    Tap { code: String, modifiers: Vec<String> },
    Text { text: String },
    ReleaseAll,
}

/// Keys at the login window, through a child process (`portputer-agent
/// inject`). There, making a key event can hang for good inside SkyLight
/// (CGSEventSourceForID waits on a lock it holds itself), which would stop
/// every key after it. A key that isn't done within 1.5 seconds has the
/// process replaced, alternating between making events with an event source
/// and without, and is sent once more.
pub struct ChildInjector {
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    acks: Option<std::sync::mpsc::Receiver<()>>,
    without_source: bool,
}

impl ChildInjector {
    pub fn new() -> ChildInjector {
        ChildInjector { child: None, stdin: None, acks: None, without_source: false }
    }

    fn start(&mut self) -> bool {
        let Ok(exe) = std::env::current_exe() else { return false };
        let mut command = std::process::Command::new(exe);
        command.arg("inject");
        if self.without_source {
            command.arg("--no-source");
        }
        let child = command.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn();
        let Ok(mut child) = child else {
            tracing::warn!("couldn't start the key process");
            return false;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        if let Some(out) = child.stdout.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines() {
                    if line.is_err() || tx.send(()).is_err() {
                        break;
                    }
                }
            });
        }
        self.stdin = child.stdin.take();
        self.acks = Some(rx);
        self.child = Some(child);
        true
    }

    fn stop(&mut self) {
        self.stdin = None;
        self.acks = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn send(&mut self, command: &InjectCommand) -> bool {
        let Ok(mut line) = serde_json::to_string(command) else { return false };
        line.push('\n');
        for _ in 0..2 {
            if self.child.is_none() && !self.start() {
                return false;
            }
            let written = self.stdin.as_mut().is_some_and(|s| s.write_all(line.as_bytes()).and_then(|_| s.flush()).is_ok());
            let done = written && self.acks.as_ref().is_some_and(|a| a.recv_timeout(Duration::from_millis(1500)).is_ok());
            if done {
                return true;
            }
            self.stop();
            self.without_source = !self.without_source;
            tracing::warn!("a key didn't go through; the key process starts again, making events {}", if self.without_source { "without an event source" } else { "with an event source" });
        }
        false
    }
}

impl Default for ChildInjector {
    fn default() -> Self {
        ChildInjector::new()
    }
}

impl Drop for ChildInjector {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Injector for ChildInjector {
    fn key(&mut self, code: &str, down: bool) -> bool {
        self.send(&InjectCommand::Key { code: code.into(), down })
    }

    fn repeat(&mut self, code: &str) -> bool {
        self.send(&InjectCommand::Repeat { code: code.into() })
    }

    fn text(&mut self, text: &str) {
        self.send(&InjectCommand::Text { text: text.into() });
    }

    fn release_all(&mut self) {
        if self.child.is_some() {
            self.send(&InjectCommand::ReleaseAll);
        }
    }

    fn tap(&mut self, code: &str, modifiers: &[String]) -> bool {
        self.send(&InjectCommand::Tap { code: code.into(), modifiers: modifiers.to_vec() })
    }
}

/// The key process: commands on standard input, a line back for each.
pub fn run_injector(without_source: bool) {
    let mut injector = MacInjector::with_source(SessionKind::LoginWindow, !without_source);
    let mut out = std::io::stdout();
    for line in BufReader::new(std::io::stdin()).lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<InjectCommand>(&line) {
            Ok(InjectCommand::Key { code, down }) => {
                injector.key(&code, down);
            }
            Ok(InjectCommand::Repeat { code }) => {
                injector.repeat(&code);
            }
            Ok(InjectCommand::Tap { code, modifiers }) => {
                injector.tap(&code, &modifiers);
            }
            Ok(InjectCommand::Text { text }) => injector.text(&text),
            Ok(InjectCommand::ReleaseAll) => injector.release_all(),
            Err(_) => {}
        }
        if out.write_all(b"ok\n").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    injector.release_all();
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

/// What each check says, for the log. (No event source is made here: at
/// the login window, the server itself leaves SkyLight alone.)
pub fn input_permission_detail() -> String {
    format!("accessibility={} post_event={} uid={}", unsafe { AXIsProcessTrusted() }, CGPreflightPostEventAccess(), unsafe { libc::geteuid() })
}

/// Whether posted keys reach macOS here, for the log at the login window:
/// Shift goes down and up (it types nothing), and the HID system's state is
/// read while it's down. Also whether this process has a window server
/// session, which posting needs. Each step is logged; a step that doesn't
/// return within 3 seconds is sampled to /Library/Logs/Portputer.
pub fn posting_probe() -> String {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU8, Ordering};

    const SHIFT: u16 = 56;
    const STEPS: [&str; 5] = ["window server session", "event source", "Shift down", "HID state", "Shift up"];
    let step = Arc::new(AtomicU8::new(0));
    let (tx, rx) = std::sync::mpsc::channel();
    let at = step.clone();
    std::thread::spawn(move || {
        at.store(0, Ordering::SeqCst);
        let session = objc2_core_graphics::CGSessionCopyCurrentDictionary().is_some();
        at.store(1, Ordering::SeqCst);
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        let post = |down: bool| {
            if let Some(event) = CGEvent::new_keyboard_event(source.as_deref(), SHIFT, down) {
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
            }
        };
        at.store(2, Ordering::SeqCst);
        post(true);
        std::thread::sleep(Duration::from_millis(100));
        at.store(3, Ordering::SeqCst);
        let hid = CGEventSource::key_state(CGEventSourceStateID::HIDSystemState, SHIFT);
        let combined = CGEventSource::key_state(CGEventSourceStateID::CombinedSessionState, SHIFT);
        at.store(4, Ordering::SeqCst);
        post(false);
        let _ = tx.send(format!("window server session={session}; while Shift was posted down: HID state={hid}, session state={combined}"));
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(result) => format!("posting probe: {result}"),
        Err(_) => {
            let stuck = STEPS[step.load(Ordering::SeqCst) as usize];
            let file = "/Library/Logs/Portputer/posting-probe-sample.txt";
            let _ = std::process::Command::new("/usr/bin/sample").args([&std::process::id().to_string(), "2", "-file", file]).output();
            format!("posting probe: stuck at \"{stuck}\" for 3 seconds; sample in {file}")
        }
    }
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
    /// For root only (the server at the login window): after the reply, the
    /// connection carries what's played to Portputer Speakers, as
    /// [`LoopbackChunk`]s.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub audio: bool,
}

/// Audio from Portputer Speakers, from the helper: rate, channels and frame
/// count (u32, native byte order), then the interleaved 32-bit float samples.
pub struct LoopbackChunk {
    pub rate: u32,
    pub channels: u32,
    pub samples: Vec<f32>,
}

impl LoopbackChunk {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.clear();
        out.extend_from_slice(&self.rate.to_ne_bytes());
        out.extend_from_slice(&self.channels.to_ne_bytes());
        out.extend_from_slice(&((self.samples.len() / self.channels.max(1) as usize) as u32).to_ne_bytes());
        for s in &self.samples {
            out.extend_from_slice(&s.to_ne_bytes());
        }
    }

    pub fn read(stream: &mut impl Read) -> std::io::Result<LoopbackChunk> {
        let mut head = [0u8; 12];
        stream.read_exact(&mut head)?;
        let word = |i: usize| u32::from_ne_bytes(head[i * 4..i * 4 + 4].try_into().unwrap());
        let (rate, channels, frames) = (word(0), word(1), word(2));
        if channels == 0 || channels > 8 || frames > 8192 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad audio chunk"));
        }
        let mut bytes = vec![0u8; (frames * channels) as usize * 4];
        stream.read_exact(&mut bytes)?;
        let samples = bytes.chunks_exact(4).map(|b| f32::from_ne_bytes(b.try_into().unwrap())).collect();
        Ok(LoopbackChunk { rate, channels, samples })
    }
}

/// Connects to the helper for what's played to Portputer Speakers.
pub fn helper_audio() -> Result<UnixStream, String> {
    let mut stream = UnixStream::connect(HELPER_SOCKET).map_err(|_| "Portputer's system components aren't installed".to_string())?;
    stream.write_all(b"{\"audio\":true}\n").map_err(|e| e.to_string())?;
    // The reply line, read a byte at a time so nothing after it is taken.
    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    while stream.read_exact(&mut byte).is_ok() && byte[0] != b'\n' {
        reply.push(byte[0]);
    }
    let reply: HelperReply = serde_json::from_slice(&reply).map_err(|e| e.to_string())?;
    if !reply.ok {
        return Err(reply.error);
    }
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    Ok(stream)
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
