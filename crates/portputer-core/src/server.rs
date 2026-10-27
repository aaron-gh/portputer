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
}

impl Shared {
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
        });
        if !platform::input_allowed(session == SessionKind::User) {
            shared.emit(ServerEvent::InputNotAllowed);
        }
        let accept = shared.clone();
        runtime.spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, address)) => {
                        let shared = accept.clone();
                        tokio::spawn(async move {
                            if let Err(e) = serve(shared, stream, address).await {
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
        Ok(Server { shared, runtime: Some(runtime), port, _announcement: announcement })
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

    pub fn unpair(&self, id: &str) -> Result<(), Error> {
        self.shared.store.remove_paired(id)
    }

    pub fn connected(&self) -> Vec<ConnectedClient> {
        self.shared.clients.lock().unwrap().values().cloned().collect()
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
    while let Ok(command) = commands.recv() {
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

async fn serve(shared: Arc<Shared>, stream: tokio::net::TcpStream, address: SocketAddr) -> Result<(), Error> {
    let hs = tokio::time::timeout(Duration::from_secs(10), transport::respond(stream, &shared.identity.private))
        .await
        .map_err(|_| Error::Protocol("handshake timed out".into()))??;
    let client_key = hs.remote_static;
    let client_id = id_of(&client_key);
    let (name, protocol) = match serde_json::from_slice::<ClientMessage>(&hs.payload) {
        Ok(ClientMessage::Hello { name, protocol }) => (name, protocol),
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
        ConnectedClient { id: client_id.clone(), name: name.clone(), address: address.ip().to_string(), paired },
    );
    shared.emit(ServerEvent::Connected { client_id: client_id.clone(), name: name.clone(), address: address.ip().to_string() });

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
            audio: cfg!(any(target_os = "macos", windows)),
            mute: platform::can_mute_speakers(),
        })
        .await?;
    if paired && !platform::input_allowed(false) {
        writer.send(&input_notice()).await?;
    }

    let mut audio: Option<AudioSubscription> = None;
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
                ClientMessage::AudioStart { mute } => {
                    audio = None;
                    match shared.audio.subscribe(address.ip(), mute) {
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

/// One client's audio stream: its own UDP socket, key and sequence numbers.
struct Subscriber {
    id: u64,
    socket: UdpSocket,
    key: PacketKey,
    client_ip: std::net::IpAddr,
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
    fn subscribe(&self, client_ip: std::net::IpAddr, mute: bool) -> Result<(AudioSubscription, ServerMessage, Result<(), String>), Error> {
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
            dest: Mutex::new(None),
            seq: AtomicU32::new(0),
            closed: Default::default(),
            mute: std::sync::atomic::AtomicBool::new(mute),
        });
        state.subscribers.push(subscriber.clone());
        let mute_result = apply_mute(&mut state);
        let (rate, channels, frames) = state.format;
        drop(state);
        let listener = subscriber.clone();
        std::thread::Builder::new().name("portputer-keepalive".into()).spawn(move || keepalive_thread(listener))?;
        Ok((
            AudioSubscription { hub: inner, subscriber },
            ServerMessage::AudioReady { port, key: hex::encode(key_bytes), sample_rate: rate, channels, frames },
            mute_result,
        ))
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
            if from.ip() != sub.client_ip {
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
