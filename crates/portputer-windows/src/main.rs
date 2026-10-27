//! Portputer's Windows app: the server for paired devices and the client for
//! controlling other computers, with Windows' standard controls, over the
//! same Rust core as the Mac and Android apps. Cross-compiled on the Mac with
//! MinGW-w64: see windows/build.sh.
//!
//!   Portputer.exe                opens the window
//!   Portputer.exe --background   starts in the notification area (at login)

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod hook;
#[cfg(windows)]
mod speech;
#[cfg(windows)]
mod ui;

#[cfg(not(windows))]
fn main() {
    eprintln!("This is Portputer's Windows app. On a Mac, use the Mac app in macos/.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    app::run();
}
