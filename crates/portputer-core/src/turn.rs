//! A TURN client (RFC 8656) over UDP, for audio and microphone packets when
//! a device reaches its computer through the internet relay. Both ends get a
//! relayed address from Cloudflare's TURN service and bind a channel to the
//! other's, so the packets cross Cloudflare's network as UDP instead of
//! inside the relay's WebSockets, where one lost packet holds up the rest.
//!
//! The packets inside are the same encrypted ones as on a direct connection
//! (`udp`), so the TURN service sees only ciphertext.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use md5::{Digest, Md5};
use serde::Deserialize;
use sha1::Sha1;

use crate::Error;

const COOKIE: u32 = 0x2112_A442;

const ALLOCATE: u16 = 0x003;
const REFRESH: u16 = 0x004;
const CHANNEL_BIND: u16 = 0x009;
const DATA_INDICATION: u16 = 0x0017;
const SUCCESS: u16 = 0x0100;
/// The class bits: request, indication, success or error.
const CLASS: u16 = 0x0110;

const ATTR_USERNAME: u16 = 0x0006;
const ATTR_INTEGRITY: u16 = 0x0008;
const ATTR_ERROR_CODE: u16 = 0x0009;
const ATTR_CHANNEL_NUMBER: u16 = 0x000C;
const ATTR_LIFETIME: u16 = 0x000D;
const ATTR_PEER_ADDRESS: u16 = 0x0012;
const ATTR_DATA: u16 = 0x0013;
const ATTR_REALM: u16 = 0x0014;
const ATTR_NONCE: u16 = 0x0015;
const ATTR_RELAYED_ADDRESS: u16 = 0x0016;
const ATTR_REQUESTED_TRANSPORT: u16 = 0x0019;

/// The one channel each allocation binds, to the other end's address.
const CHANNEL: u16 = 0x4000;
const LIFETIME_SECS: u32 = 600;
/// Within the 300 s a permission lasts.
const REFRESH_EVERY: Duration = Duration::from_secs(240);

/// TURN credentials, from the relay (`/turn/<room>`).
#[derive(Clone, Debug, Deserialize)]
pub struct Credentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
}

impl Credentials {
    /// The UDP servers among the URLs: `turn:host:port?transport=udp`.
    fn udp_servers(&self) -> Vec<String> {
        self.urls
            .iter()
            .filter_map(|u| u.strip_prefix("turn:"))
            .filter_map(|rest| {
                let (host, query) = rest.split_once('?').unwrap_or((rest, ""));
                (query.is_empty() || query.contains("transport=udp")).then(|| host.to_string())
            })
            .collect()
    }
}

#[derive(Debug, Default)]
struct Message {
    kind: u16,
    txid: [u8; 12],
    attrs: Vec<(u16, Vec<u8>)>,
}

impl Message {
    fn request(kind: u16) -> Message {
        let mut txid = [0u8; 12];
        getrandom::fill(&mut txid).expect("random bytes");
        Message { kind, txid, attrs: vec![] }
    }

    fn attr(&self, kind: u16) -> Option<&[u8]> {
        self.attrs.iter().find(|(k, _)| *k == kind).map(|(_, v)| v.as_slice())
    }

    fn add(&mut self, kind: u16, value: Vec<u8>) {
        self.attrs.push((kind, value));
    }

    /// The bytes, with MESSAGE-INTEGRITY last when there's a key.
    fn encode(&self, key: Option<&[u8]>) -> Vec<u8> {
        let mut out = Vec::with_capacity(128);
        out.extend_from_slice(&self.kind.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&COOKIE.to_be_bytes());
        out.extend_from_slice(&self.txid);
        for (kind, value) in &self.attrs {
            out.extend_from_slice(&kind.to_be_bytes());
            out.extend_from_slice(&(value.len() as u16).to_be_bytes());
            out.extend_from_slice(value);
            out.resize(out.len().next_multiple_of(4), 0);
        }
        if let Some(key) = key {
            // The length covers the integrity attribute; the HMAC covers
            // everything before it.
            let length = (out.len() - 20 + 24) as u16;
            out[2..4].copy_from_slice(&length.to_be_bytes());
            let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(key).expect("any key length");
            mac.update(&out);
            out.extend_from_slice(&ATTR_INTEGRITY.to_be_bytes());
            out.extend_from_slice(&20u16.to_be_bytes());
            out.extend_from_slice(&mac.finalize().into_bytes());
        } else {
            let length = (out.len() - 20) as u16;
            out[2..4].copy_from_slice(&length.to_be_bytes());
        }
        out
    }

