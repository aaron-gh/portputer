//! The server: accepts clients, checks they're paired, turns their messages
//! into key events and power actions, and streams the computer's sound.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::TcpListener;

use crate::Error;
use crate::audio::{self, Capture};
use crate::discovery::{self, AnnounceInfo, Announcement};
use crate::identity::{Identity, PairedClient, PairingCode, ServerStore, id_of};
use crate::platform;
use crate::protocol::{ClientMessage, PowerAction, ServerMessage, SessionKind, errors, notices};
use crate::transport;
use crate::udp::{self, PacketKey};

#[derive(Clone, Debug)]
pub enum ServerEvent {
    Listening { port: u16 },
    Connected { client_id: String, name: String, address: String },
    Disconnected { client_id: String, name: String },
    Paired { client_id: String, name: String },
    PairingFailed { name: String },
    /// Key events can't be posted: the system hasn't given permission.
    InputNotAllowed,
    AudioFailed { message: String },
    /// Whether the server is listening at the internet relay.
    Relay { listening: bool, message: String },
    Error { message: String },
}

pub trait ServerObserver: Send + Sync {
    fn event(&self, event: ServerEvent);
}

pub struct ServerConfig {
    pub store_dir: std::path::PathBuf,
    pub session: SessionKind,
    /// Tried first; another is picked if it's taken.
    pub port: u16,
    pub name: String,
    pub observer: Option<Arc<dyn ServerObserver>>,
}

#[derive(Clone, Debug)]
pub struct ConnectedClient {
    pub id: String,
    pub name: String,
    pub address: String,
    pub paired: bool,
}

struct Shared {
    store: ServerStore,
    identity: Identity,
    session: SessionKind,
    name: String,
    pairing: Mutex<Option<PairingCode>>,
    clients: Mutex<HashMap<u64, ConnectedClient>>,
    input: std::sync::mpsc::Sender<InputCommand>,
    audio: AudioHub,
    observer: Option<Arc<dyn ServerObserver>>,
    next_conn: AtomicU64,
    clipboard_batch: crate::files::ClipboardBatch,
    /// Relayed clients' microphones, by client ID, as their packets arrive
    /// on datagram connections.
    mic_inputs: Arc<MicInputs>,
    relay_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The paired clients' relay token hashes, which the relay is kept up
    /// to date with.
    relay_allowed: tokio::sync::watch::Sender<Vec<String>>,
    /// The port this server listens on.
    port: u16,
    /// The clipboard's change count after Portputer last wrote it, so that
    /// isn't sent back.
    own_clipboard: std::sync::atomic::AtomicI64,
}

impl Shared {
    fn refresh_relay_allowed(&self) {
        let allowed = self.store.relay_allowed();
        self.relay_allowed.send_if_modified(|current| {
            let changed = *current != allowed;
            *current = allowed;
            changed
        });
    }

    /// The relay details for one paired client, when the relay is on.
    fn relay_message(&self, client_key: &[u8; 32]) -> Option<ServerMessage> {
        let home = self.store.relay_home();
        let url = home.url().filter(|_| home.enabled)?;
        let token = self.store.relay_token(client_key)?;
        Some(ServerMessage::Relay { url, room: home.room, token })
    }

    fn emit(&self, event: ServerEvent) {
        if let Some(o) = &self.observer {
            o.event(event);
        }
    }
}

enum InputCommand {
    Key(String, bool, bool),
    Tap(String, Vec<String>),
    Text(String),
    ReleaseAll,
}

/// A running server. Stops when dropped.
pub struct Server {
    shared: Arc<Shared>,
    runtime: Option<tokio::runtime::Runtime>,
    port: u16,
    _announcement: Option<Announcement>,
}

