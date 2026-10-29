//! The client: connects to a server, pairs with it once, sends keys and
//! commands, plays its sound, and reconnects by itself when the connection
//! drops, as it does when the computer restarts or someone logs in.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::Error;
use crate::audio::{self, Playback};
#[cfg(not(target_os = "ios"))]
use crate::discovery::Browser;
use crate::discovery::{BrowseEvent, ServerAd};
use crate::identity::{ClientStore, Identity, KnownServer, constant_time_eq, id_of, now_secs, proof};
use crate::jitter::{JitterBuffer, JitterStats};
use crate::keys::Os;
use crate::protocol::{ClientMessage, PROTOCOL_VERSION, PowerAction, ServerMessage, SessionKind, errors};
use crate::transport::{self, Reader, Writer};
use crate::udp::{self, PacketKey};

/// How long the client keeps trying to get back to a server, long enough for
/// a restart with updates.
const RECONNECT_FOR: Duration = Duration::from_secs(20 * 60);
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
const PING_EVERY: Duration = Duration::from_secs(5);
const SILENCE_LIMIT: Duration = Duration::from_secs(15);

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("portputer-client")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    pub os: Os,
    pub session: SessionKind,
    pub user: String,
    pub version: String,
    pub power: Vec<PowerAction>,
    pub audio: bool,
    /// The computer's speakers can be muted while it streams.
    pub mute: bool,
    /// The computer offers this device's microphone to its apps.
    pub mic: bool,
    /// Clipboard and file transfer are available.
    pub files: bool,
    /// Connected through the internet relay; `host` and `port` are empty.
    pub via_relay: bool,
    pub host: String,
    pub port: u16,
    /// Paired both ways: keys and commands are allowed.
    pub trusted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionState {
    Connected(ServerInfo),
    /// The connection dropped; trying to get back.
    Reconnecting,
    Closed { reason: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    State(SessionState),
    Notice { code: String, message: String },
    Error { code: String, message: String },
    AudioStarted { sample_rate: u32 },
    AudioStopped { reason: String },
    MicStarted,
    MicStopped { reason: String },
    /// Text copied on the computer.
    ClipboardText { text: String },
    /// Files copied on the computer; [`Session::clipboard_files`] fetches them.
    ClipboardFiles { files: Vec<crate::protocol::FileInfo> },
    /// Something the client did with its own clipboard, for the person.
    ClipboardSynced { message: String },
}

pub trait SessionObserver: Send + Sync {
    fn event(&self, event: SessionEvent);
}

pub struct ClientConfig {
    pub store_dir: PathBuf,
    /// This device's name, as the server lists it.
    pub name: String,
    pub observer: Arc<dyn SessionObserver>,
    /// Keeps this computer's own clipboard in step with the server's, files
    /// included (the Mac and Windows apps). Android handles its clipboard in
    /// the app, through the clipboard events and methods.
    pub local_clipboard: bool,
}

enum Command {
    Send(ClientMessage),
    UseRelay(bool),
    Pair(String, std::sync::mpsc::Sender<Result<(), String>>),
    Close,
}

struct AudioSettings {
    volume: f32,
    latency_ms: u32,
    mute_speakers: bool,
}

struct Inner {
    tx: mpsc::UnboundedSender<Command>,
    info: Mutex<Option<ServerInfo>>,
    settings: Mutex<AudioSettings>,
    jitter: Mutex<Option<Arc<Mutex<JitterBuffer>>>>,
    identity: Identity,
    name: String,
    store_dir: PathBuf,
}

/// A connection to one server, kept up until closed.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

struct Conn {
    reader: Reader,
    writer: Writer,
    hash: Vec<u8>,
    public_key: [u8; 32],
    info: ServerInfo,
    /// None through the relay.
    peer_ip: Option<IpAddr>,
}

/// Servers seen on the network, by ID, for reconnecting at new addresses.
fn seen() -> &'static Mutex<HashMap<String, Vec<ServerAd>>> {
    static SEEN: OnceLock<Mutex<HashMap<String, Vec<ServerAd>>>> = OnceLock::new();
    let map = SEEN.get_or_init(Default::default);
    // On iOS, multicast sockets need an entitlement, so the app browses with
    // Bonjour and passes what it finds to note_seen.
    #[cfg(not(target_os = "ios"))]
    {
        static BROWSER: OnceLock<Option<Browser>> = OnceLock::new();
        BROWSER.get_or_init(|| Browser::start(note_seen).ok());
    }
    map
}

/// A server found or lost on the network, for reconnecting at new addresses.
pub fn note_seen(event: BrowseEvent) {
    let mut map = seen().lock().unwrap();
    match event {
        BrowseEvent::Found(ad) => {
            let list = map.entry(ad.id.clone()).or_default();
            list.retain(|a| a.fullname != ad.fullname);
            list.push(ad);
        }
        BrowseEvent::Lost(fullname) => map.values_mut().for_each(|l| l.retain(|a| a.fullname != fullname)),
    }
}

/// Where a server is reached: on the network, or through the internet relay.
#[derive(Clone, Debug, PartialEq)]
pub enum Endpoint {
    Tcp(String, u16),
    Relay(crate::relay::RelayAccess),
}

async fn connect_once(
    endpoint: &Endpoint,
    expected: Option<&str>,
    identity: &Identity,
    name: &str,
    store: &ClientStore,
    transfer: bool,
    datagram: Option<&str>,
) -> Result<Conn, Error> {
    let (stream, peer_ip, host, port): (transport::BoxStream, Option<IpAddr>, String, u16) = match endpoint {
        Endpoint::Tcp(host, port) => {
            let addr = tokio::net::lookup_host((host.as_str(), *port))
                .await?
                .next()
                .ok_or_else(|| Error::Protocol(format!("no address for {host}")))?;
            let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
                .await
                .map_err(|_| Error::Protocol(format!("{host} didn't answer")))??;
            stream.set_nodelay(true)?;
            (Box::new(stream), Some(addr.ip()), host.clone(), *port)
        }
        Endpoint::Relay(relay) => (crate::relay::connect(relay).await?, None, String::new(), 0),
    };
    let hello = serde_json::to_vec(&ClientMessage::Hello {
        name: name.to_string(),
        protocol: PROTOCOL_VERSION,
        transfer,
        datagram: datagram.map(String::from),
    })?;
    let hs = tokio::time::timeout(Duration::from_secs(10), transport::initiate(stream, &identity.private, &hello))
        .await
        .map_err(|_| Error::Protocol("the handshake timed out".into()))??;
    let id = id_of(&hs.remote_static);
    if let Some(expected) = expected {
        if expected != id {
            return Err(Error::WrongServer);
        }
    }
    let mut reader = hs.reader;
    let welcome: ServerMessage = tokio::time::timeout(Duration::from_secs(10), reader.recv())
        .await
        .map_err(|_| Error::Protocol("no welcome from the server".into()))??;
    let ServerMessage::Welcome { server_id, name: server_name, os, session, user, paired, version, power, audio, mute, mic, files } = welcome else {
        if let ServerMessage::Error { message, .. } = welcome {
            return Err(Error::Refused(message));
        }
        return Err(Error::Protocol("unexpected first message".into()));
    };
    if server_id != id {
        return Err(Error::Protocol("the server's ID doesn't match its key".into()));
    }
    let pinned = store.server(&id).is_some_and(|s| s.public_key == hs.remote_static);
    Ok(Conn {
        reader,
        writer: hs.writer,
        hash: hs.hash,
        public_key: hs.remote_static,
        info: ServerInfo { id, name: server_name, os, session, user, version, power, audio, mute, mic, files, host, port, via_relay: peer_ip.is_none(), trusted: pinned && paired },
        peer_ip,
    })
}