    fn decode(bytes: &[u8]) -> Option<Message> {
        if bytes.len() < 20 || bytes[0] & 0xC0 != 0 || u32::from_be_bytes(bytes[4..8].try_into().ok()?) != COOKIE {
            return None;
        }
        let length = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        let body = bytes.get(20..20 + length)?;
        let mut message = Message { kind: u16::from_be_bytes([bytes[0], bytes[1]]), txid: bytes[8..20].try_into().ok()?, attrs: vec![] };
        let mut at = 0;
        while at + 4 <= body.len() {
            let kind = u16::from_be_bytes([body[at], body[at + 1]]);
            let len = u16::from_be_bytes([body[at + 2], body[at + 3]]) as usize;
            let value = body.get(at + 4..at + 4 + len)?;
            message.attrs.push((kind, value.to_vec()));
            at += (4 + len).next_multiple_of(4);
        }
        Some(message)
    }

    fn error_code(&self) -> Option<u16> {
        self.attr(ATTR_ERROR_CODE).filter(|v| v.len() >= 4).map(|v| (v[2] & 0x7) as u16 * 100 + v[3] as u16)
    }
}

fn xor_address(address: SocketAddr, txid: &[u8; 12]) -> Vec<u8> {
    let port = address.port() ^ (COOKIE >> 16) as u16;
    let mut out = vec![0];
    match address.ip() {
        IpAddr::V4(ip) => {
            out.push(1);
            out.extend_from_slice(&port.to_be_bytes());
            out.extend_from_slice(&(u32::from(ip) ^ COOKIE).to_be_bytes());
        }
        IpAddr::V6(ip) => {
            out.push(2);
            out.extend_from_slice(&port.to_be_bytes());
            let mask: Vec<u8> = COOKIE.to_be_bytes().iter().chain(txid).copied().collect();
            out.extend(ip.octets().iter().zip(mask).map(|(a, m)| a ^ m));
        }
    }
    out
}

fn parse_xor_address(value: &[u8], txid: &[u8; 12]) -> Option<SocketAddr> {
    let port = u16::from_be_bytes([*value.get(2)?, *value.get(3)?]) ^ (COOKIE >> 16) as u16;
    match value.get(1)? {
        1 => {
            let ip = u32::from_be_bytes(value.get(4..8)?.try_into().ok()?) ^ COOKIE;
            Some(SocketAddr::new(Ipv4Addr::from(ip).into(), port))
        }
        2 => {
            let mask: Vec<u8> = COOKIE.to_be_bytes().iter().chain(txid).copied().collect();
            let octets: Vec<u8> = value.get(4..20)?.iter().zip(mask).map(|(a, m)| a ^ m).collect();
            let octets: [u8; 16] = octets.try_into().ok()?;
            Some(SocketAddr::new(Ipv6Addr::from(octets).into(), port))
        }
        _ => None,
    }
}

struct Auth {
    username: String,
    realm: Vec<u8>,
    nonce: Vec<u8>,
    key: Vec<u8>,
}

struct Shared {
    socket: UdpSocket,
    password: String,
    auth: Mutex<Auth>,
    pending: Mutex<HashMap<[u8; 12], Sender<Message>>>,
    peer: Mutex<Option<SocketAddr>>,
    closed: AtomicBool,
}

impl Shared {
    /// Sends a request and waits for its answer, resending as UDP needs;
    /// answers a 401 or a stale nonce by authenticating and trying again.
    fn request(&self, build: impl Fn() -> Message) -> Result<Message, Error> {
        for _ in 0..3 {
            let mut message = build();
            let key = {
                let auth = self.auth.lock().unwrap();
                if auth.realm.is_empty() {
                    None
                } else {
                    message.add(ATTR_USERNAME, auth.username.as_bytes().to_vec());
                    message.add(ATTR_REALM, auth.realm.clone());
                    message.add(ATTR_NONCE, auth.nonce.clone());
                    Some(auth.key.clone())
                }
            };
            let bytes = message.encode(key.as_deref());
            let (tx, rx) = mpsc::channel();
            self.pending.lock().unwrap().insert(message.txid, tx);
            let mut answer = None;
            for wait in [300u64, 600, 1200] {
                self.socket.send(&bytes)?;
                if let Ok(m) = rx.recv_timeout(Duration::from_millis(wait)) {
                    answer = Some(m);
                    break;
                }
            }
            self.pending.lock().unwrap().remove(&message.txid);
            let answer = answer.ok_or_else(|| Error::Protocol("the TURN server didn't answer".into()))?;
            if answer.kind & CLASS == SUCCESS {
                return Ok(answer);
            }
            match answer.error_code() {
                Some(401 | 438) => {
                    let realm = answer.attr(ATTR_REALM).map(<[u8]>::to_vec);
                    let nonce = answer.attr(ATTR_NONCE).map(<[u8]>::to_vec);
                    let mut auth = self.auth.lock().unwrap();
                    if let Some(realm) = realm {
                        let mut md5 = Md5::new();
                        md5.update(format!("{}:{}:{}", auth.username, String::from_utf8_lossy(&realm), self.password));
                        auth.key = md5.finalize().to_vec();
                        auth.realm = realm;
                    }
                    if let Some(nonce) = nonce {
                        auth.nonce = nonce;
                    }
                }
                code => return Err(Error::Protocol(format!("the TURN server refused: error {}", code.unwrap_or(0)))),
            }
        }
        Err(Error::Protocol("the TURN server refused the credentials".into()))
    }

