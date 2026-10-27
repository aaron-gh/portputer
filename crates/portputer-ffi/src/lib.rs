//! The bridge between Portputer's Rust core and its Swift (Mac) and Kotlin
//! (Android) apps. UniFFI generates the bindings from this file. Calls that
//! wait on the network (connecting, pairing) block, so call them off the
//! main thread. Listeners are called from the core's threads.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use portputer_core::client::{self, ClientConfig, Session, SessionEvent, SessionObserver, SessionState};
use portputer_core::discovery::{self, BrowseEvent, Browser, ServerAd};
use portputer_core::keys::{self, Os};
use portputer_core::protocol::{PowerAction, SessionKind};
use portputer_core::server::{Server, ServerConfig, ServerEvent, ServerObserver};

uniffi::setup_scaffolding!();

/// Every error, as a sentence to show or read aloud.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PortputerError {
    #[error("{message}")]
    Failed { message: String },
    #[error("A different computer answered at that address.")]
    WrongServer,
    #[error("{message}")]
    Refused { message: String },
    #[error("The connection is closed.")]
    Closed,
}

impl From<portputer_core::Error> for PortputerError {
    fn from(e: portputer_core::Error) -> Self {
        match e {
            portputer_core::Error::WrongServer => PortputerError::WrongServer,
            portputer_core::Error::Refused(message) => PortputerError::Refused { message },
            portputer_core::Error::Closed => PortputerError::Closed,
            other => PortputerError::Failed { message: other.to_string() },
        }
    }
}

fn os_name(os: Os) -> String {
    match os {
        Os::Mac => "mac",
        Os::Windows => "windows",
        Os::Other => "other",
    }
    .into()
}

fn parse_os(os: &str) -> Os {
    match os {
        "mac" => Os::Mac,
        "windows" => Os::Windows,
        _ => Os::Other,
    }
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Restart,
    Shutdown,
    Sleep,
    LogOut,
    Lock,
    ForceRestart,
    UnlockedRestart,
}

impl From<PowerAction> for Power {
    fn from(a: PowerAction) -> Self {
        match a {
            PowerAction::Restart => Power::Restart,
            PowerAction::Shutdown => Power::Shutdown,
            PowerAction::Sleep => Power::Sleep,
            PowerAction::LogOut => Power::LogOut,
            PowerAction::Lock => Power::Lock,
            PowerAction::ForceRestart => Power::ForceRestart,
            PowerAction::UnlockedRestart => Power::UnlockedRestart,
        }
    }
}