impl Session {
    /// Connects, waiting for the server's welcome. If `info().trusted` is
    /// false, [`Session::pair`] must succeed before anything else is sent.
    pub fn connect(host: &str, port: u16, expected_id: Option<String>, config: ClientConfig) -> Result<Session, Error> {
        let store = ClientStore::new(&config.store_dir);
        let identity = store.identity()?;
        let _ = seen();
        // The network first, then the relay the computer told us of.
        let relay = expected_id.as_deref().and_then(|id| store.server(id)).and_then(|s| s.relay);
        // Every address it's had, at once: the one given, then those the
        // computer told us of, such as its Tailscale address.
        let mut addresses: Vec<(String, u16)> = vec![];
        if !host.is_empty() {
            addresses.push((host.to_string(), port));
        }
        if let Some(known) = expected_id.as_deref().and_then(|id| store.server(id)) {
            if !known.host.is_empty() {
                addresses.push((known.host.clone(), known.port));
            }
            addresses.extend(known.addresses.iter().cloned());
        }
        dedup_in_order(&mut addresses);
        let direct = runtime().block_on(connect_direct(addresses, expected_id.clone(), identity.clone(), config.name.clone(), config.store_dir.clone()));
        let conn = match (direct, relay) {
            (Ok(conn), _) => conn,
            (Err(e), Some(relay)) => runtime()
                .block_on(connect_once(&Endpoint::Relay(relay), expected_id.as_deref(), &identity, &config.name, &store, false, None))
                .map_err(|relay_error| match e {
                    Error::Protocol(ref m) if m == "no address" => relay_error,
                    e => Error::Refused(format!("Not reachable directly ({e}), and not through the relay: {relay_error}")),
                })?,
            (Err(e), None) => return Err(e),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            tx,
            info: Mutex::new(Some(conn.info.clone())),
            settings: Mutex::new(AudioSettings { volume: 1.0, latency_ms: 0, mute_speakers: false }),
            jitter: Mutex::new(None),
            identity: identity.clone(),
            name: config.name.clone(),
            store_dir: config.store_dir.clone(),
        });
        if conn.info.trusted && !conn.info.via_relay {
            let _ = store.update(&conn.info.id, |s| {
                s.last_connected = now_secs();
                s.host = host.to_string();
                s.port = port;
            });
        }
        let session = Session { inner };
        let task = Supervisor {
            inner: session.inner.clone(),
            observer: config.observer,
            store,
            identity,
            name: config.name,
            audio_wanted: false,
            mic_wanted: false,
            clipboard_wanted: config.local_clipboard,
            local_clipboard: config.local_clipboard,
            own_clipboard: Arc::new(std::sync::atomic::AtomicI64::new(-1)),
            use_relay: false,
            turn_failed: false,
        };
        runtime().spawn(task.run(conn, rx));
        Ok(session)
    }

    pub fn info(&self) -> Option<ServerInfo> {
        self.inner.info.lock().unwrap().clone()
    }

    fn send(&self, message: ClientMessage) {
        let _ = self.inner.tx.send(Command::Send(message));
    }

    pub fn key(&self, code: &str, down: bool, repeat: bool) {
        self.send(ClientMessage::Key { code: code.to_string(), down, repeat });
    }

    pub fn tap(&self, code: &str, modifiers: Vec<String>) {
        self.send(ClientMessage::Tap { code: code.to_string(), modifiers });
    }

    pub fn text(&self, text: &str) {
        // In pieces that fit a message.
        let mut piece = String::new();
        for c in text.chars() {
            if piece.len() + c.len_utf8() > 8000 {
                self.send(ClientMessage::Text { text: std::mem::take(&mut piece) });
            }
            piece.push(c);
        }
        if !piece.is_empty() {
            self.send(ClientMessage::Text { text: piece });
        }
    }

    pub fn release_all(&self) {
        self.send(ClientMessage::ReleaseAll);
    }

    pub fn power(&self, action: PowerAction, user: Option<String>, password: Option<String>) {
        self.send(ClientMessage::Power { action, user, password });
    }

    pub fn start_audio(&self) {
        let mute = self.inner.settings.lock().unwrap().mute_speakers;
        self.send(ClientMessage::AudioStart { mute, turn: None });
    }

    /// Keeps the sound from the computer's own speakers while it streams here.
    pub fn set_speakers_muted(&self, muted: bool) {
        self.inner.settings.lock().unwrap().mute_speakers = muted;
        self.send(ClientMessage::MuteSpeakers { muted });
    }

    /// Sends this device's microphone to the computer.
    pub fn start_mic(&self) {
        self.send(ClientMessage::MicStart { turn: None });
    }

    pub fn stop_mic(&self) {
        self.send(ClientMessage::MicStop);
    }

    /// Has the server send its clipboard as it changes, as
    /// [`SessionEvent::ClipboardText`] and [`SessionEvent::ClipboardFiles`].
    pub fn set_clipboard_watch(&self, on: bool) {
        self.send(ClientMessage::ClipboardWatch { on });
    }

    /// Puts text on the server's clipboard.
    pub fn send_clipboard_text(&self, text: &str) {
        self.send(ClientMessage::ClipboardText { text: text.to_string() });
    }

    /// Lists a folder on the server; empty is the home folder.
    pub fn list_files(&self, path: &str) -> Result<crate::files::TransferReply, Error> {
        let path = path.to_string();
        runtime().block_on(with_transfer(self.inner.clone(), move |mut t| async move { t.list(&path).await }))
    }

    /// Downloads a file from the server into `dest`, returning where it went.
    pub fn download(&self, path: &str, dest: &std::path::Path, progress: impl Fn(u64, u64) + Send + Sync + 'static) -> Result<PathBuf, Error> {
        let (path, dest) = (path.to_string(), dest.to_path_buf());
        runtime().block_on(with_transfer(self.inner.clone(), move |mut t| async move { t.download(&path, &dest, &progress).await }))
    }

    /// Uploads a file to the server: to its Downloads folder, or with
    /// `clipboard`, to its clipboard with the other files of that copy.
    /// `name` replaces the file's own name.
    pub fn upload(
        &self,
        path: &std::path::Path,
        name: Option<String>,
        clipboard: Option<u64>,
        progress: impl Fn(u64, u64) + Send + Sync + 'static,
    ) -> Result<String, Error> {
        let path = path.to_path_buf();
        runtime().block_on(with_transfer(self.inner.clone(), move |mut t| async move {
            t.upload(&path, name.as_deref(), clipboard, &progress).await
        }))
    }

    /// Fetches the files on the server's clipboard into `dest`.
    pub fn clipboard_files(&self, dest: &std::path::Path, progress: impl Fn(u64, u64) + Send + Sync + 'static) -> Result<Vec<PathBuf>, Error> {
        let dest = dest.to_path_buf();
        runtime().block_on(with_transfer(self.inner.clone(), move |mut t| async move { t.clipboard_files(&dest, &progress).await }))
    }

    pub fn stop_audio(&self) {
        self.send(ClientMessage::AudioStop);
    }

    pub fn set_volume(&self, volume: f32) {
        self.inner.settings.lock().unwrap().volume = volume;
        if let Some(j) = self.inner.jitter.lock().unwrap().as_ref() {
            j.lock().unwrap().gain = volume;
        }
    }

    /// Milliseconds of audio held to absorb network jitter; 0 is automatic.
    pub fn set_latency_ms(&self, ms: u32) {
        self.inner.settings.lock().unwrap().latency_ms = ms;
        if let Some(j) = self.inner.jitter.lock().unwrap().as_ref() {
            j.lock().unwrap().set_target_ms(ms);
        }
    }

    /// Connects through the internet relay even when the computer can be
    /// reached directly (on), or moves back to a direct connection (off).
    /// Off for each new session.
    pub fn set_use_relay(&self, on: bool) {
        let _ = self.inner.tx.send(Command::UseRelay(on));
    }

    /// Whether this computer can be reached through the relay.
    pub fn relay_available(&self) -> bool {
        let Some(id) = self.inner.info.lock().unwrap().as_ref().map(|i| i.id.clone()) else { return false };
        ClientStore::new(&self.inner.store_dir).server(&id).is_some_and(|s| s.relay.is_some())
    }

    pub fn audio_stats(&self) -> Option<JitterStats> {
        self.inner.jitter.lock().unwrap().as_ref().map(|j| j.lock().unwrap().stats)
    }

    /// Pairs with the code the server shows. Blocks until the server answers.
    pub fn pair(&self, code: &str) -> Result<(), Error> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.inner.tx.send(Command::Pair(code.to_string(), tx)).map_err(|_| Error::Closed)?;
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(Error::Refused(message)),
            Err(_) => Err(Error::Closed),
        }
    }

    pub fn close(&self) {
        let _ = self.inner.tx.send(Command::Close);
    }
}

