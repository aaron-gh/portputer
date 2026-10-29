//! What a server does to the computer it runs on: key events, text, and
//! power. One implementation per system.

use crate::keys;
use crate::protocol::{PowerAction, SessionKind};

#[cfg(target_os = "macos")]
pub mod hid;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

/// Produces key events. Lives on the server's input thread.
pub trait Injector {
    /// False if the system has no such key.
    fn key(&mut self, code: &str, down: bool) -> bool;
    /// A repeat of a held key.
    fn repeat(&mut self, code: &str) -> bool {
        self.key(code, true)
    }
    fn text(&mut self, text: &str);
    /// Releases every key this injector has pressed.
    fn release_all(&mut self);

    fn tap(&mut self, code: &str, modifiers: &[String]) -> bool {
        for m in modifiers {
            self.key(m, true);
        }
        let ok = self.key(code, true);
        if ok {
            self.key(code, false);
        }
        for m in modifiers.iter().rev() {
            self.key(m, false);
        }
        ok
    }
}

/// Keeps track of pressed keys for [`Injector::release_all`].
#[derive(Default)]
pub struct Held(Vec<String>);

impl Held {
    pub fn note(&mut self, code: &str, down: bool) {
        self.0.retain(|c| c != code);
        if down {
            self.0.push(code.to_string());
        }
    }

    pub fn take(&mut self) -> Vec<String> {
        // Ordinary keys first, then modifiers, as a person lets go.
        let mut keys = std::mem::take(&mut self.0);
        keys.sort_by_key(|c| keys::is_modifier(c));
        keys
    }
}

pub fn new_injector(session: SessionKind) -> Box<dyn Injector> {
    // At the login window, through a child process that can be replaced
    // when posting hangs (see ChildInjector).
    #[cfg(target_os = "macos")]
    if session == SessionKind::LoginWindow && macos::is_root() {
        return Box::new(macos::ChildInjector::new());
    }
    #[cfg(target_os = "macos")]
    if session == SessionKind::User {
        return Box::new(macos::UserInjector::new());
    }
    #[cfg(target_os = "macos")]
    return Box::new(macos::MacInjector::new(session));
    #[cfg(windows)]
    return Box::new(windows::WinInjector::new(session));
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = session;
        Box::new(NoInjector)
    }
}

pub struct NoInjector;

impl Injector for NoInjector {
    fn key(&mut self, _code: &str, _down: bool) -> bool {
        false
    }
    fn text(&mut self, _text: &str) {}
    fn release_all(&mut self) {}
}

/// Whether the system lets this process post key events. On a Mac that's
/// the Accessibility permission (and asks for it the first time).
pub fn input_allowed(ask: bool) -> bool {
    #[cfg(target_os = "macos")]
    return macos::input_allowed(ask);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ask;
        true
    }
}

/// The power actions available in this session.
pub fn power_actions(session: SessionKind) -> Vec<PowerAction> {
    #[cfg(target_os = "macos")]
    return macos::power_actions(session);
    #[cfg(windows)]
    return windows::power_actions(session);
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = session;
        vec![]
    }
}

/// Carries out a power action. Lock is handled by the server with keys where
/// that's how the system does it.
pub fn power(session: SessionKind, action: PowerAction, user: Option<&str>, password: Option<&str>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return macos::power(session, action, user, password);
    #[cfg(windows)]
    return windows::power(session, action, user, password);
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (session, action, user, password);
        Err("not supported on this system".into())
    }
}

/// Whether this system can keep the sound from its speakers while streaming it.
pub fn can_mute_speakers() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// Mutes or unmutes the computer's speakers while its sound is captured: on
/// a Mac the capture tap's mute, so the stream keeps the sound; on Windows
/// the output device's mute, put back as it was afterwards.
pub fn set_speakers_muted(muted: bool) -> Result<(), String> {
    if std::env::var_os("PORTPUTER_TEST_TONE").is_some() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if crate::audio::loopback::in_use() {
        return crate::audio::loopback::set_muted(muted);
    }
    #[cfg(target_os = "macos")]
    return macos::set_speakers_muted(muted);
    #[cfg(windows)]
    return windows::set_speakers_muted(muted);
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = muted;
        Err("not supported on this system".into())
    }
}

/// The name of the user logged in at the console, if any.
pub fn console_user() -> String {
    #[cfg(target_os = "macos")]
    return macos::console_user();
    #[cfg(not(target_os = "macos"))]
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default()
}

/// Runs a command, with its error output as the error.
pub fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if text.is_empty() { format!("{program} failed ({})", out.status) } else { text })
    }
}