impl From<Power> for PowerAction {
    fn from(a: Power) -> Self {
        match a {
            Power::Restart => PowerAction::Restart,
            Power::Shutdown => PowerAction::Shutdown,
            Power::Sleep => PowerAction::Sleep,
            Power::LogOut => PowerAction::LogOut,
            Power::Lock => PowerAction::Lock,
            Power::ForceRestart => PowerAction::ForceRestart,
            Power::UnlockedRestart => PowerAction::UnlockedRestart,
        }
    }
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionType {
    User,
    LoginWindow,
}

impl From<SessionKind> for SessionType {
    fn from(s: SessionKind) -> Self {
        match s {
            SessionKind::User => SessionType::User,
            SessionKind::LoginWindow => SessionType::LoginWindow,
        }
    }
}

// ---- Keys ----

#[derive(uniffi::Record, Clone)]
pub struct MenuKey {
    pub label: String,
    pub code: String,
    pub modifiers: Vec<String>,
    /// A modifier on its own, which can be held.
    pub holdable: bool,
}

#[derive(uniffi::Record, Clone)]
pub struct MenuSection {
    pub title: String,
    pub keys: Vec<MenuKey>,
}

/// The key menu for a server's system ("mac" or "windows").
#[uniffi::export]
pub fn key_menu(os: String) -> Vec<MenuSection> {
    keys::menu(parse_os(&os))
        .into_iter()
        .map(|s| MenuSection {
            title: s.title,
            keys: s
                .keys
                .into_iter()
                .map(|k| MenuKey {
                    holdable: k.modifiers.is_empty() && keys::is_modifier(&k.code),
                    label: k.label,
                    code: k.code,
                    modifiers: k.modifiers,
                })
                .collect(),
        })
        .collect()
}

#[uniffi::export]
pub fn key_label(code: String, os: String) -> String {
    keys::label(&code, parse_os(&os))
}

/// The code for an Android key event: by physical position (the scan code)
/// when `by_position` and there is one, otherwise by Android's key code.
#[uniffi::export]
pub fn code_for_android_key(scan_code: u32, key_code: u32, by_position: bool) -> Option<String> {
    let by_scan = || keys::from_evdev(scan_code as u16);
    let by_key = || keys::from_android(key_code as u16);
    let found = if by_position { by_scan().or_else(by_key) } else { by_key().or_else(by_scan) };
    found.map(String::from)
}

#[uniffi::export]
pub fn code_for_mac_key(key_code: u16) -> Option<String> {
    keys::from_mac(key_code).map(String::from)
}

#[uniffi::export]
pub fn is_modifier_code(code: String) -> bool {
    keys::is_modifier(&code)
}

// ---- Discovery ----

#[derive(uniffi::Record, Clone, Debug)]
pub struct Advertisement {
    pub fullname: String,
    pub id: String,
    pub name: String,
    pub os: String,
    pub session: SessionType,
    pub user: String,
    pub addresses: Vec<String>,
    pub port: u16,
    pub macs: Vec<String>,
}

impl From<ServerAd> for Advertisement {
    fn from(ad: ServerAd) -> Self {
        Advertisement {
            fullname: ad.fullname,
            id: ad.id,
            name: ad.name,
            os: os_name(ad.os),
            session: ad.session.into(),
            user: ad.user,
            addresses: ad.addresses,
            port: ad.port,
            macs: ad.macs,
        }
    }
}

#[uniffi::export(with_foreign)]
pub trait DiscoveryListener: Send + Sync {
    fn found(&self, ad: Advertisement);
    fn lost(&self, fullname: String);
}

/// Looks for servers on the local network until stopped. On Android, hold a
/// multicast lock while it runs.
#[derive(uniffi::Object)]
pub struct Discovery {
    browser: Mutex<Option<Browser>>,
}

#[uniffi::export]
impl Discovery {
    #[uniffi::constructor]
    pub fn start(listener: Arc<dyn DiscoveryListener>) -> Result<Arc<Self>, PortputerError> {
        let browser = Browser::start(move |event| match event {
            BrowseEvent::Found(ad) => listener.found(ad.into()),
            BrowseEvent::Lost(name) => listener.lost(name),
        })?;
        Ok(Arc::new(Discovery { browser: Mutex::new(Some(browser)) }))
    }

    pub fn stop(&self) {
        self.browser.lock().unwrap().take();
    }
}

#[uniffi::export]
pub fn computer_name() -> String {
    discovery::computer_name()
}

// ---- Known servers ----

#[derive(uniffi::Record, Clone, Debug)]
pub struct KnownServer {
    pub id: String,
    pub name: String,
    pub os: String,
    pub host: String,
    pub port: u16,
    pub macs: Vec<String>,
    pub last_connected: u64,
}

#[uniffi::export]
pub fn known_servers(store_dir: String) -> Vec<KnownServer> {
    client::known_servers(&PathBuf::from(store_dir))
        .into_iter()
        .map(|s| KnownServer { id: s.id, name: s.name, os: s.os, host: s.host, port: s.port, macs: s.macs, last_connected: s.last_connected })
        .collect()
}

#[uniffi::export]
pub fn forget_server(store_dir: String, id: String) -> Result<(), PortputerError> {
    Ok(client::forget_server(&PathBuf::from(store_dir), &id)?)
}

/// Sends Wake on LAN packets. False if none could be sent.
#[uniffi::export]
pub fn wake(macs: Vec<String>) -> bool {
    portputer_core::wol::wake(&macs)
}

// ---- Client ----

#[derive(uniffi::Record, Clone, Debug)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub session: SessionType,
    pub user: String,
    pub version: String,
    pub power: Vec<Power>,
    pub audio: bool,
    pub host: String,
    pub port: u16,
    /// Paired both ways. Until it is, only `pair` does anything.
    pub trusted: bool,
}

impl From<client::ServerInfo> for ServerInfo {
    fn from(i: client::ServerInfo) -> Self {
        ServerInfo {
            id: i.id,
            name: i.name,
            os: os_name(i.os),
            session: i.session.into(),
            user: i.user,
            version: i.version,
            power: i.power.into_iter().map(Power::from).collect(),
            audio: i.audio,
            host: i.host,
            port: i.port,
            trusted: i.trusted,
        }
    }
}