struct AudioReceiver {
    stop: Arc<AtomicBool>,
    _playback: Playback,
    /// The relay's datagram connection, when there is one.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for AudioReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

struct Supervisor {
    inner: Arc<Inner>,
    observer: Arc<dyn SessionObserver>,
    store: ClientStore,
    identity: Identity,
    name: String,
    audio_wanted: bool,
    mic_wanted: bool,
    clipboard_wanted: bool,
    local_clipboard: bool,
    /// This computer's clipboard change count after Portputer wrote it.
    own_clipboard: Arc<std::sync::atomic::AtomicI64>,
    /// Use Internet Relay: stay on the relay, even with a direct address.
    use_relay: bool,
    /// TURN didn't work in this session: the relay's datagram connections
    /// carry audio and the microphone instead.
    turn_failed: bool,
}

/// Nothing arrived over TURN in time.
enum TurnFallback {
    Audio,
    Mic,
}

enum Ended {
    Closed,
    Lost(String),
    Refused(String),
    /// Another connection to the same computer to use in its place: direct
    /// in place of relayed, or relayed when Use Internet Relay is turned on.
    Switch(Box<Conn>),
}

impl Supervisor {
    fn emit(&self, event: SessionEvent) {
        self.observer.event(event);
    }

    async fn run(mut self, first: Conn, mut rx: mpsc::UnboundedReceiver<Command>) {
        let mut conn = first;
        loop {
            let info = conn.info.clone();
            *self.inner.info.lock().unwrap() = Some(info.clone());
            self.emit(SessionEvent::State(SessionState::Connected(info.clone())));
            match self.serve(conn, &mut rx).await {
                Ended::Closed => {
                    self.emit(SessionEvent::State(SessionState::Closed { reason: "Disconnected.".into() }));
                    return;
                }
                Ended::Refused(reason) => {
                    self.emit(SessionEvent::State(SessionState::Closed { reason }));
                    return;
                }
                Ended::Switch(other) => {
                    tracing::info!("connection moved to {}", if other.info.via_relay { "the relay" } else { &other.info.host });
                    conn = *other;
                    continue;
                }
                Ended::Lost(reason) => {
                    tracing::info!("connection lost: {reason}");
                    if !self.inner.info.lock().unwrap().as_ref().is_some_and(|i| i.trusted) {
                        self.emit(SessionEvent::State(SessionState::Closed { reason }));
                        return;
                    }
                }
            }
            self.emit(SessionEvent::State(SessionState::Reconnecting));
            match self.reconnect(&info, &mut rx).await {
                Some(c) => conn = c,
                None => return,
            }
        }
    }

    /// Candidates to try: the server's current announcements (the user
    /// session first), then where it was, then the relay.
    fn candidates(&self, info: &ServerInfo) -> Vec<Endpoint> {
        let known = self.store.server(&info.id);
        let mut out: Vec<(String, u16)> = vec![];
        if let Some(ads) = seen().lock().unwrap().get(&info.id) {
            let mut ads = ads.clone();
            ads.sort_by_key(|a| a.session != SessionKind::User);
            for ad in ads {
                for address in &ad.addresses {
                    out.push((address.clone(), ad.port));
                }
            }
        }
        let (host, port) = if info.via_relay {
            known.as_ref().map(|k| (k.host.clone(), k.port)).unwrap_or_default()
        } else {
            (info.host.clone(), info.port)
        };
        if !host.is_empty() {
            out.push((host.clone(), port));
            let other = if port == crate::protocol::LOGIN_WINDOW_PORT { crate::protocol::DEFAULT_PORT } else { crate::protocol::LOGIN_WINDOW_PORT };
            out.push((host, other));
        }
        // The addresses the computer told us of, on both servers' ports: after
        // a restart, the one at the login window answers.
        if let Some(k) = &known {
            for (h, p) in &k.addresses {
                out.push((h.clone(), *p));
                out.push((h.clone(), if *p == crate::protocol::LOGIN_WINDOW_PORT { crate::protocol::DEFAULT_PORT } else { crate::protocol::LOGIN_WINDOW_PORT }));
            }
        }
        dedup_in_order(&mut out);
        let mut endpoints: Vec<Endpoint> = out.into_iter().map(|(h, p)| Endpoint::Tcp(h, p)).collect();
        if let Some(relay) = known.and_then(|k| k.relay) {
            endpoints.push(Endpoint::Relay(relay));
        }
        endpoints
    }

    async fn reconnect(&mut self, info: &ServerInfo, rx: &mut mpsc::UnboundedReceiver<Command>) -> Option<Conn> {
        let started = Instant::now();
        while started.elapsed() < RECONNECT_FOR {
            let (direct, relayed): (Vec<Endpoint>, Vec<Endpoint>) = self.candidates(info).into_iter().partition(|e| matches!(e, Endpoint::Tcp(..)));
            let addresses: Vec<(String, u16)> = direct.into_iter().filter_map(|e| if let Endpoint::Tcp(h, p) = e { Some((h, p)) } else { None }).collect();
            // Every direct address at once, then the relay; the relay first
            // with Use Internet Relay on.
            let mut order: Vec<Option<Endpoint>> = std::iter::once(None).chain(relayed.into_iter().map(Some)).collect();
            if self.use_relay {
                order.rotate_left(1);
            }
            for endpoint in order {
                // Anything asked for meanwhile is dropped: keys typed into a
                // computer that isn't there shouldn't arrive later.
                while let Ok(command) = rx.try_recv() {
                    match command {
                        Command::Close => {
                            self.emit(SessionEvent::State(SessionState::Closed { reason: "Disconnected.".into() }));
                            return None;
                        }
                        Command::Pair(_, reply) => {
                            let _ = reply.send(Err("Not connected.".into()));
                        }
                        Command::Send(_) => {}
                        Command::UseRelay(on) => self.use_relay = on,
                    }
                }
                let attempt = match &endpoint {
                    None => {
                        if addresses.is_empty() {
                            continue;
                        }
                        connect_direct(addresses.clone(), Some(info.id.clone()), self.identity.clone(), self.name.clone(), self.inner.store_dir.clone()).await
                    }
                    Some(relay) => tokio::time::timeout(Duration::from_secs(12), connect_once(relay, Some(&info.id), &self.identity, &self.name, &self.store, false, None))
                        .await
                        .unwrap_or_else(|_| Err(Error::Protocol("timed out".into()))),
                };
                if let Ok(conn) = attempt {
                    if conn.info.trusted {
                        if !conn.info.via_relay {
                            let (host, port) = (conn.info.host.clone(), conn.info.port);
                            let _ = self.store.update(&conn.info.id, |s| {
                                s.last_connected = now_secs();
                                s.host = host;
                                s.port = port;
                            });
                        }
                        return Some(conn);
                    }
                }
            }
            tokio::time::sleep(RECONNECT_EVERY).await;
        }
        self.emit(SessionEvent::State(SessionState::Closed { reason: "The computer didn't come back.".into() }));
        None
    }