impl Server {
    pub fn start(config: ServerConfig) -> Result<Server, Error> {
        #[cfg(target_os = "macos")]
        {
            audio::loopback::use_for_capture(config.session == SessionKind::LoginWindow);
            // Outputs the login window's server left routed through Portputer.
            if config.session == SessionKind::User && audio::loopback::recover() {
                tracing::info!("put back the outputs the login window's server had changed");
            }
        }
        let store = ServerStore::new(&config.store_dir);
        let identity = store.identity()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("portputer-server")
            .enable_all()
            .build()?;
        let listener = runtime.block_on(async {
            match TcpListener::bind(("0.0.0.0", config.port)).await {
                Ok(l) => Ok(l),
                Err(_) => TcpListener::bind(("0.0.0.0", 0)).await,
            }
        })?;
        let port = listener.local_addr()?.port();
        let (input_tx, input_rx) = std::sync::mpsc::channel();
        let session = config.session;
        std::thread::Builder::new().name("portputer-input".into()).spawn(move || input_thread(session, input_rx))?;
        let shared = Arc::new(Shared {
            store,
            identity,
            session,
            name: config.name.clone(),
            pairing: Mutex::new(None),
            clients: Mutex::new(HashMap::new()),
            input: input_tx,
            audio: AudioHub::default(),
            observer: config.observer,
            next_conn: AtomicU64::new(1),
            clipboard_batch: Default::default(),
            mic_inputs: Default::default(),
            relay_task: Mutex::new(None),
            relay_allowed: tokio::sync::watch::Sender::new(Vec::new()),
            port,
            own_clipboard: std::sync::atomic::AtomicI64::new(-1),
        });
        // PORTPUTER_NO_PROMPT keeps tests from asking for permission.
        let ask = session == SessionKind::User && std::env::var_os("PORTPUTER_NO_PROMPT").is_none();
        if !platform::input_allowed(ask) {
            shared.emit(ServerEvent::InputNotAllowed);
        }
        let accept = shared.clone();
        runtime.spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, address)) => {
                        let shared = accept.clone();
                        let _ = stream.set_nodelay(true);
                        tokio::spawn(async move {
                            if let Err(e) = serve(shared, Box::new(stream), Some(address)).await {
                                tracing::debug!("client {address}: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!("accept: {e}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            }
        });
        let user = platform::console_user();
        let announcement = match discovery::announce(AnnounceInfo {
            id: &shared.identity.id(),
            name: &config.name,
            session,
            user: &user,
            port,
        }) {
            Ok(a) => Some(a),
            Err(e) => {
                shared.emit(ServerEvent::Error { message: format!("Discovery: {e}") });
                None
            }
        };
        shared.emit(ServerEvent::Listening { port });
        let server = Server { shared, runtime: Some(runtime), port, _announcement: announcement };
        server.restart_relay();
        Ok(server)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn id(&self) -> String {
        self.shared.identity.id()
    }

    /// Makes a new pairing code, good for ten minutes or five wrong tries,
    /// and returns it as shown.
    pub fn new_pairing_code(&self) -> String {
        let code = PairingCode::new();
        let shown = code.display();
        *self.shared.pairing.lock().unwrap() = Some(code);
        shown
    }

    pub fn cancel_pairing(&self) {
        *self.shared.pairing.lock().unwrap() = None;
    }

    pub fn pairing_seconds_left(&self) -> u64 {
        self.shared.pairing.lock().unwrap().as_ref().filter(|c| c.valid()).map_or(0, |c| c.expires_in().as_secs())
    }

    pub fn paired(&self) -> Vec<PairedClient> {
        self.shared.store.paired()
    }

    /// Unpairs a client, which also stops letting it in at the relay.
    pub fn unpair(&self, id: &str) -> Result<(), Error> {
        self.shared.store.remove_paired(id)?;
        self.shared.refresh_relay_allowed();
        Ok(())
    }

    pub fn connected(&self) -> Vec<ConnectedClient> {
        self.shared.clients.lock().unwrap().values().cloned().collect()
    }

    /// Turns the internet relay on or off, and sets another relay than the
    /// built-in one (none for the built-in one).
    pub fn set_relay(&self, enabled: bool, custom_url: Option<String>) -> Result<(), Error> {
        let mut home = self.shared.store.relay_home();
        home.enabled = enabled;
        home.custom_url = custom_url.filter(|u| !u.trim().is_empty());
        self.shared.store.set_relay_home(&home)?;
        self.restart_relay();
        Ok(())
    }

    pub fn relay_home(&self) -> crate::relay::RelayHome {
        self.shared.store.relay_home()
    }

    fn restart_relay(&self) {
        let Some(runtime) = &self.runtime else { return };
        if let Some(task) = self.shared.relay_task.lock().unwrap().take() {
            task.abort();
        }
        let home = self.shared.store.relay_home();
        if !home.enabled {
            self.shared.emit(ServerEvent::Relay { listening: false, message: "Off.".into() });
            return;
        }
        let Some(url) = home.url() else {
            self.shared.emit(ServerEvent::Relay { listening: false, message: "This build has no relay address.".into() });
            return;
        };
        self.shared.relay_allowed.send_replace(self.shared.store.relay_allowed());
        let refresh = self.shared.clone();
        let accept_shared = self.shared.clone();
        let state_shared = self.shared.clone();
        let session = self.shared.session;
        let allowed = self.shared.relay_allowed.subscribe();
        let task = runtime.spawn(async move {
            // Pairings can change in another process too (the app, for the
            // server at the login window), so the list is read again now and
            // then as well as when this server pairs or unpairs.
            let refresher = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    refresh.refresh_relay_allowed();
                }
            });
            crate::relay::listen(
            url,
            home,
            session,
            allowed,
            move |stream| {
                let shared = accept_shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(shared, stream, None).await {
                        tracing::debug!("relayed client: {e}");
                    }
                });
            },
            move |state| {
                let (listening, message) = match state {
                    Ok(()) => (true, "Listening.".to_string()),
                    Err(e) => (false, e),
                };
                state_shared.emit(ServerEvent::Relay { listening, message });
            },
            )
            .await;
            refresher.abort();
        });
        *self.shared.relay_task.lock().unwrap() = Some(task);
    }

    pub fn input_allowed(&self) -> bool {
        platform::input_allowed(false)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.shared.input.send(InputCommand::ReleaseAll);
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_millis(500));
        }
    }
}