#[uniffi::export(with_foreign)]
pub trait SessionListener: Send + Sync {
    fn connected(&self, info: ServerInfo);
    fn reconnecting(&self);
    fn closed(&self, reason: String);
    fn notice(&self, code: String, message: String);
    fn error(&self, code: String, message: String);
    fn audio_started(&self, sample_rate: u32);
    fn audio_stopped(&self, reason: String);
}

struct Relay(Arc<dyn SessionListener>);

impl SessionObserver for Relay {
    fn event(&self, event: SessionEvent) {
        match event {
            SessionEvent::State(SessionState::Connected(info)) => self.0.connected(info.into()),
            SessionEvent::State(SessionState::Reconnecting) => self.0.reconnecting(),
            SessionEvent::State(SessionState::Closed { reason }) => self.0.closed(reason),
            SessionEvent::Notice { code, message } => self.0.notice(code, message),
            SessionEvent::Error { code, message } => self.0.error(code, message),
            SessionEvent::AudioStarted { sample_rate } => self.0.audio_started(sample_rate),
            SessionEvent::AudioStopped { reason } => self.0.audio_stopped(reason),
        }
    }
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct AudioStats {
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub underruns: u64,
    pub buffered_ms: u32,
}

#[derive(uniffi::Object)]
pub struct ClientSession {
    session: Session,
}

#[uniffi::export]
impl ClientSession {
    /// Connects and waits for the server's welcome (up to about 15 seconds).
    #[uniffi::constructor]
    pub fn connect(
        host: String,
        port: u16,
        expected_id: Option<String>,
        store_dir: String,
        device_name: String,
        listener: Arc<dyn SessionListener>,
    ) -> Result<Arc<Self>, PortputerError> {
        init_logging();
        let config = ClientConfig { store_dir: PathBuf::from(store_dir), name: device_name, observer: Arc::new(Relay(listener)) };
        let session = Session::connect(&host, port, expected_id, config)?;
        Ok(Arc::new(ClientSession { session }))
    }

    pub fn info(&self) -> Option<ServerInfo> {
        self.session.info().map(ServerInfo::from)
    }

    pub fn pair(&self, code: String) -> Result<(), PortputerError> {
        Ok(self.session.pair(&code)?)
    }

    pub fn key(&self, code: String, down: bool) {
        self.session.key(&code, down);
    }

    pub fn tap(&self, code: String, modifiers: Vec<String>) {
        self.session.tap(&code, modifiers);
    }

    pub fn text(&self, text: String) {
        self.session.text(&text);
    }

    pub fn release_all(&self) {
        self.session.release_all();
    }

    pub fn power(&self, action: Power, user: Option<String>, password: Option<String>) {
        self.session.power(action.into(), user, password);
    }

    pub fn start_audio(&self) {
        self.session.start_audio();
    }

    pub fn stop_audio(&self) {
        self.session.stop_audio();
    }

    pub fn set_volume(&self, volume: f32) {
        self.session.set_volume(volume);
    }

    pub fn set_latency_ms(&self, ms: u32) {
        self.session.set_latency_ms(ms);
    }

    pub fn audio_stats(&self) -> Option<AudioStats> {
        self.session.audio_stats().map(|s| AudioStats {
            received: s.received,
            lost: s.lost,
            late: s.late,
            underruns: s.underruns,
            buffered_ms: s.buffered_ms,
        })
    }