    async fn serve(&mut self, conn: Conn, rx: &mut mpsc::UnboundedReceiver<Command>) -> Ended {
        let Conn { mut reader, writer, hash, public_key, mut info, peer_ip } = conn;
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Result<ServerMessage, Error>>();
        let read_task = tokio::spawn(async move {
            loop {
                let m = reader.recv::<ServerMessage>().await;
                let failed = m.is_err();
                if msg_tx.send(m).is_err() || failed {
                    break;
                }
            }
        });
        let mut audio: Option<AudioReceiver> = None;
        let mut mic: Option<MicSender> = None;
        let mut pending_pair: Option<(String, std::sync::mpsc::Sender<Result<(), String>>)> = None;
        let mut ping = tokio::time::interval(PING_EVERY);
        // Through the relay, the computer's direct addresses are tried now
        // and then, and the connection moves to one that answers.
        let (upgrade_tx, mut upgrade_rx) = mpsc::channel::<Conn>(1);
        let mut upgrade: Option<tokio::task::JoinHandle<()>> = None;
        let mut upgrade_timer = tokio::time::interval(Duration::from_secs(60));
        let mut last_heard = Instant::now();
        let mut n = 0u64;
        // Through the relay: TURN allocations waiting for the server's
        // answer, and word when nothing comes over TURN.
        let mut audio_turn: Option<Arc<crate::turn::Allocation>> = None;
        let mut mic_turn: Option<Arc<crate::turn::Allocation>> = None;
        let (fallback_tx, mut fallback_rx) = mpsc::unbounded_channel::<TurnFallback>();
        // Set while audio restarts without TURN, so it isn't announced twice.
        let mut audio_restarting = false;
        if info.trusted && self.audio_wanted {
            let _ = self.send_audio_start(&info, &writer, &mut audio_turn).await;
        }
        if info.trusted && self.mic_wanted {
            let _ = self.send_mic_start(&info, &writer, &mut mic_turn).await;
        }
        let mut local_watch: Option<tokio::task::JoinHandle<()>> = None;
        if info.trusted && info.files && self.clipboard_wanted {
            let _ = writer.send(&ClientMessage::ClipboardWatch { on: true }).await;
            if self.local_clipboard {
                local_watch = Some(tokio::spawn(watch_local_clipboard(self.inner.clone(), writer.clone(), self.own_clipboard.clone(), self.observer.clone())));
            }
        }
        let ended = loop {
            tokio::select! {
                command = rx.recv() => {
                    let Some(command) = command else { break Ended::Closed };
                    match command {
                        Command::Close => {
                            let _ = writer.send(&ClientMessage::ReleaseAll).await;
                            break Ended::Closed;
                        }
                        Command::Pair(code, reply) => {
                            let message = ClientMessage::Pair { proof: hex::encode(proof(&code, "client", &hash)) };
                            if writer.send(&message).await.is_err() {
                                let _ = reply.send(Err("Not connected.".into()));
                            } else {
                                pending_pair = Some((code, reply));
                            }
                        }
                        Command::UseRelay(on) => {
                            self.use_relay = on;
                            if !info.trusted || on == info.via_relay {
                                continue;
                            }
                            if let Some(t) = upgrade.take() {
                                t.abort();
                            }
                            upgrade = Some(if on { self.switch_to_relay(&info, upgrade_tx.clone()) } else { self.try_direct(&info, upgrade_tx.clone()) });
                        }
                        Command::Send(message) => {
                            if !info.trusted {
                                continue;
                            }
                            let sent = match message {
                                ClientMessage::AudioStart { .. } => {
                                    self.audio_wanted = true;
                                    self.send_audio_start(&info, &writer, &mut audio_turn).await
                                }
                                ClientMessage::MicStart { .. } => {
                                    self.mic_wanted = true;
                                    self.send_mic_start(&info, &writer, &mut mic_turn).await
                                }
                                message => {
                                    match &message {
                                        ClientMessage::ClipboardWatch { on } => self.clipboard_wanted = *on,
                                        ClientMessage::MicStop => {
                                            self.mic_wanted = false;
                                            mic = None;
                                        }
                                        ClientMessage::AudioStop => {
                                            self.audio_wanted = false;
                                            audio = None;
                                            *self.inner.jitter.lock().unwrap() = None;
                                        }
                                        _ => {}
                                    }
                                    writer.send(&message).await
                                }
                            };
                            if let Err(e) = sent {
                                break Ended::Lost(e.to_string());
                            }
                        }
                    }
                }
                message = msg_rx.recv() => {
                    last_heard = Instant::now();
                    let message = match message {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => break Ended::Lost(e.to_string()),
                        None => break Ended::Lost("connection closed".into()),
                    };
                    match message {
                        ServerMessage::Paired { proof: server_proof } => {
                            let Some((code, reply)) = pending_pair.take() else { continue };
                            let expected = proof(&code, "server", &hash);
                            let genuine = hex::decode(&server_proof).is_ok_and(|p| constant_time_eq(&p, &expected));
                            if !genuine {
                                let _ = reply.send(Err("The computer couldn't prove it knows the code.".into()));
                                break Ended::Refused("Pairing failed: the computer's proof was wrong.".into());
                            }
                            let known = KnownServer {
                                id: info.id.clone(),
                                name: info.name.clone(),
                                public_key,
                                os: format!("{:?}", info.os).to_lowercase(),
                                host: info.host.clone(),
                                port: info.port,
                                macs: seen().lock().unwrap().get(&info.id).and_then(|a| a.first().map(|a| a.macs.clone())).unwrap_or_default(),
                                last_connected: now_secs(),
                                addresses: vec![],
                                relay: None,
                            };
                            if let Err(e) = self.store.upsert(known) {
                                let _ = reply.send(Err(e.to_string()));
                                continue;
                            }
                            info.trusted = true;
                            *self.inner.info.lock().unwrap() = Some(info.clone());
                            let _ = reply.send(Ok(()));
                            self.emit(SessionEvent::State(SessionState::Connected(info.clone())));
                        }
                        ServerMessage::Error { code, message } => {
                            if (code == errors::PAIRING_FAILED || code == errors::NO_PAIRING_CODE) && pending_pair.is_some() {
                                let (_, reply) = pending_pair.take().unwrap();
                                let _ = reply.send(Err(message));
                            } else if code == errors::VERSION {
                                break Ended::Refused(message);
                            } else {
                                if code == errors::AUDIO_FAILED {
                                    self.audio_wanted = false;
                                }
                                if code == errors::MIC_FAILED {
                                    self.mic_wanted = false;
                                }
                                self.emit(SessionEvent::Error { code, message });
                            }
                        }
                        ServerMessage::Notice { code, message } => self.emit(SessionEvent::Notice { code, message }),
                        ServerMessage::AudioReady { port, key, sample_rate, channels, frames, turn } => {
                            audio = None;
                            let turn = match (turn.and_then(|t| t.parse::<SocketAddr>().ok()), audio_turn.take()) {
                                (Some(peer), Some(allocation)) if peer_ip.is_none() => self.bind_turn(allocation, peer).await,
                                _ => None,
                            };
                            let started = match (peer_ip, turn) {
                                (Some(ip), _) => self.start_audio(ip, port, &key, sample_rate, channels, frames),
                                (None, Some(turn)) => self.start_audio_turn(turn, &key, sample_rate, channels, frames, fallback_tx.clone()),
                                (None, None) => self.start_audio_relayed(&info, &key, sample_rate, channels, frames),
                            };
                            match started {
                                Ok(a) => {
                                    audio = Some(a);
                                    if !std::mem::take(&mut audio_restarting) {
                                        self.emit(SessionEvent::AudioStarted { sample_rate });
                                    }
                                }
                                Err(e) => {
                                    let _ = writer.send(&ClientMessage::AudioStop).await;
                                    self.audio_wanted = false;
                                    self.emit(SessionEvent::Error { code: errors::AUDIO_FAILED.into(), message: e.to_string() });
                                }
                            }
                        }
                        ServerMessage::MicReady { port, key, turn } => {
                            mic = None;
                            let turn = match (turn.and_then(|t| t.parse::<SocketAddr>().ok()), mic_turn.take()) {
                                (Some(peer), Some(allocation)) if peer_ip.is_none() => self.bind_turn(allocation, peer).await,
                                _ => None,
                            };
                            let started = match (peer_ip, turn) {
                                (Some(ip), _) => start_mic_sender(ip, port, &key),
                                (None, Some(turn)) => start_mic_turn(turn, &key, fallback_tx.clone()),
                                (None, None) => self.start_mic_relayed(&info, &key),
                            };
                            match started {
                                Ok(m) => {
                                    mic = Some(m);
                                    self.emit(SessionEvent::MicStarted);
                                }
                                Err(e) => {
                                    let _ = writer.send(&ClientMessage::MicStop).await;
                                    self.mic_wanted = false;
                                    self.emit(SessionEvent::Error { code: errors::MIC_FAILED.into(), message: e.to_string() });
                                }
                            }
                        }
                        ServerMessage::Addresses { addresses, port } => {
                            if info.trusted {
                                let _ = self.store.update(&info.id, |s| remember_addresses(s, &addresses, port));
                                if info.via_relay && !self.use_relay && upgrade.as_ref().is_none_or(|t| t.is_finished()) {
                                    upgrade = Some(self.try_direct(&info, upgrade_tx.clone()));
                                }
                            }
                        }
                        ServerMessage::Relay { url, room, token } => {
                            if info.trusted {
                                let relay = crate::relay::RelayAccess { url, room, token };
                                let _ = self.store.update(&info.id, |s| s.relay = Some(relay));
                            }
                        }
                        ServerMessage::ClipboardText { text } => {
                            if self.local_clipboard {
                                let own = self.own_clipboard.clone();
                                tokio::task::spawn_blocking(move || {
                                    if crate::clipboard::write_text(&text).is_ok() {
                                        own.store(crate::clipboard::change_count(), std::sync::atomic::Ordering::SeqCst);
                                    }
                                });
                            } else {
                                self.emit(SessionEvent::ClipboardText { text });
                            }
                        }
                        ServerMessage::ClipboardFiles { files } => {
                            if self.local_clipboard {
                                tokio::spawn(paste_server_files(self.inner.clone(), files, self.own_clipboard.clone(), self.observer.clone()));
                            } else {
                                self.emit(SessionEvent::ClipboardFiles { files });
                            }
                        }
                        ServerMessage::MicStopped { reason } => {
                            if mic.take().is_some() {
                                self.emit(SessionEvent::MicStopped { reason });
                            }
                        }
                        ServerMessage::AudioStopped { reason } => {
                            audio = None;
                            *self.inner.jitter.lock().unwrap() = None;
                            self.emit(SessionEvent::AudioStopped { reason });
                        }
                        ServerMessage::Pong { .. } | ServerMessage::Welcome { .. } => {}
                    }
                }
                Some(other) = upgrade_rx.recv() => {
                    // One that no longer matches Use Internet Relay is dropped.
                    if other.info.via_relay == self.use_relay {
                        break Ended::Switch(Box::new(other));
                    }
                }
                Some(what) = fallback_rx.recv() => {
                    tracing::info!("nothing came over TURN; using the relay's datagram connections");
                    self.turn_failed = true;
                    let restarted = match what {
                        TurnFallback::Audio if self.audio_wanted && audio.is_some() => {
                            audio = None;
                            *self.inner.jitter.lock().unwrap() = None;
                            audio_restarting = true;
                            self.send_audio_start(&info, &writer, &mut audio_turn).await
                        }
                        TurnFallback::Mic if self.mic_wanted && mic.is_some() => {
                            mic = None;
                            self.send_mic_start(&info, &writer, &mut mic_turn).await
                        }
                        _ => Ok(()),
                    };
                    if let Err(e) = restarted {
                        break Ended::Lost(e.to_string());
                    }
                }
                _ = upgrade_timer.tick(), if info.via_relay && info.trusted && !self.use_relay => {
                    if upgrade.as_ref().is_none_or(|t| t.is_finished()) {
                        upgrade = Some(self.try_direct(&info, upgrade_tx.clone()));
                    }
                }
                _ = ping.tick() => {
                    if last_heard.elapsed() > SILENCE_LIMIT {
                        break Ended::Lost("the computer stopped responding".into());
                    }
                    n += 1;
                    if let Err(e) = writer.send(&ClientMessage::Ping { n }).await {
                        break Ended::Lost(e.to_string());
                    }
                }
            }
        };
        if let Some((_, reply)) = pending_pair.take() {
            let _ = reply.send(Err("The connection closed.".into()));
        }
        if let Some(t) = upgrade.take() {
            t.abort();
        }
        if mic.take().is_some() {
            self.emit(SessionEvent::MicStopped { reason: "disconnected".into() });
        }
        if let Some(w) = local_watch.take() {
            w.abort();
        }
        let had_audio = audio.take().is_some();
        *self.inner.jitter.lock().unwrap() = None;
        if had_audio {
            self.emit(SessionEvent::AudioStopped { reason: "disconnected".into() });
        }
        read_task.abort();
        writer.shutdown().await;
        ended
    }

