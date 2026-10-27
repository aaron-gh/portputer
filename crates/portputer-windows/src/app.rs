//! The window: the server's settings and pairing at the top; below, the
//! computers on the network, or the connected one's controls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use portputer_core::client::{ClientConfig, ServerInfo, Session, SessionEvent, SessionObserver, SessionState};
use portputer_core::discovery::{BrowseEvent, Browser, ServerAd, computer_name};
use portputer_core::identity::{KnownServer, PairedClient};
use portputer_core::keys::{self, Os};
use portputer_core::protocol::{DEFAULT_PORT, PowerAction, SessionKind};
use portputer_core::server::{Server, ServerConfig, ServerEvent, ServerObserver};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

use crate::{hook, speech, ui};

mod id {
    pub const ACCEPT: u16 = 101;
    pub const STARTUP: u16 = 102;
    pub const PAIR_NEW: u16 = 103;
    pub const CODE: u16 = 104;
    pub const PAIRED: u16 = 105;
    pub const UNPAIR: u16 = 106;
    pub const COMPUTERS: u16 = 201;
    pub const CONNECT: u16 = 202;
    pub const WAKE: u16 = 203;
    pub const FORGET: u16 = 204;
    pub const HOST: u16 = 205;
    pub const PORT: u16 = 206;
    pub const CONNECT_ADDRESS: u16 = 207;
    pub const PAIR_CODE: u16 = 301;
    pub const PAIR: u16 = 302;
    pub const CAPTURE: u16 = 303;
    pub const AUDIO: u16 = 304;
    pub const MUTE_SPEAKERS: u16 = 312;
    pub const MIC: u16 = 313;
    pub const SEND_FILES: u16 = 314;
    pub const RELAY_URL: u16 = 107;
    pub const RELAY_TOKEN: u16 = 108;
    pub const RELAY_SAVE: u16 = 109;
    pub const VOLUME: u16 = 305;
    pub const KEYS: u16 = 306;
    pub const TEXT: u16 = 307;
    pub const TYPE: u16 = 308;
    pub const POWER: u16 = 309;
    pub const DISCONNECT: u16 = 310;
    pub const RELEASE_ALL: u16 = 311;
    pub const QUIT: u16 = 401;
    pub const TRAY_OPEN: u16 = 501;
    pub const TRAY_PAIR: u16 = 502;
    pub const TRAY_QUIT: u16 = 503;
    pub const MENU_KEY: u16 = 1000;
    pub const MENU_POWER: u16 = 3000;
}

struct Controls {
    server_heading: HWND,
    accept: HWND,
    startup: HWND,
    server_status: HWND,
    pair_new: HWND,
    code_label: HWND,
    code: HWND,
    paired_label: HWND,
    paired: HWND,
    unpair: HWND,
    relay_url_label: HWND,
    relay_url: HWND,
    relay_token_label: HWND,
    relay_token: HWND,
    relay_save: HWND,
    relay_status: HWND,
    computers_label: HWND,
    computers: HWND,
    connect: HWND,
    wake: HWND,
    forget: HWND,
    host_label: HWND,
    host: HWND,
    port_label: HWND,
    port: HWND,
    connect_address: HWND,
    status: HWND,
    pair_label: HWND,
    pair_code: HWND,
    pair: HWND,
    capture: HWND,
    audio: HWND,
    mute_speakers: HWND,
    mic: HWND,
    volume_label: HWND,
    volume: HWND,
    keys: HWND,
    power: HWND,
    release_all: HWND,
    send_files: HWND,
    text_label: HWND,
    text: HWND,
    type_text: HWND,
    disconnect: HWND,
    quit: HWND,
}

#[derive(Clone, PartialEq)]
enum Conn {
    Idle,
    Connecting(String),
    Connected(ServerInfo),
    Reconnecting(ServerInfo),
}

#[derive(Clone)]
struct Computer {
    id: String,
    name: String,
    os: Os,
    known: Option<KnownServer>,
    ads: Vec<ServerAd>,
}

impl Computer {
    fn ad(&self) -> Option<&ServerAd> {
        self.ads.iter().find(|a| a.session == SessionKind::User).or_else(|| self.ads.first())
    }

    fn macs(&self) -> Vec<String> {
        let mut m: Vec<String> = self.ads.iter().flat_map(|a| a.macs.clone()).collect();
        if let Some(k) = &self.known {
            m.extend(k.macs.clone());
        }
        m.sort();
        m.dedup();
        m
    }

    fn line(&self) -> String {
        let os = match self.os {
            Os::Mac => "Mac",
            Os::Windows => "Windows",
            Os::Other => "Computer",
        };
        let state = match self.ad() {
            Some(a) if a.session == SessionKind::LoginWindow => "at the login window".to_string(),
            Some(a) if !a.user.is_empty() => format!("{} logged in", a.user),
            Some(_) => "available".to_string(),
            None if self.known.as_ref().is_some_and(|k| k.relay.is_some()) => "through the internet relay".to_string(),
            None => "not on the network".to_string(),
        };
        let paired = if self.known.is_some() { "" } else { ", not paired" };
        format!("{}, {os}, {state}{paired}", self.name)
    }
}