fn input_thread(session: SessionKind, commands: std::sync::mpsc::Receiver<InputCommand>) {
    let mut injector = platform::new_injector(session);
    // At the login window, where typing is hardest to check: that keys
    // arrive (never which: they're passwords), and what macOS says about
    // posting them.
    let mut first = true;
    while let Ok(command) = commands.recv() {
        if session == SessionKind::LoginWindow && matches!(command, InputCommand::Key(..) | InputCommand::Tap(..) | InputCommand::Text(..)) && std::mem::take(&mut first) {
            #[cfg(target_os = "macos")]
            tracing::info!("keys arriving at the login window; {}", platform::macos::input_permission_detail());
        }
        match command {
            InputCommand::Key(code, _, true) => {
                injector.repeat(&code);
            }
            InputCommand::Key(code, down, false) => {
                injector.key(&code, down);
            }
            InputCommand::Tap(code, modifiers) => {
                injector.tap(&code, &modifiers);
            }
            InputCommand::Text(text) => injector.text(&text),
            InputCommand::ReleaseAll => injector.release_all(),
        }
    }
    injector.release_all();
}

/// Serves one connection, over TCP (`address`) or through the relay (none).
async fn serve(shared: Arc<Shared>, stream: transport::BoxStream, address: Option<SocketAddr>) -> Result<(), Error> {
    let client_ip = address.map(|a| a.ip());
    let shown_address = address.map_or_else(|| "internet relay".to_string(), |a| a.ip().to_string());
    let hs = tokio::time::timeout(Duration::from_secs(10), transport::respond(stream, &shared.identity.private))
        .await
        .map_err(|_| Error::Protocol("handshake timed out".into()))??;
    let client_key = hs.remote_static;
    let client_id = id_of(&client_key);
    let (name, protocol, transfer, datagram) = match serde_json::from_slice::<ClientMessage>(&hs.payload) {
        Ok(ClientMessage::Hello { name, protocol, transfer, datagram }) => (name, protocol, transfer, datagram),
        _ => return Err(Error::Protocol("no hello".into())),
    };
    let writer = hs.writer.clone();
    let mut reader = hs.reader;
    if protocol != crate::protocol::PROTOCOL_VERSION {
        writer
            .send(&ServerMessage::Error {
                code: errors::VERSION.into(),
                message: format!("This computer uses Portputer protocol {}; the client uses {protocol}.", crate::protocol::PROTOCOL_VERSION),
            })
            .await?;
        return Ok(());
    }
    let mut paired = shared.store.is_paired(&client_key);
    let conn = shared.next_conn.fetch_add(1, Ordering::Relaxed);
    shared.clients.lock().unwrap().insert(
        conn,
        ConnectedClient { id: client_id.clone(), name: name.clone(), address: shown_address.clone(), paired },
    );
    if !transfer && datagram.is_none() {
        shared.emit(ServerEvent::Connected { client_id: client_id.clone(), name: name.clone(), address: shown_address.clone() });
    }

    writer
        .send(&ServerMessage::Welcome {
            server_id: shared.identity.id(),
            name: shared.name.clone(),
            os: crate::keys::Os::current(),
            session: shared.session,
            user: platform::console_user(),
            paired,
            version: env!("CARGO_PKG_VERSION").into(),
            power: platform::power_actions(shared.session),
            audio: audio_available(shared.session),
            mute: audio_available(shared.session) && platform::can_mute_speakers(),
            mic: microphone_available(),
            files: files_available(shared.session),
        })
        .await?;
    if let Some(purpose) = datagram {
        shared.clients.lock().unwrap().remove(&conn);
        if !paired {
            return Ok(());
        }
        return serve_datagrams(shared, reader, writer, &client_id, &purpose).await;
    }
    if transfer {
        shared.clients.lock().unwrap().remove(&conn);
        if !paired || !files_available(shared.session) {
            writer.send(&ServerMessage::Error { code: errors::UNSUPPORTED.into(), message: "File transfer isn't available.".into() }).await?;
            return Ok(());
        }
        return crate::files::serve(reader, writer, &shared.clipboard_batch, &shared.own_clipboard).await;
    }
    // Only on control connections: transfer connections answer requests.
    if paired {
        if let Some(message) = shared.relay_message(&client_key) {
            writer.send(&message).await?;
        }
        writer.send(&ServerMessage::Addresses { addresses: own_addresses(), port: shared.port }).await?;
    }
    if paired && !platform::input_allowed(false) {
        writer.send(&input_notice()).await?;
    }

    let mut audio: Option<AudioSubscription> = None;
    let mut mic: Option<MicSink> = None;
    let mut clipboard_watch: Option<tokio::task::JoinHandle<()>> = None;
    let result: Result<(), Error> = async {
        loop {
            let message: ClientMessage = tokio::time::timeout(Duration::from_secs(30), reader.recv())
                .await
                .map_err(|_| Error::Protocol("client stopped responding".into()))??;
            match message {
                ClientMessage::Ping { n } => writer.send(&ServerMessage::Pong { n }).await?,
                ClientMessage::Pair { proof } => {
                    let outcome = {
                        let mut pairing = shared.pairing.lock().unwrap();
                        match pairing.as_mut() {
                            None => Err(errors::NO_PAIRING_CODE),
                            Some(code) => {
                                if code.check(&hs.hash, &proof) {
                                    let reply = code.server_proof(&hs.hash);
                                    *pairing = None;
                                    Ok(reply)
                                } else {
                                    Err(errors::PAIRING_FAILED)
                                }
                            }
                        }
                    };
                    match outcome {
                        Ok(reply) => {
                            shared.store.add_paired(client_key, &name)?;
                            paired = true;
                            if let Some(c) = shared.clients.lock().unwrap().get_mut(&conn) {
                                c.paired = true;
                            }
                            shared.emit(ServerEvent::Paired { client_id: client_id.clone(), name: name.clone() });
                            writer.send(&ServerMessage::Paired { proof: reply }).await?;
                            shared.refresh_relay_allowed();
                            if let Some(message) = shared.relay_message(&client_key) {
                                writer.send(&message).await?;
                            }
                            writer.send(&ServerMessage::Addresses { addresses: own_addresses(), port: shared.port }).await?;
                            if !platform::input_allowed(false) {
                                writer.send(&input_notice()).await?;
                            }
                        }
                        Err(code) => {
                            shared.emit(ServerEvent::PairingFailed { name: name.clone() });
                            let message = if code == errors::NO_PAIRING_CODE {
                                "This computer isn't showing a pairing code. Choose Pair New Device on it first."
                            } else {
                                "The pairing code is wrong or has expired."
                            };
                            writer.send(&ServerMessage::Error { code: code.into(), message: message.into() }).await?;
                        }
                    }
                }
                _ if !paired => {
                    writer
                        .send(&ServerMessage::Error { code: errors::NOT_PAIRED.into(), message: "This device isn't paired.".into() })
                        .await?;
                }
                ClientMessage::Hello { .. } => {}
                ClientMessage::Key { code, down, repeat } => {
                    if crate::keys::by_code(&code).is_none() {
                        writer
                            .send(&ServerMessage::Notice { code: notices::UNKNOWN_KEY.into(), message: format!("Unknown key {code}.") })
                            .await?;
                    } else {
                        let _ = shared.input.send(InputCommand::Key(code, down, repeat));
                    }
                }
                ClientMessage::Tap { code, modifiers } => {
                    let _ = shared.input.send(InputCommand::Tap(code, modifiers));
                }
                ClientMessage::Text { text } => {
                    let _ = shared.input.send(InputCommand::Text(text));
                }
                ClientMessage::ReleaseAll => {
                    let _ = shared.input.send(InputCommand::ReleaseAll);
                }
                ClientMessage::AudioStart { .. } if !audio_available(shared.session) => {
                    writer
                        .send(&ServerMessage::Error {
                            code: errors::AUDIO_FAILED.into(),
                            message: "Computer Audio at the login window needs the Portputer Microphone driver: on the Mac, Portputer Settings, Microphone, Install.".into(),
                        })
                        .await?;
                }
                ClientMessage::AudioStart { mute, turn } => {
                    audio = None;
                    let turn = match (client_ip, turn) {
                        (None, Some(peer)) => turn_to(&shared, &peer).await,
                        _ => None,
                    };
                    match shared.audio.subscribe(client_ip, client_id.clone(), mute, turn) {
                        Ok((sub, ready, mute_result)) => {
                            audio = Some(sub);
                            writer.send(&ready).await?;
                            if let Err(message) = mute_result {
                                writer.send(&ServerMessage::Error { code: errors::MUTE_FAILED.into(), message }).await?;
                            }
                        }
                        Err(e) => {
                            shared.emit(ServerEvent::AudioFailed { message: e.to_string() });
                            writer.send(&ServerMessage::Error { code: errors::AUDIO_FAILED.into(), message: e.to_string() }).await?;
                        }
                    }
                }
                ClientMessage::MuteSpeakers { muted } => {
                    let result = match &audio {
                        Some(sub) => sub.set_mute(muted),
                        None => Ok(()),
                    };
                    if let Err(message) = result {
                        writer.send(&ServerMessage::Error { code: errors::MUTE_FAILED.into(), message }).await?;
                    }
                }
                ClientMessage::MicStart { turn } => {
                    mic = None;
                    let turn = match (client_ip, turn) {
                        (None, Some(peer)) => turn_to(&shared, &peer).await,
                        _ => None,
                    };
                    match start_microphone(client_ip, &client_id, &shared.mic_inputs, turn) {
                        Ok((sink, ready)) => {
                            mic = Some(sink);
                            writer.send(&ready).await?;
                        }
                        Err(e) => {
                            writer.send(&ServerMessage::Error { code: errors::MIC_FAILED.into(), message: e.to_string() }).await?;
                        }
                    }
                }
                ClientMessage::ClipboardWatch { on } => {
                    if let Some(w) = clipboard_watch.take() {
                        w.abort();
                    }
                    if on && files_available(shared.session) {
                        clipboard_watch = Some(tokio::spawn(watch_clipboard(shared.clone(), writer.clone())));
                    }
                }
                ClientMessage::ClipboardText { text } => {
                    if files_available(shared.session) {
                        let shared = shared.clone();
                        tokio::task::spawn_blocking(move || {
                            if crate::clipboard::write_text(&text).is_ok() {
                                shared.own_clipboard.store(crate::clipboard::change_count(), Ordering::SeqCst);
                            }
                        });
                    }
                }
                ClientMessage::MicStop => {
                    mic = None;
                    writer.send(&ServerMessage::MicStopped { reason: "stopped".into() }).await?;
                }
                ClientMessage::AudioStop => {
                    audio = None;
                    writer.send(&ServerMessage::AudioStopped { reason: "stopped".into() }).await?;
                }
                ClientMessage::Power { action, user, password } => {
                    let allowed = platform::power_actions(shared.session).contains(&action);
                    let result = if !allowed {
                        Err("This computer can't do that from here.".to_string())
                    } else if action == PowerAction::Lock && crate::keys::Os::current() == crate::keys::Os::Mac {
                        let _ = shared.input.send(InputCommand::Tap("KeyQ".into(), vec!["ControlLeft".into(), "MetaLeft".into()]));
                        Ok(())
                    } else {
                        let session = shared.session;
                        tokio::task::spawn_blocking(move || platform::power(session, action, user.as_deref(), password.as_deref()))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                    };
                    if let Err(message) = result {
                        writer.send(&ServerMessage::Error { code: errors::POWER_FAILED.into(), message }).await?;
                    }
                }
            }
        }
    }
    .await;

    drop(audio);
    drop(mic);
    if let Some(w) = clipboard_watch.take() {
        w.abort();
    }
    let _ = shared.input.send(InputCommand::ReleaseAll);
    shared.clients.lock().unwrap().remove(&conn);
    shared.emit(ServerEvent::Disconnected { client_id, name });
    writer.shutdown().await;
    match result {
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
        other => other,
    }
}