    fn start_audio(&self, server: IpAddr, port: u16, key: &str, rate: u32, channels: u16, frames: u32) -> Result<AudioReceiver, Error> {
        let key: [u8; 32] = hex::decode(key)
            .ok()
            .and_then(|k| k.try_into().ok())
            .ok_or_else(|| Error::Protocol("bad audio key".into()))?;
        let key = PacketKey::new(&key);
        let bind: IpAddr = if server.is_ipv6() { "::".parse().unwrap() } else { "0.0.0.0".parse().unwrap() };
        let socket = UdpSocket::bind((bind, 0))?;
        socket.connect((server, port))?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let (volume, latency) = {
            let s = self.inner.settings.lock().unwrap();
            (s.volume, s.latency_ms)
        };
        let mut buffer = JitterBuffer::new(rate, channels, frames, latency);
        buffer.gain = volume;
        let jitter = Arc::new(Mutex::new(buffer));
        let stop = Arc::new(AtomicBool::new(false));

        let send_socket = socket.try_clone()?;
        let send_key = key.clone();
        let send_stop = stop.clone();
        std::thread::Builder::new().name("portputer-keepalive".into()).spawn(move || {
            let seq = AtomicU32::new(0);
            while !send_stop.load(Ordering::Relaxed) {
                let packet = send_key.seal(udp::KIND_KEEPALIVE, seq.fetch_add(1, Ordering::Relaxed), &[]);
                let _ = send_socket.send(&packet);
                std::thread::sleep(Duration::from_millis(udp::KEEPALIVE_INTERVAL_MS));
            }
        })?;

        let recv_jitter = jitter.clone();
        let recv_stop = stop.clone();
        std::thread::Builder::new().name("portputer-audio-recv".into()).spawn(move || {
            let mut buf = vec![0u8; 8192];
            while !recv_stop.load(Ordering::Relaxed) {
                let Ok(n) = socket.recv(&mut buf) else { continue };
                if let Some((udp::KIND_AUDIO, seq, payload)) = key.open(&buf[..n]) {
                    recv_jitter.lock().unwrap().push(seq, udp::decode_pcm(&payload));
                }
            }
        })?;

        let playback = audio::start_playback(jitter.clone())?;
        *self.inner.jitter.lock().unwrap() = Some(jitter);
        Ok(AudioReceiver { stop, _playback: playback, task: None })
    }