struct App {
    hwnd: HWND,
    c: Controls,
    server: Option<Server>,
    browser: Option<Browser>,
    ads: HashMap<String, ServerAd>,
    computers: Vec<Computer>,
    paired: Vec<PairedClient>,
    session: Option<Session>,
    conn: Conn,
    generation: u64,
    capturing: bool,
    audio_wanted: bool,
    menu_keys: Vec<keys::MenuKey>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn data_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Portputer")
}

fn server_dir() -> PathBuf {
    data_dir().join("server")
}

fn client_dir() -> PathBuf {
    data_dir().join("client")
}

fn setting(name: &str) -> Option<String> {
    std::fs::read_to_string(data_dir().join(format!("{name}.setting"))).ok().map(|s| s.trim().to_string())
}

fn set_setting(name: &str, value: &str) {
    let _ = std::fs::create_dir_all(data_dir());
    let _ = std::fs::write(data_dir().join(format!("{name}.setting")), value);
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn starts_at_login() -> bool {
    std::process::Command::new("reg.exe")
        .args(["query", RUN_KEY, "/v", "Portputer"])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn set_start_at_login(on: bool) {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let value = format!("\"{exe}\" --background");
    let mut command = std::process::Command::new("reg.exe");
    if on {
        command.args(["add", RUN_KEY, "/v", "Portputer", "/t", "REG_SZ", "/d", &value, "/f"]);
    } else {
        command.args(["delete", RUN_KEY, "/v", "Portputer", "/f"]);
    }
    // No console window.
    let _ = command.creation_flags(0x0800_0000).status();
}

struct ServerEvents;

impl ServerObserver for ServerEvents {
    fn event(&self, event: ServerEvent) {
        ui::run_on_ui(move || {
            with_app(|a| a.server_event(event));
        });
    }
}

struct SessionEvents(u64);

impl SessionObserver for SessionEvents {
    fn event(&self, event: SessionEvent) {
        let generation = self.0;
        ui::run_on_ui(move || {
            with_app(|a| {
                if a.generation == generation {
                    a.session_event(event)
                }
            });
        });
    }
}

impl App {
    fn create_controls(hwnd: HWND) -> Controls {
        let l = |t: &str| ui::label(hwnd, t);
        let c = Controls {
            server_heading: l("This computer"),
            accept: ui::checkbox(hwnd, "Accept connections from paired devices", id::ACCEPT),
            startup: ui::checkbox(hwnd, "Start at login", id::STARTUP),
            server_status: l(""),
            pair_new: ui::button(hwnd, "Pair New Device", id::PAIR_NEW),
            code_label: l("Pairing code"),
            code: ui::edit(hwnd, id::CODE, true),
            paired_label: l("Paired devices"),
            paired: ui::listbox(hwnd, id::PAIRED),
            unpair: ui::button(hwnd, "Remove Device", id::UNPAIR),
            relay_url_label: l("Internet relay address"),
            relay_url: ui::edit(hwnd, id::RELAY_URL, false),
            relay_token_label: l("Relay token"),
            relay_token: ui::edit(hwnd, id::RELAY_TOKEN, false),
            relay_save: ui::button(hwnd, "Save Relay Settings", id::RELAY_SAVE),
            relay_status: l("Internet relay off."),
            computers_label: l("Computers"),
            computers: ui::listbox(hwnd, id::COMPUTERS),
            connect: ui::button(hwnd, "Connect", id::CONNECT),
            wake: ui::button(hwnd, "Wake", id::WAKE),
            forget: ui::button(hwnd, "Forget", id::FORGET),
            host_label: l("Host name or IP address"),
            host: ui::edit(hwnd, id::HOST, false),
            port_label: l("Port"),
            port: ui::edit(hwnd, id::PORT, false),
            connect_address: ui::button(hwnd, "Connect by Address", id::CONNECT_ADDRESS),
            status: l(""),
            pair_label: l("Pairing code from the computer"),
            pair_code: ui::edit(hwnd, id::PAIR_CODE, false),
            pair: ui::button(hwnd, "Pair", id::PAIR),
            capture: ui::checkbox(hwnd, "Send keyboard to computer (Control-Windows-Escape returns it)", id::CAPTURE),
            audio: ui::checkbox(hwnd, "Computer audio", id::AUDIO),
            mute_speakers: ui::checkbox(hwnd, "Mute computer speakers", id::MUTE_SPEAKERS),
            mic: ui::checkbox(hwnd, "Send microphone", id::MIC),
            volume_label: l("Audio volume"),
            volume: ui::trackbar(hwnd, id::VOLUME),
            keys: ui::button(hwnd, "Keys and Shortcuts", id::KEYS),
            power: ui::button(hwnd, "Power", id::POWER),
            release_all: ui::button(hwnd, "Release All Keys", id::RELEASE_ALL),
            send_files: ui::button(hwnd, "Send Files", id::SEND_FILES),
            text_label: l("Text to type"),
            text: ui::edit(hwnd, id::TEXT, false),
            type_text: ui::button(hwnd, "Type", id::TYPE),
            disconnect: ui::button(hwnd, "Disconnect", id::DISCONNECT),
            quit: ui::button(hwnd, "Quit Portputer", id::QUIT),
        };
        ui::set_text(c.port, &DEFAULT_PORT.to_string());
        if let Some(relay) = portputer_core::identity::ServerStore::new(server_dir()).relay() {
            ui::set_text(c.relay_url, &relay.url);
            ui::set_text(c.relay_token, &relay.token);
        }
        speech::make_live_region(c.server_status);
        speech::make_live_region(c.status);
        c
    }

    fn layout(&self) {
        let mut rect = Default::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        let c = &self.c;
        let mut f = ui::Flow { x: 12, y: 12, width: rect.right - 24 };
        let h = 24;
        f.row(&[(c.server_heading, 1)], h);
        f.row(&[(c.accept, 2), (c.startup, 1)], h);
        f.row(&[(c.server_status, 1)], h);
        f.row(&[(c.pair_new, 1), (c.code_label, 1), (c.code, 2)], h + 2);
        f.row(&[(c.paired_label, 1)], h);
        f.row(&[(c.paired, 3), (c.unpair, 1)], 64);
        f.row(&[(c.relay_url_label, 2), (c.relay_token_label, 1)], h);
        f.row(&[(c.relay_url, 2), (c.relay_token, 1)], h + 2);
        f.row(&[(c.relay_save, 1), (c.relay_status, 2)], h + 4);
        f.gap(8);
        f.row(&[(c.status, 1)], h);
        f.row(&[(c.computers_label, 1)], h);
        f.row(&[(c.computers, 1)], 150);
        f.row(&[(c.connect, 1), (c.wake, 1), (c.forget, 1)], h + 4);
        f.row(&[(c.host_label, 2), (c.port_label, 1)], h);
        f.row(&[(c.host, 2), (c.port, 1)], h + 2);
        f.row(&[(c.connect_address, 1)], h + 4);
        f.row(&[(c.pair_label, 1)], h);
        f.row(&[(c.pair_code, 3), (c.pair, 1)], h + 2);
        f.row(&[(c.capture, 1)], h);
        f.row(&[(c.audio, 1), (c.mute_speakers, 1), (c.mic, 1)], h);
        f.row(&[(c.volume_label, 1), (c.volume, 3)], 30);
        f.row(&[(c.keys, 1), (c.power, 1), (c.release_all, 1), (c.send_files, 1)], h + 4);
        f.row(&[(c.text_label, 1), (c.text, 3), (c.type_text, 1)], h + 2);
        f.row(&[(c.disconnect, 1)], h + 4);
        f.gap(8);
        f.row(&[(c.quit, 1)], h + 4);
    }

    /// Shows the controls that apply now.
    fn refresh(&mut self) {
        let c = &self.c;
        let running = self.server.is_some();
        ui::set_checked(c.accept, running);
        ui::enable(c.pair_new, running);
        let pairing = self.server.as_ref().is_some_and(|s| s.pairing_seconds_left() > 0);
        ui::show(c.code_label, pairing);
        ui::show(c.code, pairing);
        self.paired = self.server.as_ref().map(|s| s.paired()).unwrap_or_default();
        let connected: Vec<String> = self.server.as_ref().map(|s| s.connected().into_iter().map(|c| c.id).collect()).unwrap_or_default();
        let lines: Vec<String> = self
            .paired
            .iter()
            .map(|p| if connected.contains(&p.id) { format!("{}, connected", p.name) } else { p.name.clone() })
            .collect();
        ui::list_set(c.paired, &lines);
        ui::enable(c.unpair, !self.paired.is_empty());

        let idle = matches!(self.conn, Conn::Idle);
        for h in [c.computers_label, c.computers, c.connect, c.wake, c.forget, c.host_label, c.host, c.port_label, c.port, c.connect_address] {
            ui::show(h, idle);
        }
        let info = match &self.conn {
            Conn::Connected(i) | Conn::Reconnecting(i) => Some(i.clone()),
            _ => None,
        };
        let trusted = matches!(&self.conn, Conn::Connected(i) if i.trusted);
        let untrusted = matches!(&self.conn, Conn::Connected(i) if !i.trusted);
        for h in [c.pair_label, c.pair_code, c.pair] {
            ui::show(h, untrusted);
        }
        for h in [c.capture, c.audio, c.mute_speakers, c.mic, c.volume_label, c.volume, c.keys, c.power, c.release_all, c.send_files, c.text_label, c.text, c.type_text] {
            ui::show(h, info.as_ref().is_some_and(|i| i.trusted));
            ui::enable(h, trusted);
        }
        ui::show(c.disconnect, !idle);
        if let Some(i) = &info {
            ui::enable(c.audio, trusted && i.audio);
            ui::enable(c.volume, trusted && i.audio);
            ui::show(c.mute_speakers, i.trusted && i.audio && i.mute);
            ui::show(c.mic, i.trusted && i.mic);
            ui::show(c.send_files, i.trusted && i.files);
            ui::enable(c.power, trusted && !i.power.is_empty());
        }
        ui::set_checked(c.capture, self.capturing);
        ui::set_checked(c.audio, self.audio_wanted);
        ui::set_checked(c.mute_speakers, setting("mute_speakers").as_deref() == Some("1"));
        if idle {
            self.refresh_computers();
        }
        self.layout();
    }

    fn refresh_computers(&mut self) {
        let known: Vec<KnownServer> = portputer_core::client::known_servers(&client_dir());
        let own = self.server.as_ref().map(|s| s.id()).unwrap_or_default();
        let mut ids: Vec<String> = known.iter().map(|k| k.id.clone()).collect();
        ids.extend(self.ads.values().map(|a| a.id.clone()));
        ids.sort();
        ids.dedup();
        ids.retain(|i| *i != own);
        self.computers = ids
            .into_iter()
            .map(|id| {
                let ads: Vec<ServerAd> = self.ads.values().filter(|a| a.id == id).cloned().collect();
                let k = known.iter().find(|k| k.id == id).cloned();
                let name = ads.first().map(|a| a.name.clone()).or_else(|| k.as_ref().map(|k| k.name.clone())).unwrap_or_else(|| id.clone());
                let os = ads.first().map(|a| a.os).unwrap_or(match k.as_ref().map(|k| k.os.as_str()) {
                    Some("mac") => Os::Mac,
                    Some("windows") => Os::Windows,
                    _ => Os::Other,
                });
                Computer { id, name, os, known: k, ads }
            })
            .collect();
        self.computers.sort_by_key(|c| (c.known.is_none(), c.name.to_lowercase()));
        let lines: Vec<String> = self.computers.iter().map(Computer::line).collect();
        ui::list_set(self.c.computers, &if lines.is_empty() { vec!["No computers found.".to_string()] } else { lines });
    }

    fn say(&self, text: &str) {
        speech::status(self.c.status, text);
    }

    fn start_server(&mut self) {
        if self.server.is_some() {
            return;
        }
        match Server::start(ServerConfig {
            store_dir: server_dir(),
            session: SessionKind::User,
            port: DEFAULT_PORT,
            name: computer_name(),
            observer: Some(Arc::new(ServerEvents)),
        }) {
            Ok(s) => {
                ui::set_text(self.c.server_status, &format!("Server on, port {}.", s.port()));
                self.server = Some(s);
            }
            Err(e) => speech::status(self.c.server_status, &format!("Server not started: {e}")),
        }
        set_setting("accept", "1");
    }

    fn stop_server(&mut self) {
        self.server = None;
        set_setting("accept", "0");
        speech::status(self.c.server_status, "Server off.");
    }

    fn server_event(&mut self, event: ServerEvent) {
        match event {
            ServerEvent::Paired { name, .. } => {
                speech::status(self.c.server_status, &format!("Paired with {name}."));
                ui::set_text(self.c.code, "");
            }
            ServerEvent::PairingFailed { name } => speech::status(self.c.server_status, &format!("Pairing with {name} failed: wrong code.")),
            ServerEvent::Connected { name, .. } => speech::status(self.c.server_status, &format!("{name} connected.")),
            ServerEvent::Disconnected { name, .. } => speech::status(self.c.server_status, &format!("{name} disconnected.")),
            ServerEvent::AudioFailed { message } | ServerEvent::Error { message } => speech::status(self.c.server_status, &message),
            ServerEvent::Relay { listening, message } => {
                let text = if listening { "Listening at the internet relay.".to_string() } else { format!("Internet relay: {message}") };
                ui::set_text(self.c.relay_status, &text);
            }
            ServerEvent::Listening { .. } | ServerEvent::InputNotAllowed => {}
        }
        self.refresh();
    }

    fn new_pairing_code(&mut self) {
        let Some(server) = &self.server else { return };
        let code = server.new_pairing_code();
        ui::set_text(self.c.code, &code);
        self.refresh();
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.c.code));
        }
        let spelled: Vec<String> = code.split('-').map(|g| g.chars().map(String::from).collect::<Vec<_>>().join(" ")).collect();
        speech::speak(&format!("Pairing code {}. Enter it in Portputer on the device within 10 minutes.", spelled.join(", ")));
    }

    fn connect_to(&mut self, host: String, port: u16, id: Option<String>, name: String) {
        self.disconnect();
        self.generation += 1;
        let generation = self.generation;
        self.conn = Conn::Connecting(name.clone());
        self.say(&format!("Connecting to {name}."));
        self.refresh();
        let config = ClientConfig { store_dir: client_dir(), name: computer_name(), observer: Arc::new(SessionEvents(generation)), local_clipboard: true };
        std::thread::spawn(move || {
            let result = Session::connect(&host, port, id, config);
            ui::run_on_ui(move || {
                with_app(|a| {
                    if a.generation != generation {
                        if let Ok(s) = &result {
                            s.close();
                        }
                        return;
                    }
                    match result {
                        Ok(s) => {
                            let volume = setting("volume").and_then(|v| v.parse::<u32>().ok()).unwrap_or(100);
                            s.set_volume(volume as f32 / 100.0);
                            s.set_speakers_muted(setting("mute_speakers").as_deref() == Some("1"));
                            ui::set_trackbar_pos(a.c.volume, volume);
                            let info = s.info();
                            a.session = Some(s);
                            if let Some(info) = info {
                                a.connected(info);
                            }
                        }
                        Err(e) => {
                            a.conn = Conn::Idle;
                            a.say(&format!("Couldn't connect to {name}. {e}"));
                            a.refresh();
                        }
                    }
                });
            });
        });
    }

    fn connected(&mut self, info: ServerInfo) {
        let was_trusted = matches!(&self.conn, Conn::Connected(i) | Conn::Reconnecting(i) if i.trusted);
        self.say(&if info.session == SessionKind::LoginWindow {
            format!("Connected to {} at the login window.", info.name)
        } else if info.user.is_empty() {
            format!("Connected to {}.", info.name)
        } else {
            format!("Connected to {}, {} logged in.", info.name, info.user)
        });
        if info.via_relay {
            self.say("Through the internet relay.");
        }
        if !info.trusted {
            self.say(&format!("Connected to {}. On it, open Portputer and choose Pair New Device, then enter the code here.", info.name));
        }
        let start_audio = info.trusted && !was_trusted && info.audio;
        self.conn = Conn::Connected(info);
        if start_audio {
            self.set_audio(true);
        }
        self.refresh();
    }

    fn session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::State(SessionState::Connected(info)) => self.connected(info),
            SessionEvent::State(SessionState::Reconnecting) => {
                if let Conn::Connected(i) = &self.conn {
                    let i = i.clone();
                    self.say(&format!("Connection to {} lost. Reconnecting.", i.name));
                    self.conn = Conn::Reconnecting(i);
                }
                self.refresh();
            }
            SessionEvent::State(SessionState::Closed { reason }) => {
                self.set_capturing(false);
                self.session = None;
                self.conn = Conn::Idle;
                self.audio_wanted = false;
                self.say(&format!("Disconnected. {reason}"));
                self.refresh();
            }
            SessionEvent::Notice { message, .. } => self.say(&message),
            SessionEvent::Error { code, message } => {
                if code == "audio_failed" {
                    self.audio_wanted = false;
                    self.refresh();
                }
                if code == "mic_failed" {
                    ui::set_checked(self.c.mic, false);
                }
                self.say(&message);
            }
            SessionEvent::AudioStarted { .. } => self.say("Audio on."),
            SessionEvent::AudioStopped { .. } => {}
            SessionEvent::MicStarted => self.say("Microphone on."),
            SessionEvent::ClipboardSynced { message } => self.say(&message),
            // This computer's clipboard is kept in step by the core.
            SessionEvent::ClipboardText { .. } | SessionEvent::ClipboardFiles { .. } => {}
            SessionEvent::MicStopped { .. } => ui::set_checked(self.c.mic, false),
        }
    }

    fn disconnect(&mut self) {
        self.generation += 1;
        self.set_capturing(false);
        if let Some(s) = self.session.take() {
            s.close();
        }
        self.audio_wanted = false;
        if self.conn != Conn::Idle {
            self.conn = Conn::Idle;
            self.say("Disconnected.");
        }
        self.refresh();
    }

    fn set_capturing(&mut self, on: bool) {
        if on == self.capturing {
            return;
        }
        if on {
            let ok = hook::start(hook::Handlers {
                key: |code, down, repeat| {
                    with_app(|a| {
                        if let Some(s) = &a.session {
                            s.key(code, down, repeat);
                        }
                    });
                },
                escape: || {
                    with_app(|a| {
                        a.set_capturing(false);
                        a.say("Keyboard to Windows.");
                    });
                },
            });
            if !ok {
                self.say("Windows didn't allow the keyboard hook.");
                ui::set_checked(self.c.capture, false);
                return;
            }
            self.capturing = true;
            let name = match &self.conn {
                Conn::Connected(i) => i.name.clone(),
                _ => String::new(),
            };
            self.say(&format!("Keyboard to {name}. Control-Windows-Escape returns it."));
        } else {
            self.capturing = false;
            hook::stop();
            if let Some(s) = &self.session {
                s.release_all();
            }
        }
        ui::set_checked(self.c.capture, self.capturing);
    }

    fn set_audio(&mut self, on: bool) {
        self.audio_wanted = on;
        if let Some(s) = &self.session {
            if on { s.start_audio() } else { s.stop_audio() }
        }
        ui::set_checked(self.c.audio, on);
    }

    fn info(&self) -> Option<ServerInfo> {
        match &self.conn {
            Conn::Connected(i) => Some(i.clone()),
            _ => None,
        }
    }

    fn keys_menu(&mut self) {
        let Some(info) = self.info() else { return };
        self.menu_keys.clear();
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for section in keys::menu(info.os) {
                let Ok(sub) = CreatePopupMenu() else { continue };
                for key in section.keys {
                    let w = ui::wide(&key.label);
                    let _ = AppendMenuW(sub, MF_STRING, (id::MENU_KEY as usize) + self.menu_keys.len(), PCWSTR(w.as_ptr()));
                    self.menu_keys.push(key);
                }
                let title = ui::wide(&section.title);
                let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, PCWSTR(title.as_ptr()));
            }
            let chosen = self.popup(menu, self.c.keys);
            let _ = DestroyMenu(menu);
            if chosen >= id::MENU_KEY as u32 {
                if let (Some(key), Some(s)) = (self.menu_keys.get((chosen - id::MENU_KEY as u32) as usize), &self.session) {
                    s.tap(&key.code, key.modifiers.clone());
                    self.say(&format!("{} pressed.", key.label));
                }
            }
        }
    }

    fn power_menu(&mut self) {
        let Some(info) = self.info() else { return };
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (i, action) in info.power.iter().enumerate() {
                let w = ui::wide(power_label(*action, info.os));
                let _ = AppendMenuW(menu, MF_STRING, id::MENU_POWER as usize + i, PCWSTR(w.as_ptr()));
            }
            let chosen = self.popup(menu, self.c.power);
            let _ = DestroyMenu(menu);
            if chosen < id::MENU_POWER as u32 {
                return;
            }
            let Some(action) = info.power.get((chosen - id::MENU_POWER as u32) as usize).copied() else { return };
            let label = power_label(action, info.os);
            if action == PowerAction::UnlockedRestart {
                self.say("Restart Without FileVault Unlock needs a password: use the Mac or Android app for now.");
                return;
            }
            if action != PowerAction::Lock && !ui::confirm(self.hwnd, "Portputer", &format!("{label} {}?", info.name)) {
                return;
            }
            if let Some(s) = &self.session {
                s.power(action, None, None);
                self.say(&format!("{label} sent to {}.", info.name));
            }
        }
    }

    /// Shows a popup menu under a button and returns the chosen ID, or 0.
    fn popup(&self, menu: HMENU, under: HWND) -> u32 {
        unsafe {
            let mut rect = Default::default();
            let _ = GetWindowRect(under, &mut rect);
            let _ = SetForegroundWindow(self.hwnd);
            TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, rect.left, rect.bottom, None, self.hwnd, None).0 as u32
        }
    }

    fn command(&mut self, control: u16, notification: u16) {
        match control {
            id::ACCEPT => {
                if ui::checked(self.c.accept) { self.start_server() } else { self.stop_server() }
                self.refresh();
            }
            id::STARTUP => set_start_at_login(ui::checked(self.c.startup)),
            id::RELAY_SAVE => {
                let url = ui::text(self.c.relay_url).trim().to_string();
                let token = ui::text(self.c.relay_token);
                let relay = portputer_core::relay::RelayConfig { url, token };
                let relay = relay.is_set().then_some(relay);
                let result = match &self.server {
                    Some(s) => s.set_relay(relay),
                    None => portputer_core::identity::ServerStore::new(server_dir()).set_relay(relay.as_ref()),
                };
                match result {
                    Ok(()) => speech::status(self.c.relay_status, "Relay settings saved."),
                    Err(e) => speech::status(self.c.relay_status, &e.to_string()),
                }
            }
            id::PAIR_NEW => self.new_pairing_code(),
            id::UNPAIR => {
                if let Some(p) = ui::list_selection(self.c.paired).and_then(|i| self.paired.get(i)).cloned() {
                    if let Some(s) = &self.server {
                        let _ = s.unpair(&p.id);
                    }
                    speech::status(self.c.server_status, &format!("{} removed.", p.name));
                    self.refresh();
                }
            }
            id::COMPUTERS if notification as u32 == LBN_DBLCLK => self.connect_selected(),
            id::CONNECT => self.connect_selected(),
            id::WAKE => {
                if let Some(c) = self.selected_computer() {
                    let sent = portputer_core::wol::wake(&c.macs());
                    self.say(&if sent { format!("Wake on LAN packet sent to {}.", c.name) } else { "Wake on LAN packet not sent.".into() });
                }
            }
            id::FORGET => {
                if let Some(c) = self.selected_computer() {
                    if ui::confirm(self.hwnd, "Portputer", &format!("Forget {}? Pairing again needs a new code from it.", c.name)) {
                        let _ = portputer_core::client::forget_server(&client_dir(), &c.id);
                        self.say(&format!("{} forgotten.", c.name));
                        self.refresh();
                    }
                }
            }
            id::CONNECT_ADDRESS => {
                let host = ui::text(self.c.host).trim().to_string();
                let port = ui::text(self.c.port).trim().parse().unwrap_or(DEFAULT_PORT);
                if !host.is_empty() {
                    self.connect_to(host.clone(), port, None, host);
                }
            }
            id::PAIR => self.pair(),
            id::CAPTURE => self.set_capturing(ui::checked(self.c.capture)),
            id::AUDIO => self.set_audio(ui::checked(self.c.audio)),
            id::MIC => {
                if let Some(s) = &self.session {
                    if ui::checked(self.c.mic) { s.start_mic() } else { s.stop_mic() }
                }
            }
            id::SEND_FILES => self.send_files(),
            id::MUTE_SPEAKERS => {
                let muted = ui::checked(self.c.mute_speakers);
                set_setting("mute_speakers", if muted { "1" } else { "0" });
                if let Some(s) = &self.session {
                    s.set_speakers_muted(muted);
                }
            }
            id::KEYS => self.keys_menu(),
            id::POWER => self.power_menu(),
            id::RELEASE_ALL => {
                if let Some(s) = &self.session {
                    s.release_all();
                    self.say("All keys released.");
                }
            }
            id::TYPE => {
                let text = ui::text(self.c.text);
                if let (false, Some(s)) = (text.is_empty(), &self.session) {
                    s.text(&text);
                    ui::set_text(self.c.text, "");
                    self.say("Text typed.");
                }
            }
            id::DISCONNECT => self.disconnect(),
            id::QUIT | id::TRAY_QUIT => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            id::TRAY_OPEN => show_window(self.hwnd),
            id::TRAY_PAIR => {
                show_window(self.hwnd);
                self.new_pairing_code();
            }
            _ => {}
        }
    }

    /// Uploads files chosen in the Open dialog to the computer's Downloads folder.
    fn send_files(&mut self) {
        let Some(session) = self.session.clone() else { return };
        let files = ui::open_files(self.hwnd, "Send Files");
        if files.is_empty() {
            return;
        }
        let n = files.len();
        self.say(&format!("Sending {}.", portputer_core::client::count_files(n)));
        std::thread::spawn(move || {
            let mut failure = None;
            for f in &files {
                if let Err(e) = session.upload(f, None, None, |_, _| {}) {
                    failure = Some(e.to_string());
                }
            }
            let text = match failure {
                Some(e) => format!("Sending failed: {e}"),
                None => format!("Sent {} to Downloads.", portputer_core::client::count_files(n)),
            };
            ui::run_on_ui(move || {
                with_app(|a| a.say(&text));
            });
        });
    }

    fn selected_computer(&self) -> Option<Computer> {
        ui::list_selection(self.c.computers).and_then(|i| self.computers.get(i)).cloned()
    }

    fn connect_selected(&mut self) {
        let Some(c) = self.selected_computer() else { return };
        if let Some(ad) = c.ad() {
            if let Some(host) = ad.addresses.first() {
                return self.connect_to(host.clone(), ad.port, Some(c.id.clone()), c.name.clone());
            }
        }
        // The last address, then the relay, which the core tries after it.
        if let Some(k) = c.known.as_ref().filter(|k| !k.host.is_empty() || k.relay.is_some()) {
            return self.connect_to(k.host.clone(), k.port, Some(c.id.clone()), c.name.clone());
        }
        self.say(&format!("{} is not on the network.", c.name));
    }

    fn pair(&mut self) {
        let code = ui::text(self.c.pair_code);
        let Some(s) = self.session.clone() else { return };
        if code.trim().is_empty() {
            return;
        }
        self.say("Pairing.");
        std::thread::spawn(move || {
            let result = s.pair(&code);
            ui::run_on_ui(move || {
                with_app(|a| match result {
                    Ok(()) => {
                        ui::set_text(a.c.pair_code, "");
                        if let Some(info) = a.session.as_ref().and_then(|s| s.info()) {
                            a.say(&format!("Paired with {}.", info.name));
                            a.connected(info);
                        }
                    }
                    Err(e) => a.say(&format!("Pairing failed. {e}")),
                });
            });
        });
    }
}