fn input_notice() -> ServerMessage {
    let message = if cfg!(target_os = "macos") {
        "Portputer can't type on this Mac: it needs Accessibility permission, in System Settings, Privacy & Security, Accessibility."
    } else {
        "Portputer can't type on this computer."
    };
    ServerMessage::Notice { code: notices::INPUT_PERMISSION.into(), message: message.into() }
}

/// This computer's addresses on every network it's on: the local network's,
/// and VPNs' such as Tailscale's. Link-local addresses need a scope or are
/// meaningless elsewhere, so they're left out.
fn own_addresses() -> Vec<String> {
    let mut out: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| i.ip())
        .filter(|ip| match ip {
            std::net::IpAddr::V4(v4) => !v4.is_link_local(),
            std::net::IpAddr::V6(v6) => !v6.is_unicast_link_local(),
        })
        .map(|ip| ip.to_string())
        .collect();
    out.sort_by_key(|a| a.contains(':'));
    out.dedup();
    out.truncate(16);
    out
}

/// Clipboard and files, for a logged-in user only: the server at the login
/// window runs as root.
fn files_available(session: SessionKind) -> bool {
    session == SessionKind::User && crate::clipboard::available()
}

/// Text copied on this computer is the most that's sent in one message.
const MAX_CLIPBOARD_TEXT: usize = 60_000;