    /// Through the relay: a TURN allocation for audio or the microphone,
    /// unless TURN already failed in this session or the relay has none.
    async fn client_turn(&mut self, info: &ServerInfo) -> Option<Arc<crate::turn::Allocation>> {
        if !info.via_relay || self.turn_failed {
            return None;
        }
        let access = self.store.server(&info.id)?.relay?;
        let setup = tokio::task::spawn_blocking(move || -> Result<crate::turn::Allocation, Error> {
            crate::turn::Allocation::new(&crate::relay::turn_credentials(&access)?)
        });
        match tokio::time::timeout(Duration::from_secs(5), setup).await {
            Ok(Ok(Ok(allocation))) => Some(Arc::new(allocation)),
            other => {
                if let Ok(Ok(Err(e))) = other {
                    tracing::info!("TURN not used: {e}");
                }
                self.turn_failed = true;
                None
            }
        }
    }

    /// Binds a TURN allocation's channel to the server's relayed address.
    async fn bind_turn(&mut self, allocation: Arc<crate::turn::Allocation>, peer: SocketAddr) -> Option<Arc<crate::turn::Allocation>> {
        let bound = allocation.clone();
        match tokio::task::spawn_blocking(move || bound.bind(peer)).await {
            Ok(Ok(())) => Some(allocation),
            _ => {
                self.turn_failed = true;
                None
            }
        }
    }

    async fn send_audio_start(&mut self, info: &ServerInfo, writer: &Writer, pending: &mut Option<Arc<crate::turn::Allocation>>) -> Result<(), Error> {
        let mute = self.inner.settings.lock().unwrap().mute_speakers;
        *pending = self.client_turn(info).await;
        let turn = pending.as_ref().map(|a| a.relayed().to_string());
        writer.send(&ClientMessage::AudioStart { mute, turn }).await
    }

    async fn send_mic_start(&mut self, info: &ServerInfo, writer: &Writer, pending: &mut Option<Arc<crate::turn::Allocation>>) -> Result<(), Error> {
        *pending = self.client_turn(info).await;
        let turn = pending.as_ref().map(|a| a.relayed().to_string());
        writer.send(&ClientMessage::MicStart { turn }).await
    }

    /// Audio over TURN, with keep-alives back so the path stays open. When
    /// nothing arrives within 3 seconds, `fallback` hears of it.
    fn start_audio_turn(
        &self,
        turn: Arc<crate::turn::Allocation>,
        key: &str,
        rate: u32,
        channels: u16,
        frames: u32,
        fallback: mpsc::UnboundedSender<TurnFallback>,
    ) -> Result<AudioReceiver, Error> {
        let key = PacketKey::new(&hex::decode(key).ok().and_then(|k| k.try_into().ok()).ok_or_else(|| Error::Protocol("bad audio key".into()))?);
        let (volume, latency) = {
            let s = self.inner.settings.lock().unwrap();
            (s.volume, s.latency_ms)
        };
        let mut buffer = JitterBuffer::new(rate, channels, frames, latency);
        buffer.gain = volume;
        let jitter = Arc::new(Mutex::new(buffer));
        let stop = Arc::new(AtomicBool::new(false));
        let arrived = Arc::new(AtomicBool::new(false));
        let (keepalive_turn, keepalive_key, keepalive_stop) = (turn.clone(), key.clone(), stop.clone());
        std::thread::Builder::new().name("portputer-keepalive".into()).spawn(move || {
            let mut seq = 0u32;
            while !keepalive_stop.load(Ordering::Relaxed) {
                let _ = keepalive_turn.send(&keepalive_key.seal(udp::KIND_KEEPALIVE, seq, &[]));
                seq = seq.wrapping_add(1);
                std::thread::sleep(Duration::from_millis(udp::KEEPALIVE_INTERVAL_MS));
            }
        })?;
        let (recv_jitter, recv_stop, recv_arrived) = (jitter.clone(), stop.clone(), arrived.clone());
        std::thread::Builder::new().name("portputer-audio-turn".into()).spawn(move || {
            while !recv_stop.load(Ordering::Relaxed) {
                let Some(packet) = turn.recv_timeout(Duration::from_millis(200)) else { continue };
                if let Some((udp::KIND_AUDIO, seq, payload)) = key.open(&packet) {
                    recv_arrived.store(true, Ordering::Relaxed);
                    recv_jitter.lock().unwrap().push(seq, udp::decode_pcm(&payload));
                }
            }
        })?;
        let playback = audio::start_playback(jitter.clone())?;
        *self.inner.jitter.lock().unwrap() = Some(jitter);
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep(TURN_GRACE).await;
            if !arrived.load(Ordering::Relaxed) {
                let _ = fallback.send(TurnFallback::Audio);
            }
        });
        Ok(AudioReceiver { stop, _playback: playback, task: Some(watchdog) })
    }

    /// Connects through the relay in the background, for Use Internet
    /// Relay; the connection is sent on `found`.
    fn switch_to_relay(&self, info: &ServerInfo, found: mpsc::Sender<Conn>) -> tokio::task::JoinHandle<()> {
        let endpoint = self.relay_endpoint(info);
        let (id, identity, name, store_dir, observer) = (info.id.clone(), self.identity.clone(), self.name.clone(), self.inner.store_dir.clone(), self.observer.clone());
        tokio::spawn(async move {
            let attempt = match endpoint {
                Ok(endpoint) => {
                    let store = ClientStore::new(&store_dir);
                    tokio::time::timeout(Duration::from_secs(12), connect_once(&endpoint, Some(&id), &identity, &name, &store, false, None))
                        .await
                        .unwrap_or_else(|_| Err(Error::Protocol("the relay didn't answer".into())))
                }
                Err(e) => Err(e),
            };
            match attempt {
                Ok(conn) if conn.info.trusted => {
                    let _ = found.send(conn).await;
                }
                Ok(_) => {}
                Err(e) => observer.event(SessionEvent::Error {
                    code: errors::RELAY_FAILED.into(),
                    message: format!("Couldn't connect through the internet relay. {e}"),
                }),
            }
        })
    }

    /// Tries the computer's direct addresses in the background; a trusted
    /// connection that answers is sent on `found`.
    fn try_direct(&self, info: &ServerInfo, found: mpsc::Sender<Conn>) -> tokio::task::JoinHandle<()> {
        let addresses: Vec<(String, u16)> = self
            .candidates(info)
            .into_iter()
            .filter_map(|e| if let Endpoint::Tcp(h, p) = e { Some((h, p)) } else { None })
            .collect();
        let (id, identity, name, store_dir) = (info.id.clone(), self.identity.clone(), self.name.clone(), self.inner.store_dir.clone());
        // PORTPUTER_STAY_ON_RELAY keeps tests on the relay.
        let stay = std::env::var_os("PORTPUTER_STAY_ON_RELAY").is_some();
        tokio::spawn(async move {
            if addresses.is_empty() || stay {
                return;
            }
            if let Ok(conn) = connect_direct(addresses, Some(id), identity, name, store_dir).await {
                if conn.info.trusted {
                    let _ = found.send(conn).await;
                }
            }
        })
    }

    /// The relay this server is reached through.
    fn relay_endpoint(&self, info: &ServerInfo) -> Result<Endpoint, Error> {
        self.store
            .server(&info.id)
            .and_then(|s| s.relay)
            .map(Endpoint::Relay)
            .ok_or_else(|| Error::Protocol("no relay for this computer".into()))
    }

    /// Audio through the relay: packets in data frames on a datagram
    /// connection of their own, in place of UDP.
    fn start_audio_relayed(&self, info: &ServerInfo, key: &str, rate: u32, channels: u16, frames: u32) -> Result<AudioReceiver, Error> {
        let key = PacketKey::new(&hex::decode(key).ok().and_then(|k| k.try_into().ok()).ok_or_else(|| Error::Protocol("bad audio key".into()))?);
        let (volume, latency) = {
            let s = self.inner.settings.lock().unwrap();
            (s.volume, s.latency_ms)
        };
        let mut buffer = JitterBuffer::new(rate, channels, frames, latency);
        buffer.gain = volume;
        let jitter = Arc::new(Mutex::new(buffer));
        let endpoint = self.relay_endpoint(info)?;
        let (id, identity, name, store_dir) = (info.id.clone(), self.identity.clone(), self.name.clone(), self.inner.store_dir.clone());
        let push = jitter.clone();
        let task = runtime().spawn(async move {
            let store = ClientStore::new(&store_dir);
            let Ok(mut conn) = connect_once(&endpoint, Some(&id), &identity, &name, &store, false, Some("audio")).await else { return };
            while let Ok(frame) = conn.reader.recv_frame::<ServerMessage>().await {
                if let transport::Frame::Data(packet) = frame {
                    if let Some((udp::KIND_AUDIO, seq, payload)) = key.open(&packet) {
                        push.lock().unwrap().push(seq, udp::decode_pcm(&payload));
                    }
                }
            }
        });
        let playback = audio::start_playback(jitter.clone())?;
        *self.inner.jitter.lock().unwrap() = Some(jitter);
        Ok(AudioReceiver { stop: Default::default(), _playback: playback, task: Some(task) })
    }

    /// The microphone through the relay, on a datagram connection.
    fn start_mic_relayed(&self, info: &ServerInfo, key: &str) -> Result<MicSender, Error> {
        let key = PacketKey::new(&hex::decode(key).ok().and_then(|k| k.try_into().ok()).ok_or_else(|| Error::Protocol("bad microphone key".into()))?);
        let endpoint = self.relay_endpoint(info)?;
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (id, identity, name, store_dir) = (info.id.clone(), self.identity.clone(), self.name.clone(), self.inner.store_dir.clone());
        let task = runtime().spawn(async move {
            let store = ClientStore::new(&store_dir);
            let Ok(conn) = connect_once(&endpoint, Some(&id), &identity, &name, &store, false, Some("mic")).await else { return };
            while let Some(packet) = rx.recv().await {
                if conn.writer.send_data(&packet).await.is_err() {
                    return;
                }
            }
        });
        let capture = record_mic(key, move |packet| {
            let _ = tx.send(packet.to_vec());
        })?;
        Ok(MicSender { _capture: capture, task: Some(task), stop: None })
    }
}

