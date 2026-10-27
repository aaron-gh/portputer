//! What a server does to the computer it runs on: key events, text, and
//! power. One implementation per system.

use crate::keys;
use crate::protocol::{PowerAction, SessionKind};

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

/// Produces key events. Lives on the server's input thread.
pub trait Injector {
    /// False if the system has no such key.
    fn key(&mut self, code: &str, down: bool) -> bool;
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