/// Sends the clipboard to a client whenever something new is copied.
async fn watch_clipboard(shared: Arc<Shared>, writer: transport::Writer) {
    let mut last = crate::clipboard::change_count();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        let count = crate::clipboard::change_count();
        if count == last {
            continue;
        }
        last = count;
        if count == shared.own_clipboard.load(Ordering::SeqCst) {
            continue;
        }
        let message = match tokio::task::spawn_blocking(crate::clipboard::read).await.ok().flatten() {
            Some(crate::clipboard::Clip::Text(text)) if text.len() <= MAX_CLIPBOARD_TEXT => ServerMessage::ClipboardText { text },
            Some(crate::clipboard::Clip::Files(paths)) => {
                let files: Vec<crate::protocol::FileInfo> = paths
                    .iter()
                    .filter_map(|p| {
                        let meta = std::fs::metadata(p).ok().filter(|m| m.is_file())?;
                        Some(crate::protocol::FileInfo { name: p.file_name()?.to_string_lossy().into_owned(), size: meta.len() })
                    })
                    .collect();
                if files.is_empty() {
                    continue;
                }
                ServerMessage::ClipboardFiles { files }
            }
            _ => continue,
        };
        if writer.send(&message).await.is_err() {
            return;
        }
    }
}

/// Whether this computer can offer a client's microphone to its apps.
fn microphone_available() -> bool {
    #[cfg(target_os = "macos")]
    return audio::feed::installed();
    #[cfg(not(target_os = "macos"))]
    false
}