/// The paired servers this client knows.
pub fn known_servers(store_dir: &std::path::Path) -> Vec<KnownServer> {
    ClientStore::new(store_dir).servers()
}

pub fn forget_server(store_dir: &std::path::Path, id: &str) -> Result<(), Error> {
    ClientStore::new(store_dir).forget(id)
}

/// How long audio or the microphone waits for the first packet over TURN
/// before using the relay's datagram connections instead.
const TURN_GRACE: Duration = Duration::from_secs(3);

/// This device's microphone, sent to the computer as it's recorded.
struct MicSender {
    _capture: audio::Capture,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Over TURN: stops the thread watching for the server's keep-alives.
    stop: Option<Arc<AtomicBool>>,
}

impl Drop for MicSender {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Records the microphone into 5 ms packets for `send`.
fn record_mic(key: PacketKey, send: impl Fn(&[u8]) + Send + 'static) -> Result<audio::Capture, Error> {
    let mut resample = audio::ToMono48k::default();
    let mut pending: Vec<f32> = Vec::new();
    let mut seq = 0u32;
    audio::start_input(move |samples, channels, rate| {
        resample.push(samples, channels, rate, &mut pending);
        while pending.len() >= udp::MIC_FRAMES {
            let frame: Vec<i16> = pending.drain(..udp::MIC_FRAMES).map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
            send(&key.seal(udp::KIND_MIC, seq, &udp::encode_pcm(&frame)));
            seq = seq.wrapping_add(1);
        }
    })
}

/// The microphone over TURN. The server's keep-alives show the path works;
/// without one within 3 seconds, `fallback` hears of it.
fn start_mic_turn(turn: Arc<crate::turn::Allocation>, key: &str, fallback: mpsc::UnboundedSender<TurnFallback>) -> Result<MicSender, Error> {
    let key = PacketKey::new(&hex::decode(key).ok().and_then(|k| k.try_into().ok()).ok_or_else(|| Error::Protocol("bad microphone key".into()))?);
    let stop = Arc::new(AtomicBool::new(false));
    let arrived = Arc::new(AtomicBool::new(false));
    let (watch_turn, watch_key, watch_stop, watch_arrived) = (turn.clone(), key.clone(), stop.clone(), arrived.clone());
    std::thread::Builder::new().name("portputer-mic-turn".into()).spawn(move || {
        while !watch_stop.load(Ordering::Relaxed) {
            let Some(packet) = watch_turn.recv_timeout(Duration::from_millis(200)) else { continue };
            if let Some((udp::KIND_KEEPALIVE, _, _)) = watch_key.open(&packet) {
                watch_arrived.store(true, Ordering::Relaxed);
            }
        }
    })?;
    let capture = record_mic(key, move |packet| {
        let _ = turn.send(packet);
    })?;
    let watchdog = runtime().spawn(async move {
        tokio::time::sleep(TURN_GRACE).await;
        if !arrived.load(Ordering::Relaxed) {
            let _ = fallback.send(TurnFallback::Mic);
        }
    });
    Ok(MicSender { _capture: capture, task: Some(watchdog), stop: Some(stop) })
}

fn start_mic_sender(server: IpAddr, port: u16, key: &str) -> Result<MicSender, Error> {
    let key: [u8; 32] = hex::decode(key)
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| Error::Protocol("bad microphone key".into()))?;
    let key = PacketKey::new(&key);
    let bind: IpAddr = if server.is_ipv6() { "::".parse().unwrap() } else { "0.0.0.0".parse().unwrap() };
    let socket = UdpSocket::bind((bind, 0))?;
    socket.connect((server, port))?;
    socket.set_nonblocking(true)?;
    let capture = record_mic(key, move |packet| {
        let _ = socket.send(packet);
    })?;
    Ok(MicSender { _capture: capture, task: None, stop: None })
}

/// Opens a transfer connection to the connected server and runs `op` on it.
async fn with_transfer<T, F, Fut>(inner: Arc<Inner>, op: F) -> Result<T, Error>
where
    F: FnOnce(crate::files::TransferClient) -> Fut,
    Fut: std::future::Future<Output = Result<T, Error>>,
{
    let info = inner.info.lock().unwrap().clone().ok_or(Error::Closed)?;
    if !info.trusted {
        return Err(Error::Refused("Not paired.".into()));
    }
    let store = ClientStore::new(&inner.store_dir);
    let endpoint = if info.via_relay {
        Endpoint::Relay(store.server(&info.id).and_then(|s| s.relay).ok_or_else(|| Error::Protocol("no relay for this computer".into()))?)
    } else {
        Endpoint::Tcp(info.host.clone(), info.port)
    };
    let conn = connect_once(&endpoint, Some(&info.id), &inner.identity, &inner.name, &store, true, None).await?;
    if !conn.info.trusted {
        return Err(Error::Refused("Not paired.".into()));
    }
    op(crate::files::TransferClient { reader: conn.reader, writer: conn.writer }).await
}