fn power_label(action: PowerAction, os: Os) -> &'static str {
    match action {
        PowerAction::Restart => "Restart",
        PowerAction::Shutdown => "Shut Down",
        PowerAction::Sleep => "Sleep",
        PowerAction::LogOut => "Log Out",
        PowerAction::Lock => "Lock Screen",
        PowerAction::ForceRestart => "Force Restart",
        PowerAction::UnlockedRestart if os == Os::Mac => "Restart Without FileVault Unlock",
        PowerAction::UnlockedRestart => "Restart Without Disk Unlock",
    }
}

fn show_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn tray(hwnd: HWND, add: bool) {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: ui::WM_TRAY,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() },
        ..Default::default()
    };
    let tip = ui::wide("Portputer");
    data.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe {
        let _ = Shell_NotifyIconW(if add { NIM_ADD } else { NIM_DELETE }, &data);
    }
}

fn tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (id, label) in [(id::TRAY_OPEN, "Open Portputer"), (id::TRAY_PAIR, "Pair New Device"), (id::TRAY_QUIT, "Quit Portputer")] {
            let w = ui::wide(label);
            let _ = AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(w.as_ptr()));
        }
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, None, hwnd, None).0 as u16;
        let _ = DestroyMenu(menu);
        if chosen != 0 {
            with_app(|a| a.command(chosen, 0));
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handled = std::panic::catch_unwind(|| match msg {
        ui::WM_RUN => {
            ui::run_queued();
            Some(LRESULT(0))
        }
        WM_COMMAND => {
            let control = (wparam.0 & 0xFFFF) as u16;
            let notification = ((wparam.0 >> 16) & 0xFFFF) as u16;
            with_app(|a| a.command(control, notification));
            Some(LRESULT(0))
        }
        WM_HSCROLL => {
            with_app(|a| {
                if HWND(lparam.0 as *mut _) == a.c.volume {
                    let pos = ui::trackbar_pos(a.c.volume);
                    set_setting("volume", &pos.to_string());
                    if let Some(s) = &a.session {
                        s.set_volume(pos as f32 / 100.0);
                    }
                }
            });
            Some(LRESULT(0))
        }
        WM_SIZE => {
            with_app(|a| a.layout());
            Some(LRESULT(0))
        }
        ui::WM_TRAY => {
            match (lparam.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | 0x400 | 0x401 => show_window(hwnd),
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
                _ => {}
            }
            Some(LRESULT(0))
        }
        WM_CLOSE => {
            // The server keeps running; the notification area icon brings the window back.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            with_app(|a| {
                a.set_capturing(false);
                a.disconnect();
                a.server = None;
            });
            tray(hwnd, false);
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    });
    match handled {
        Ok(Some(result)) => result,
        Ok(None) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        Err(_) => LRESULT(0),
    }
}