type MicInputs = Mutex<HashMap<String, (PacketKey, Arc<Mutex<crate::jitter::JitterBuffer>>)>>;

/// Whether this server can send the computer's audio. At the Mac login
/// window, Core Audio taps would need the System Audio Recording
/// permission, which nobody can grant there, so the audio goes through
/// Portputer's driver instead (audio::loopback), when it's installed. The
/// test tone needs neither.
fn audio_available(session: SessionKind) -> bool {
    if cfg!(windows) || std::env::var_os("PORTPUTER_TEST_TONE").is_some() {
        return true;
    }
    #[cfg(target_os = "macos")]
    return session == SessionKind::User || audio::loopback::available();
    #[cfg(not(target_os = "macos"))]
    {
        let _ = session;
        false
    }
}

/// The computer's end of a TURN path to a relayed client's relayed
/// address: credentials from the relay, an allocation, and a channel bound
/// to the client. None when any of it fails; the client then uses a
/// datagram connection.
async fn turn_to(shared: &Arc<Shared>, peer: &str) -> Option<Arc<crate::turn::Allocation>> {
    let peer: SocketAddr = peer.parse().ok()?;
    let home = shared.store.relay_home();
    let setup = tokio::task::spawn_blocking(move || -> Result<crate::turn::Allocation, Error> {
        let credentials = crate::relay::turn_credentials_for_home(&home)?;
        let allocation = crate::turn::Allocation::new(&credentials)?;
        allocation.bind(peer)?;
        Ok(allocation)
    });
    match tokio::time::timeout(Duration::from_secs(5), setup).await {
        Ok(Ok(Ok(allocation))) => Some(Arc::new(allocation)),
        Ok(Ok(Err(e))) => {
            tracing::info!("TURN not used: {e}");
            None
        }
        _ => None,
    }
}

/// A client's microphone, played into the Portputer Microphone driver.
struct MicSink {
    stop: Arc<std::sync::atomic::AtomicBool>,
    _output: Box<dyn Send>,
    /// For a relayed client: its entry in the server's microphone inputs.
    relayed: Option<(Arc<MicInputs>, String)>,
}

impl Drop for MicSink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some((inputs, id)) = self.relayed.take() {
            inputs.lock().unwrap().remove(&id);
        }
    }
}

