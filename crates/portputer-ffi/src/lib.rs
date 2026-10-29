//! The bridge between Portputer's Rust core and its Swift (Mac, iOS) and
//! Kotlin (Android) apps. UniFFI generates the bindings from this file. Calls that
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

/// Every error, as a sentence to show or read aloud: the bindings' exception
/// message.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
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

/// The code for an iOS key press: by physical position (the HID usage, from
/// `UIKey.keyCode`) when `by_position`, otherwise by the character the key
/// types without modifiers in this device's layout, where a US key types it.
#[uniffi::export]
pub fn code_for_ios_key(hid_usage: u16, characters: String, by_position: bool) -> Option<String> {
    let by_char = || {
        let mut chars = characters.chars();
        let c = chars.next().filter(|_| chars.next().is_none())?;
        keys::us_key_for_char(c.to_ascii_lowercase()).map(|(code, _)| code)
    };
    let found = if by_position { keys::from_hid(hid_usage) } else { by_char().or_else(|| keys::from_hid(hid_usage)) };
    found.map(String::from)
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

/// A server resolved by the app's own Bonjour browsing (iOS, where the core
/// can't browse): its full name, TXT record, addresses and port. Noted for
/// reconnecting; None if it isn't a Portputer server.
#[uniffi::export]
pub fn network_found(
    fullname: String,
    txt: std::collections::HashMap<String, String>,
    addresses: Vec<String>,
    port: u16,
) -> Option<Advertisement> {
    let ips = addresses.iter().filter_map(|a| a.parse().ok()).collect();
    let ad = discovery::server_ad(&fullname, |k| txt.get(k).cloned().unwrap_or_default(), ips, port)?;
    client::note_seen(BrowseEvent::Found(ad.clone()));
    Some(ad.into())
}

#[uniffi::export]
pub fn network_lost(fullname: String) {
    client::note_seen(BrowseEvent::Lost(fullname));
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
    /// Reachable through the internet relay when not on the network.
    pub has_relay: bool,
}

#[uniffi::export]
pub fn known_servers(store_dir: String) -> Vec<KnownServer> {
    client::known_servers(&PathBuf::from(store_dir))
        .into_iter()
        .map(|s| KnownServer {
            id: s.id,
            name: s.name,
            os: s.os,
            host: s.host,
            port: s.port,
            macs: s.macs,
            last_connected: s.last_connected,
            has_relay: s.relay.is_some(),
        })
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
    /// The computer's speakers can be muted while it streams.
    pub mute: bool,
    /// The computer offers this device's microphone to its apps.
    pub mic: bool,
    /// Clipboard and file transfer are available.
    pub files: bool,
    /// Connected through the internet relay.
    pub via_relay: bool,
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
            mute: i.mute,
            mic: i.mic,
            files: i.files,
            via_relay: i.via_relay,
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
    fn mic_started(&self);
    fn mic_stopped(&self, reason: String);
    /// Text copied on the computer.
    fn clipboard_text(&self, text: String);
    /// Files copied on the computer; `ClientSession::clipboard_files` fetches them.
    fn clipboard_files(&self, files: Vec<FileInfo>);
    /// What the client did with this computer's clipboard (desktop apps).
    fn clipboard_synced(&self, message: String);
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub size: u64,
    pub modified: u64,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FilePlace {
    pub name: String,
    pub path: String,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FileListing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<FileEntry>,
    pub places: Vec<FilePlace>,
}

#[uniffi::export(with_foreign)]
pub trait ProgressListener: Send + Sync {
    fn progress(&self, done: u64, total: u64);
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
            SessionEvent::MicStarted => self.0.mic_started(),
            SessionEvent::MicStopped { reason } => self.0.mic_stopped(reason),
            SessionEvent::ClipboardText { text } => self.0.clipboard_text(text),
            SessionEvent::ClipboardFiles { files } => {
                self.0.clipboard_files(files.into_iter().map(|f| FileInfo { name: f.name, size: f.size }).collect())
            }
            SessionEvent::ClipboardSynced { message } => self.0.clipboard_synced(message),
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
        local_clipboard: bool,
        listener: Arc<dyn SessionListener>,
    ) -> Result<Arc<Self>, PortputerError> {
        init_logging();
        let config = ClientConfig { store_dir: PathBuf::from(store_dir), name: device_name, observer: Arc::new(Relay(listener)), local_clipboard };
        let session = Session::connect(&host, port, expected_id, config)?;
        Ok(Arc::new(ClientSession { session }))
    }

    pub fn info(&self) -> Option<ServerInfo> {
        self.session.info().map(ServerInfo::from)
    }

    pub fn pair(&self, code: String) -> Result<(), PortputerError> {
        Ok(self.session.pair(&code)?)
    }

    /// `repeat` for the repeated key-downs of a held key.
    pub fn key(&self, code: String, down: bool, repeat: bool) {
        self.session.key(&code, down, repeat);
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

    /// Sends this device's microphone to the computer.
    pub fn start_mic(&self) {
        self.session.start_mic();
    }

    pub fn stop_mic(&self) {
        self.session.stop_mic();
    }

    /// Has the computer send its clipboard as it changes.
    pub fn set_clipboard_watch(&self, on: bool) {
        self.session.set_clipboard_watch(on);
    }

    pub fn send_clipboard_text(&self, text: String) {
        self.session.send_clipboard_text(&text);
    }

    /// A folder on the computer; empty is the home folder. Blocks.
    pub fn list_files(&self, path: String) -> Result<FileListing, PortputerError> {
        match self.session.list_files(&path)? {
            portputer_core::files::TransferReply::Listing { path, parent, entries, places } => Ok(FileListing {
                path,
                parent,
                entries: entries
                    .into_iter()
                    .map(|e| FileEntry { name: e.name, path: e.path, dir: e.dir, size: e.size, modified: e.modified })
                    .collect(),
                places: places.into_iter().map(|p| FilePlace { name: p.name, path: p.path }).collect(),
            }),
            _ => Err(PortputerError::Failed { message: "Unexpected reply.".into() }),
        }
    }

    /// Downloads a file from the computer into `dest_dir`; returns where. Blocks.
    pub fn download(&self, path: String, dest_dir: String, listener: Arc<dyn ProgressListener>) -> Result<String, PortputerError> {
        let saved = self.session.download(&path, &PathBuf::from(dest_dir), move |d, t| listener.progress(d, t))?;
        Ok(saved.display().to_string())
    }

    /// Uploads a file to the computer's Downloads folder, or with
    /// `clipboard` (one number per copy), to its clipboard. Blocks.
    pub fn upload(
        &self,
        path: String,
        name: Option<String>,
        clipboard: Option<u64>,
        listener: Arc<dyn ProgressListener>,
    ) -> Result<String, PortputerError> {
        Ok(self.session.upload(&PathBuf::from(path), name, clipboard, move |d, t| listener.progress(d, t))?)
    }

    /// Fetches the files on the computer's clipboard into `dest_dir`. Blocks.
    pub fn clipboard_files(&self, dest_dir: String, listener: Arc<dyn ProgressListener>) -> Result<Vec<String>, PortputerError> {
        let paths = self.session.clipboard_files(&PathBuf::from(dest_dir), move |d, t| listener.progress(d, t))?;
        Ok(paths.into_iter().map(|p| p.display().to_string()).collect())
    }

    /// Keeps the sound from the computer's own speakers while it streams.
    pub fn set_speakers_muted(&self, muted: bool) {
        self.session.set_speakers_muted(muted);
    }

    pub fn set_volume(&self, volume: f32) {
        self.session.set_volume(volume);
    }

    /// Milliseconds of audio buffer; 0 is automatic.
    pub fn set_latency_ms(&self, ms: u32) {
        self.session.set_latency_ms(ms);
    }

    /// Use Internet Relay: through the relay even when the computer can be
    /// reached directly. Off for each new session.
    pub fn set_use_relay(&self, on: bool) {
        self.session.set_use_relay(on);
    }

    /// Whether the computer can be reached through the relay.
    pub fn relay_available(&self) -> bool {
        self.session.relay_available()
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

    /// Ends the session. (Not `close`, which the Kotlin bindings use to free
    /// the object.)
    pub fn disconnect(&self) {
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
    Relay { listening: bool, message: String },
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
            ServerEvent::Relay { listening, message } => ServerNotice::Relay { listening, message },
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
pub struct RelaySettings {
    pub enabled: bool,
    /// Empty for the built-in relay.
    pub custom_url: String,
    /// The relay this build uses by default; empty if it has none.
    pub built_in_url: String,
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

    /// Turns the internet relay on or off; `custom_url` empty for the
    /// built-in relay.
    pub fn set_relay(&self, enabled: bool, custom_url: String) -> Result<(), PortputerError> {
        let custom = Some(custom_url).filter(|u| !u.trim().is_empty());
        match self.server.lock().unwrap().as_ref() {
            Some(s) => Ok(s.set_relay(enabled, custom)?),
            None => {
                let store = portputer_core::identity::ServerStore::new(&self.store_dir);
                let mut home = store.relay_home();
                home.enabled = enabled;
                home.custom_url = custom;
                Ok(store.set_relay_home(&home)?)
            }
        }
    }

    pub fn relay_settings(&self) -> RelaySettings {
        let home = portputer_core::identity::ServerStore::new(&self.store_dir).relay_home();
        RelaySettings {
            enabled: home.enabled,
            custom_url: home.custom_url.unwrap_or_default(),
            built_in_url: portputer_core::relay::default_url().unwrap_or("").to_string(),
        }
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

/// The separate permission checks behind `input_allowed`, for the log.
#[uniffi::export]
pub fn input_permission_detail() -> String {
    #[cfg(target_os = "macos")]
    return portputer_core::platform::macos::input_permission_detail();
    #[cfg(not(target_os = "macos"))]
    String::new()
}

/// Whether the Portputer Microphone driver is installed (Mac).
#[uniffi::export]
pub fn microphone_driver_installed() -> bool {
    #[cfg(target_os = "macos")]
    return portputer_core::audio::feed::installed();
    #[cfg(not(target_os = "macos"))]
    false
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
        let relay = std::fs::read_to_string(dir.join("relay.json")).ok();
        let request = HelperRequest { sync: Some((read("identity.json"), read("paired.json"))), relay, ..Default::default() };
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

#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum Reachability {
    /// At one of its addresses; `tailscale` when that's a Tailscale address.
    Direct { host: String, tailscale: bool },
    Relay,
    NotReachable,
}

/// Whether a paired computer can be reached now, and how, without
/// connecting to it. Blocks for up to about 8 seconds.
#[uniffi::export]
pub fn probe_computer(store_dir: String, id: String) -> Reachability {
    match client::probe(&PathBuf::from(store_dir), &id) {
        client::Reachability::Direct { host, tailscale } => Reachability::Direct { host, tailscale },
        client::Reachability::Relay => Reachability::Relay,
        client::Reachability::NotReachable => Reachability::NotReachable,
    }
}