/// Where files copied on the server go on this computer before they're put
/// on its clipboard.
fn local_clipboard_dir() -> PathBuf {
    crate::files::clipboard_dir(crate::identity::now_secs())
}

/// Files copied on the server, put on this computer's clipboard.
async fn paste_server_files(
    inner: Arc<Inner>,
    files: Vec<crate::protocol::FileInfo>,
    own: Arc<std::sync::atomic::AtomicI64>,
    observer: Arc<dyn SessionObserver>,
) {
    let dest = local_clipboard_dir();
    let result = with_transfer(inner, move |mut t| async move { t.clipboard_files(&dest, &|_, _| {}).await }).await;
    let message = match result {
        Ok(paths) if !paths.is_empty() => {
            let refs: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
            match crate::clipboard::write_files(&refs) {
                Ok(()) => {
                    own.store(crate::clipboard::change_count(), std::sync::atomic::Ordering::SeqCst);
                    format!("Copied {} from the computer.", count_files(files.len()))
                }
                Err(e) => format!("Copying files failed: {e}"),
            }
        }
        Ok(_) => return,
        Err(e) => format!("Copying files failed: {e}"),
    };
    observer.event(SessionEvent::ClipboardSynced { message });
}

pub fn count_files(n: usize) -> String {
    if n == 1 { "1 file".into() } else { format!("{n} files") }
}

/// This computer's clipboard, sent to the server whenever something new is
/// copied: text as a message, files uploaded to the server's clipboard.
async fn watch_local_clipboard(inner: Arc<Inner>, writer: Writer, own: Arc<std::sync::atomic::AtomicI64>, observer: Arc<dyn SessionObserver>) {
    use std::sync::atomic::Ordering;
    let mut last = crate::clipboard::change_count();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        let count = crate::clipboard::change_count();
        if count == last {
            continue;
        }
        last = count;
        if count == own.load(Ordering::SeqCst) {
            continue;
        }
        match tokio::task::spawn_blocking(crate::clipboard::read).await.ok().flatten() {
            Some(crate::clipboard::Clip::Text(text)) => {
                if writer.send(&ClientMessage::ClipboardText { text }).await.is_err() {
                    return;
                }
            }
            Some(crate::clipboard::Clip::Files(paths)) => {
                let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
                if paths.is_empty() {
                    continue;
                }
                let batch = count as u64 ^ crate::identity::now_secs();
                let n = paths.len();
                let result = with_transfer(inner.clone(), move |mut t| async move {
                    for p in &paths {
                        t.upload(p, None, Some(batch), &|_, _| {}).await?;
                    }
                    Ok(())
                })
                .await;
                let message = match result {
                    Ok(()) => format!("Copied {} to the computer.", count_files(n)),
                    Err(e) => format!("Copying files failed: {e}"),
                };
                observer.event(SessionEvent::ClipboardSynced { message });
            }
            None => {}
        }
    }
}

fn dedup_in_order<T: PartialEq + Clone>(items: &mut Vec<T>) {
    let mut seen: Vec<T> = Vec::with_capacity(items.len());
    items.retain(|i| {
        if seen.contains(i) {
            false
        } else {
            seen.push(i.clone());
            true
        }
    });
}

/// Keeps a server's addresses from its `addresses` message, newest first.
fn remember_addresses(server: &mut KnownServer, addresses: &[String], port: u16) {
    let mut all: Vec<(String, u16)> = addresses.iter().map(|a| (a.clone(), port)).collect();
    all.extend(server.addresses.drain(..));
    dedup_in_order(&mut all);
    all.truncate(24);
    server.addresses = all;
}

/// Tries every address at once; the first to answer wins and the rest are
/// stopped. Without addresses, fails with "no address".
async fn connect_direct(addresses: Vec<(String, u16)>, expected: Option<String>, identity: Identity, name: String, store_dir: PathBuf) -> Result<Conn, Error> {
    let mut attempts = tokio::task::JoinSet::new();
    for (host, port) in addresses {
        let (expected, identity, name, store_dir) = (expected.clone(), identity.clone(), name.clone(), store_dir.clone());
        attempts.spawn(async move {
            let store = ClientStore::new(&store_dir);
            tokio::time::timeout(Duration::from_secs(8), connect_once(&Endpoint::Tcp(host.clone(), port), expected.as_deref(), &identity, &name, &store, false, None))
                .await
                .unwrap_or_else(|_| Err(Error::Protocol(format!("{host} didn't answer"))))
        });
    }
    let mut last = Error::Protocol("no address".into());
    while let Some(result) = attempts.join_next().await {
        match result {
            Ok(Ok(conn)) => {
                attempts.abort_all();
                return Ok(conn);
            }
            // A different computer answering is the most telling failure.
            Ok(Err(e)) => {
                if !matches!(last, Error::WrongServer) {
                    last = e;
                }
            }
            Err(_) => {}
        }
    }
    Err(last)
}

/// Whether a paired computer can be reached now, and how.
#[derive(Clone, Debug, PartialEq)]
pub enum Reachability {
    /// At one of its addresses; `tailscale` when that's a Tailscale address.
    Direct { host: String, tailscale: bool },
    /// Through the internet relay.
    Relay,
    NotReachable,
}

fn is_tailscale(host: &str) -> bool {
    // 100.64.0.0/10, and Tailscale's IPv6 range.
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]),
        Ok(IpAddr::V6(v6)) => v6.segments()[0] == 0xfd7a && v6.segments()[1] == 0x115c,
        Err(_) => false,
    }
}

/// Checks whether a paired computer can be reached, without connecting to
/// it: a TCP connection to each address it's had, all at once, then the
/// relay's word on whether it's listening there. Nothing reaches the
/// computer beyond a connection opened and closed.
pub fn probe(store_dir: &std::path::Path, id: &str) -> Reachability {
    let Some(known) = ClientStore::new(store_dir).server(id) else { return Reachability::NotReachable };
    let mut addresses: Vec<(String, u16)> = vec![];
    // Each address on both servers' ports: at the login window, only the
    // login window's server answers.
    let other = |p: u16| if p == crate::protocol::LOGIN_WINDOW_PORT { crate::protocol::DEFAULT_PORT } else { crate::protocol::LOGIN_WINDOW_PORT };
    if !known.host.is_empty() {
        addresses.push((known.host.clone(), known.port));
        addresses.push((known.host.clone(), other(known.port)));
    }
    for (h, p) in &known.addresses {
        addresses.push((h.clone(), *p));
        addresses.push((h.clone(), other(*p)));
    }
    dedup_in_order(&mut addresses);
    let direct = runtime().block_on(async move {
        let mut attempts = tokio::task::JoinSet::new();
        for (host, port) in addresses {
            attempts.spawn(async move {
                let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
                match tokio::time::timeout(Duration::from_millis(2500), TcpStream::connect(&target)).await {
                    Ok(Ok(_)) => Some(host),
                    _ => None,
                }
            });
        }
        while let Some(result) = attempts.join_next().await {
            if let Ok(Some(host)) = result {
                attempts.abort_all();
                return Some(host);
            }
        }
        None
    });
    if let Some(host) = direct {
        let tailscale = is_tailscale(&host);
        return Reachability::Direct { host, tailscale };
    }
    match known.relay.as_ref().map(crate::relay::status) {
        Some(Ok(true)) => Reachability::Relay,
        _ => Reachability::NotReachable,
    }
}