/// A datagram connection from a relayed client: audio packets out, or
/// microphone packets in, as data frames.
async fn serve_datagrams(shared: Arc<Shared>, mut reader: transport::Reader, writer: transport::Writer, client_id: &str, purpose: &str) -> Result<(), Error> {
    match purpose {
        "audio" => {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
            shared.audio.attach_relay(client_id, tx);
            loop {
                tokio::select! {
                    packet = rx.recv() => match packet {
                        Some(p) => writer.send_data(&p).await?,
                        None => return Ok(()),
                    },
                    frame = reader.recv_frame::<ClientMessage>() => {
                        if frame.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
        "mic" => loop {
            let transport::Frame::Data(packet) = reader.recv_frame::<ClientMessage>().await? else { continue };
            let input = shared.mic_inputs.lock().unwrap().get(client_id).cloned();
            if let Some((key, jitter)) = input {
                if let Some((udp::KIND_MIC, seq, payload)) = key.open(&packet) {
                    jitter.lock().unwrap().push(seq, udp::decode_pcm(&payload));
                }
            }
        },
        _ => Ok(()),
    }
}

/// Plays a microphone stream where this computer's apps can record it.
fn microphone_output(jitter: Arc<Mutex<crate::jitter::JitterBuffer>>) -> Result<Box<dyn Send>, Error> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(audio::feed::start(jitter)?));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = jitter;
        Err(Error::Audio("This computer has no Portputer Microphone driver.".into()))
    }
}

/// A client's microphone: over UDP from `client_ip`, or for a relayed
/// client (no address), over TURN or its datagram connection.
fn start_microphone(
    client_ip: Option<std::net::IpAddr>,
    client_id: &str,
    inputs: &Arc<MicInputs>,
    turn: Option<Arc<crate::turn::Allocation>>,
) -> Result<(MicSink, ServerMessage), Error> {
    let jitter = Arc::new(Mutex::new(crate::jitter::JitterBuffer::new(udp::MIC_RATE, 1, udp::MIC_FRAMES as u32, 0)));
    let output = microphone_output(jitter.clone())?;
    let Some(client_ip) = client_ip else {
        let (key_bytes, key) = PacketKey::random();
        // The datagram connection stays possible: the client falls back to
        // it when nothing comes over TURN.
        inputs.lock().unwrap().insert(client_id.to_string(), (key.clone(), jitter.clone()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let relayed_address = turn.as_ref().map(|t| t.relayed().to_string());
        if let Some(turn) = turn {
            let stopped = stop.clone();
            std::thread::Builder::new().name("portputer-mic-turn".into()).spawn(move || {
                // Keep-alives the other way show the client the path works.
                let mut last_keepalive = Instant::now() - Duration::from_secs(1);
                let mut seq = 0u32;
                while !stopped.load(Ordering::Relaxed) {
                    if last_keepalive.elapsed() >= Duration::from_millis(udp::KEEPALIVE_INTERVAL_MS) {
                        let _ = turn.send(&key.seal(udp::KIND_KEEPALIVE, seq, &[]));
                        seq = seq.wrapping_add(1);
                        last_keepalive = Instant::now();
                    }
                    let Some(packet) = turn.recv_timeout(Duration::from_millis(100)) else { continue };
                    if let Some((udp::KIND_MIC, seq, payload)) = key.open(&packet) {
                        jitter.lock().unwrap().push(seq, udp::decode_pcm(&payload));
                    }
                }
            })?;
        }
        let sink = MicSink { stop, _output: output, relayed: Some((inputs.clone(), client_id.to_string())) };
        return Ok((sink, ServerMessage::MicReady { port: 0, key: hex::encode(key_bytes), turn: relayed_address }));
    };
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    let port = socket.local_addr()?.port();
    let (key_bytes, key) = PacketKey::random();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    std::thread::Builder::new().name("portputer-mic-recv".into()).spawn(move || {
        let mut buf = vec![0u8; 4096];
        while !stopped.load(Ordering::Relaxed) {
            let Ok((n, from)) = socket.recv_from(&mut buf) else { continue };
            if from.ip() != client_ip {
                continue;
            }
            if let Some((udp::KIND_MIC, seq, payload)) = key.open(&buf[..n]) {
                jitter.lock().unwrap().push(seq, udp::decode_pcm(&payload));
            }
        }
    })?;
    Ok((MicSink { stop, _output: output, relayed: None }, ServerMessage::MicReady { port, key: hex::encode(key_bytes), turn: None }))
}

/// One client's audio stream: its own UDP socket, key and sequence numbers.
struct Subscriber {
    id: u64,
    socket: UdpSocket,
    key: PacketKey,
    /// None for a relayed client, whose packets go over TURN or on its
    /// datagram connection.
    client_ip: Option<std::net::IpAddr>,
    client_id: String,
    relay_tx: Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    turn: Option<Arc<crate::turn::Allocation>>,
    dest: Mutex<Option<(SocketAddr, Instant)>>,
    seq: AtomicU32,
    closed: std::sync::atomic::AtomicBool,
    /// This client wants the computer's speakers muted while it listens.
    mute: std::sync::atomic::AtomicBool,
}

struct AudioSubscription {
    hub: Arc<HubInner>,
    subscriber: Arc<Subscriber>,
}

impl Drop for AudioSubscription {
    fn drop(&mut self) {
        self.subscriber.closed.store(true, Ordering::Relaxed);
        let mut state = self.hub.state.lock().unwrap();
        state.subscribers.retain(|s| s.id != self.subscriber.id);
        // Unmuted before the capture stops, so the speakers come back.
        let _ = apply_mute(&mut state);
        if state.subscribers.is_empty() {
            state.capture = None;
        }
    }
}

impl AudioSubscription {
    fn set_mute(&self, mute: bool) -> Result<(), String> {
        self.subscriber.mute.store(mute, Ordering::Relaxed);
        apply_mute(&mut self.hub.state.lock().unwrap())
    }
}

/// The speakers are muted while anyone listening asks for it.
fn apply_mute(state: &mut HubState) -> Result<(), String> {
    let want = state.capture.is_some() && state.subscribers.iter().any(|s| s.mute.load(Ordering::Relaxed));
    if want == state.muted {
        return Ok(());
    }
    platform::set_speakers_muted(want)?;
    state.muted = want;
    Ok(())
}

#[derive(Default)]
struct HubState {
    subscribers: Vec<Arc<Subscriber>>,
    capture: Option<Capture>,
    format: (u32, u16, u32),
    next_id: u64,
    muted: bool,
}

#[derive(Default)]
struct HubInner {
    state: Mutex<HubState>,
}

/// Shares one capture among every client listening.
#[derive(Default)]
struct AudioHub(Arc<HubInner>);

impl AudioHub {
    #[allow(clippy::type_complexity)]
    fn subscribe(
        &self,
        client_ip: Option<std::net::IpAddr>,
        client_id: String,
        mute: bool,
        turn: Option<Arc<crate::turn::Allocation>>,
    ) -> Result<(AudioSubscription, ServerMessage, Result<(), String>), Error> {
        let inner = self.0.clone();
        let mut state = inner.state.lock().unwrap();
        if state.capture.is_none() {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(64);
            let capture = audio::start_capture(move |samples, channels| {
                // To stereo, 16-bit; the sending thread cuts it into packets.
                let mut out = Vec::with_capacity(samples.len() / channels.max(1) * 2);
                for frame in samples.chunks(channels.max(1)) {
                    let l = frame[0];
                    let r = if frame.len() > 1 { frame[1] } else { l };
                    out.push((l.clamp(-1.0, 1.0) * 32767.0) as i16);
                    out.push((r.clamp(-1.0, 1.0) * 32767.0) as i16);
                }
                let _ = tx.try_send(out);
            });
            let capture = capture?;
            // 5 ms packets.
            let frames_per_packet = (capture.rate / 200).max(1);
            state.format = (capture.rate, 2, frames_per_packet);
            state.capture = Some(capture);
            let hub = inner.clone();
            std::thread::Builder::new().name("portputer-audio-send".into()).spawn(move || {
                send_thread(hub, rx, frames_per_packet as usize)
            })?;
        }
        let socket = UdpSocket::bind(("0.0.0.0", 0))?;
        let port = socket.local_addr()?.port();
        let (key_bytes, key) = PacketKey::random();
        state.next_id += 1;
        let subscriber = Arc::new(Subscriber {
            id: state.next_id,
            socket,
            key,
            client_ip,
            client_id,
            relay_tx: Mutex::new(None),
            turn,
            dest: Mutex::new(None),
            seq: AtomicU32::new(0),
            closed: Default::default(),
            mute: std::sync::atomic::AtomicBool::new(mute),
        });
        state.subscribers.push(subscriber.clone());
        let mute_result = apply_mute(&mut state);
        let (rate, channels, frames) = state.format;
        let turn = subscriber.turn.as_ref().map(|t| t.relayed().to_string());
        drop(state);
        if client_ip.is_some() {
            let listener = subscriber.clone();
            std::thread::Builder::new().name("portputer-keepalive".into()).spawn(move || keepalive_thread(listener))?;
        }
        Ok((
            AudioSubscription { hub: inner, subscriber },
            ServerMessage::AudioReady { port, key: hex::encode(key_bytes), sample_rate: rate, channels, frames, turn },
            mute_result,
        ))
    }
}

impl AudioHub {
    /// Sends a relayed client's audio on its datagram connection.
    fn attach_relay(&self, client_id: &str, tx: tokio::sync::mpsc::Sender<Vec<u8>>) {
        for sub in self.0.state.lock().unwrap().subscribers.iter().filter(|s| s.client_id == client_id) {
            *sub.relay_tx.lock().unwrap() = Some(tx.clone());
        }
    }
}

/// Learns where to send a client's audio from its keep-alives.
fn keepalive_thread(sub: Arc<Subscriber>) {
    let socket = match sub.socket.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    socket.set_read_timeout(Some(Duration::from_millis(500))).ok();
    let mut buf = [0u8; 256];
    while !sub.closed.load(Ordering::Relaxed) {
        if let Ok((n, from)) = socket.recv_from(&mut buf) {
            if Some(from.ip()) != sub.client_ip {
                continue;
            }
            if let Some((udp::KIND_KEEPALIVE, _, _)) = sub.key.open(&buf[..n]) {
                *sub.dest.lock().unwrap() = Some((from, Instant::now()));
            }
        }
    }
}

fn send_thread(hub: Arc<HubInner>, rx: std::sync::mpsc::Receiver<Vec<i16>>, frames: usize) {
    let mut carry: Vec<i16> = Vec::new();
    let samples_per_packet = frames * 2;
    loop {
        // Ends when the capture stops and drops the sending side.
        let Ok(chunk) = rx.recv() else { return };
        carry.extend_from_slice(&chunk);
        let subscribers: Vec<Arc<Subscriber>> = hub.state.lock().unwrap().subscribers.clone();
        while carry.len() >= samples_per_packet {
            let packet: Vec<i16> = carry.drain(..samples_per_packet).collect();
            let payload = udp::encode_pcm(&packet);
            for sub in &subscribers {
                // The datagram connection when the client opened one (TURN
                // didn't work for it), then TURN.
                let relay = sub.relay_tx.lock().unwrap().clone();
                if let Some(tx) = relay {
                    let seq = sub.seq.fetch_add(1, Ordering::Relaxed);
                    // Dropped, not queued, when the relay falls behind.
                    let _ = tx.try_send(sub.key.seal(udp::KIND_AUDIO, seq, &payload));
                    continue;
                }
                if let Some(turn) = &sub.turn {
                    let seq = sub.seq.fetch_add(1, Ordering::Relaxed);
                    let _ = turn.send(&sub.key.seal(udp::KIND_AUDIO, seq, &payload));
                    continue;
                }
                let dest = *sub.dest.lock().unwrap();
                let Some((addr, last)) = dest else { continue };
                if last.elapsed() > Duration::from_millis(udp::KEEPALIVE_TIMEOUT_MS) {
                    continue;
                }
                let seq = sub.seq.fetch_add(1, Ordering::Relaxed);
                let _ = sub.socket.send_to(&sub.key.seal(udp::KIND_AUDIO, seq, &payload), addr);
            }
        }
    }
}