    pub fn close(&self) {
        self.session.close();
    }
}

// ---- Server ----

#[derive(uniffi::Enum, Clone, Debug)]
pub enum ServerNotice {
    Listening { port: u16 },
    Connected { client_id: String, name: String, address: String },
    Disconnected { client_id: String, name: String },
    Paired { client_id: String, name: String },
    PairingFailed { name: String },
    InputNotAllowed,
    AudioFailed { message: String },
    Error { message: String },
}

impl From<ServerEvent> for ServerNotice {
    fn from(e: ServerEvent) -> Self {
        match e {
            ServerEvent::Listening { port } => ServerNotice::Listening { port },
            ServerEvent::Connected { client_id, name, address } => ServerNotice::Connected { client_id, name, address },
            ServerEvent::Disconnected { client_id, name } => ServerNotice::Disconnected { client_id, name },
            ServerEvent::Paired { client_id, name } => ServerNotice::Paired { client_id, name },
            ServerEvent::PairingFailed { name } => ServerNotice::PairingFailed { name },
            ServerEvent::InputNotAllowed => ServerNotice::InputNotAllowed,
            ServerEvent::AudioFailed { message } => ServerNotice::AudioFailed { message },
            ServerEvent::Error { message } => ServerNotice::Error { message },
        }
    }
}

#[uniffi::export(with_foreign)]
pub trait ServerListener: Send + Sync {
    fn event(&self, event: ServerNotice);
}

struct ServerRelay(Arc<dyn ServerListener>);

impl ServerObserver for ServerRelay {
    fn event(&self, event: ServerEvent) {
        self.0.event(event.into());
    }
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct PairedDevice {
    pub id: String,
    pub name: String,
    pub paired_at: u64,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct ConnectedDevice {
    pub id: String,
    pub name: String,
    pub address: String,
    pub paired: bool,
}

#[derive(uniffi::Object)]
pub struct ServerHandle {
    server: Mutex<Option<Server>>,
    store_dir: PathBuf,
}

impl ServerHandle {
    fn with<T>(&self, f: impl FnOnce(&Server) -> T) -> Option<T> {
        self.server.lock().unwrap().as_ref().map(f)
    }
}

#[uniffi::export]
impl ServerHandle {
    #[uniffi::constructor]
    pub fn start(store_dir: String, name: String, port: u16, listener: Arc<dyn ServerListener>) -> Result<Arc<Self>, PortputerError> {
        init_logging();
        let store_dir = PathBuf::from(store_dir);
        let server = Server::start(ServerConfig {
            store_dir: store_dir.clone(),
            session: SessionKind::User,
            port,
            name,
            observer: Some(Arc::new(ServerRelay(listener))),
        })?;
        Ok(Arc::new(ServerHandle { server: Mutex::new(Some(server)), store_dir }))
    }

    pub fn stop(&self) {
        self.server.lock().unwrap().take();
    }

    pub fn port(&self) -> u16 {
        self.with(|s| s.port()).unwrap_or(0)
    }

    pub fn id(&self) -> String {
        self.with(|s| s.id()).unwrap_or_default()
    }

    /// A new code, shown as XXXX-XXXX-XXXX, good for ten minutes.
    pub fn new_pairing_code(&self) -> String {
        self.with(|s| s.new_pairing_code()).unwrap_or_default()
    }

    pub fn cancel_pairing(&self) {
        self.with(|s| s.cancel_pairing());
    }

    pub fn pairing_seconds_left(&self) -> u64 {
        self.with(|s| s.pairing_seconds_left()).unwrap_or(0)
    }

    pub fn paired(&self) -> Vec<PairedDevice> {
        portputer_core::identity::ServerStore::new(&self.store_dir)
            .paired()
            .into_iter()
            .map(|p| PairedDevice { id: p.id, name: p.name, paired_at: p.paired_at })
            .collect()
    }

    pub fn unpair(&self, id: String) -> Result<(), PortputerError> {
        Ok(portputer_core::identity::ServerStore::new(&self.store_dir).remove_paired(&id)?)
    }

    pub fn connected(&self) -> Vec<ConnectedDevice> {
        self.with(|s| s.connected())
            .unwrap_or_default()
            .into_iter()
            .map(|c| ConnectedDevice { id: c.id, name: c.name, address: c.address, paired: c.paired })
            .collect()
    }
}

/// Whether this process may post key events; on a Mac, `ask` shows the
/// Accessibility permission request.
#[uniffi::export]
pub fn input_allowed(ask: bool) -> bool {
    portputer_core::platform::input_allowed(ask)
}

/// Whether the helper daemon and the server at the login window are
/// installed (Mac).
#[uniffi::export]
pub fn system_components_installed() -> bool {
    #[cfg(target_os = "macos")]
    return portputer_core::platform::macos::helper_available();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Gives the server at the login window this server's identity and paired
/// devices, through the helper daemon (Mac).
#[uniffi::export]
pub fn sync_login_window(store_dir: String) -> Result<(), PortputerError> {
    #[cfg(target_os = "macos")]
    {
        use portputer_core::platform::macos::{HelperRequest, helper};
        let dir = PathBuf::from(store_dir);
        let store = portputer_core::identity::ServerStore::new(&dir);
        store.identity()?;
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_else(|_| "[]".into());
        let request = HelperRequest { sync: Some((read("identity.json"), read("paired.json"))), ..Default::default() };
        helper(&request).map_err(|message| PortputerError::Failed { message })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = store_dir;
        Err(PortputerError::Failed { message: "Only on a Mac.".into() })
    }
}

fn init_logging() {
    #[cfg(target_os = "android")]
    android_logger::init_once(android_logger::Config::default().with_tag("portputer").with_max_level(log::LevelFilter::Info));
}