const CLASS: &str = "PortputerWindow";

pub fn run() {
    let background = std::env::args().any(|a| a == "--background");
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let class = ui::wide(CLASS);
        // One at a time: starting it again shows the window.
        if let Ok(existing) = FindWindowW(PCWSTR(class.as_ptr()), None) {
            if !existing.is_invalid() {
                if !background {
                    show_window(existing);
                }
                return;
            }
        }
        let _ = windows::Win32::UI::Controls::InitCommonControlsEx(&windows::Win32::UI::Controls::INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<windows::Win32::UI::Controls::INITCOMMONCONTROLSEX>() as u32,
            dwICC: windows::Win32::UI::Controls::ICC_STANDARD_CLASSES | windows::Win32::UI::Controls::ICC_BAR_CLASSES,
        });
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: ui::hinstance(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH((windows::Win32::Graphics::Gdi::COLOR_BTNFACE.0 + 1) as usize as *mut _),
            lpszClassName: PCWSTR(class.as_ptr()),
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let title = ui::wide("Portputer");
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            620,
            860,
            None,
            None,
            Some(ui::hinstance()),
            None,
        ) else {
            return;
        };
        ui::set_main_window(hwnd);
        let controls = App::create_controls(hwnd);
        ui::set_checked(controls.startup, starts_at_login());
        let browser = Browser::start(|event| {
            ui::run_on_ui(move || {
                with_app(|a| {
                    match event {
                        BrowseEvent::Found(ad) => {
                            a.ads.insert(ad.fullname.clone(), ad);
                        }
                        BrowseEvent::Lost(name) => {
                            a.ads.remove(&name);
                        }
                    }
                    if a.conn == Conn::Idle {
                        a.refresh_computers();
                    }
                });
            });
        })
        .ok();
        APP.with(|cell| {
            *cell.borrow_mut() = Some(App {
                hwnd,
                c: controls,
                server: None,
                browser,
                ads: HashMap::new(),
                computers: vec![],
                paired: vec![],
                session: None,
                conn: Conn::Idle,
                generation: 0,
                capturing: false,
                audio_wanted: false,
                menu_keys: vec![],
            })
        });
        with_app(|a| {
            if setting("accept").as_deref() != Some("0") {
                a.start_server();
            }
            a.refresh();
            let _ = &a.browser;
        });
        tray(hwnd, true);
        if !background {
            show_window(hwnd);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if IsDialogMessageW(hwnd, &msg).as_bool() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