    fn bind(&self, peer: SocketAddr) -> Result<(), Error> {
        self.request(|| {
            let mut m = Message::request(CHANNEL_BIND);
            m.add(ATTR_CHANNEL_NUMBER, vec![(CHANNEL >> 8) as u8, CHANNEL as u8, 0, 0]);
            let address = xor_address(peer, &m.txid);
            m.add(ATTR_PEER_ADDRESS, address);
            m
        })?;
        Ok(())
    }

    fn refresh(&self, lifetime: u32) -> Result<(), Error> {
        self.request(|| {
            let mut m = Message::request(REFRESH);
            m.add(ATTR_LIFETIME, lifetime.to_be_bytes().to_vec());
            m
        })?;
        Ok(())
    }
}

/// A relayed address at a TURN server, with a channel to one peer once
/// bound. Packets from the peer arrive on `recv_timeout`.
pub struct Allocation {
    shared: Arc<Shared>,
    relayed: SocketAddr,
    incoming: Mutex<Receiver<Vec<u8>>>,
}

impl Allocation {
    /// Allocates at the first of the credentials' UDP servers that answers.
    pub fn new(credentials: &Credentials) -> Result<Allocation, Error> {
        let mut last = Error::Protocol("no TURN server over UDP".into());
        for server in credentials.udp_servers() {
            match Allocation::at(&server, credentials) {
                Ok(a) => return Ok(a),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn at(server: &str, credentials: &Credentials) -> Result<Allocation, Error> {
        // Cloudflare's relayed addresses are IPv4.
        let address = server
            .to_socket_addrs()?
            .find(SocketAddr::is_ipv4)
            .ok_or_else(|| Error::Protocol(format!("no IPv4 address for {server}")))?;
        let socket = UdpSocket::bind(("0.0.0.0", 0))?;
        socket.connect(address)?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let shared = Arc::new(Shared {
            socket,
            password: credentials.credential.clone(),
            auth: Mutex::new(Auth { username: credentials.username.clone(), realm: vec![], nonce: vec![], key: vec![] }),
            pending: Mutex::new(HashMap::new()),
            peer: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::sync_channel(512);
        let reader = shared.clone();
        std::thread::Builder::new().name("portputer-turn".into()).spawn(move || read_loop(reader, tx))?;
        let answer = shared.request(|| {
            let mut m = Message::request(ALLOCATE);
            m.add(ATTR_REQUESTED_TRANSPORT, vec![17, 0, 0, 0]);
            m.add(ATTR_LIFETIME, LIFETIME_SECS.to_be_bytes().to_vec());
            m
        });
        let answer = match answer {
            Ok(a) => a,
            Err(e) => {
                shared.closed.store(true, Ordering::Relaxed);
                return Err(e);
            }
        };
        let relayed = answer
            .attr(ATTR_RELAYED_ADDRESS)
            .and_then(|v| parse_xor_address(v, &answer.txid))
            .ok_or_else(|| Error::Protocol("no relayed address".into()))?;
        let refresher = shared.clone();
        std::thread::Builder::new().name("portputer-turn-refresh".into()).spawn(move || refresh_loop(refresher))?;
        Ok(Allocation { shared, relayed, incoming: Mutex::new(rx) })
    }

    /// The address the peer sends to.
    pub fn relayed(&self) -> SocketAddr {
        self.relayed
    }

    /// Binds the channel to the peer's relayed address; packets from
    /// anywhere else are dropped by the server.
    pub fn bind(&self, peer: SocketAddr) -> Result<(), Error> {
        self.shared.bind(peer)?;
        *self.shared.peer.lock().unwrap() = Some(peer);
        Ok(())
    }

    /// Sends one packet to the peer, as ChannelData.
    pub fn send(&self, data: &[u8]) -> std::io::Result<usize> {
        let mut packet = Vec::with_capacity(4 + data.len());
        packet.extend_from_slice(&CHANNEL.to_be_bytes());
        packet.extend_from_slice(&(data.len() as u16).to_be_bytes());
        packet.extend_from_slice(data);
        self.shared.socket.send(&packet)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        self.incoming.lock().unwrap().recv_timeout(timeout).ok()
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
        // A zero lifetime frees it at once; no wait for the answer.
        let mut m = Message::request(REFRESH);
        m.add(ATTR_LIFETIME, vec![0, 0, 0, 0]);
        let auth = self.shared.auth.lock().unwrap();
        m.add(ATTR_USERNAME, auth.username.as_bytes().to_vec());
        m.add(ATTR_REALM, auth.realm.clone());
        m.add(ATTR_NONCE, auth.nonce.clone());
        let _ = self.shared.socket.send(&m.encode(Some(&auth.key)));
    }
}

fn read_loop(shared: Arc<Shared>, tx: SyncSender<Vec<u8>>) {
    let mut buf = vec![0u8; 4096];
    while !shared.closed.load(Ordering::Relaxed) {
        let Ok(n) = shared.socket.recv(&mut buf) else { continue };
        let packet = &buf[..n];
        if n >= 4 && packet[0] & 0xC0 == 0x40 {
            // ChannelData.
            let len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
            if let Some(data) = packet.get(4..4 + len) {
                let _ = tx.try_send(data.to_vec());
            }
            continue;
        }
        let Some(message) = Message::decode(packet) else { continue };
        if message.kind == DATA_INDICATION {
            if let Some(data) = message.attr(ATTR_DATA) {
                let _ = tx.try_send(data.to_vec());
            }
        } else if let Some(waiting) = shared.pending.lock().unwrap().remove(&message.txid) {
            let _ = waiting.send(message);
        }
    }
}

fn refresh_loop(shared: Arc<Shared>) {
    let mut waited = Duration::ZERO;
    while !shared.closed.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        waited += Duration::from_millis(500);
        if waited < REFRESH_EVERY {
            continue;
        }
        waited = Duration::ZERO;
        if let Err(e) = shared.refresh(LIFETIME_SECS) {
            tracing::warn!("TURN refresh: {e}");
        }
        let peer = *shared.peer.lock().unwrap();
        if let Some(peer) = peer {
            if let Err(e) = shared.bind(peer) {
                tracing::warn!("TURN channel refresh: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_mapped_address_from_rfc_5769() {
        // RFC 5769, 2.2: 192.0.2.1 port 32853.
        let txid = [0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae];
        let value = [0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43];
        let address: SocketAddr = "192.0.2.1:32853".parse().unwrap();
        assert_eq!(parse_xor_address(&value, &txid), Some(address));
        assert_eq!(xor_address(address, &txid), value);
        let v6: SocketAddr = "[2001:db8:1234:5678:11:2233:4455:6677]:32853".parse().unwrap();
        assert_eq!(parse_xor_address(&xor_address(v6, &txid), &txid), Some(v6));
    }

    #[test]
    fn messages_round_trip_with_integrity() {
        let mut m = Message::request(ALLOCATE);
        m.add(ATTR_REQUESTED_TRANSPORT, vec![17, 0, 0, 0]);
        m.add(ATTR_USERNAME, b"user".to_vec());
        let bytes = m.encode(Some(b"key"));
        assert_eq!(bytes.len() % 4, 0);
        let back = Message::decode(&bytes).unwrap();
        assert_eq!(back.kind, ALLOCATE);
        assert_eq!(back.attr(ATTR_USERNAME), Some(&b"user"[..]));
        let integrity = back.attr(ATTR_INTEGRITY).unwrap();
        let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(b"key").unwrap();
        mac.update(&bytes[..bytes.len() - 24]);
        assert_eq!(integrity, &mac.finalize().into_bytes()[..]);
    }

    #[test]
    fn udp_servers() {
        let c = Credentials {
            urls: vec![
                "stun:stun.cloudflare.com:3478".into(),
                "turn:turn.cloudflare.com:3478?transport=udp".into(),
                "turn:turn.cloudflare.com:3478?transport=tcp".into(),
                "turns:turn.cloudflare.com:5349?transport=tcp".into(),
                "turn:turn.cloudflare.com:53?transport=udp".into(),
            ],
            username: String::new(),
            credential: String::new(),
        };
        assert_eq!(c.udp_servers(), ["turn.cloudflare.com:3478", "turn.cloudflare.com:53"]);
    }
}
