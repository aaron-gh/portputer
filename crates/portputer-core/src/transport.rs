//! The control connection: TCP, a Noise XX handshake, then JSON messages in
//! Noise transport messages, each sent as a 16-bit big-endian length and the
//! ciphertext.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;
use snow::{Builder, HandshakeState, StatelessTransportState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use crate::Error;

pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"portputer 1";
const MAX_MESSAGE: usize = 65535;
/// Room left for the AEAD tag in each transport message.
pub const MAX_PAYLOAD: usize = MAX_MESSAGE - 16;

pub fn builder() -> Builder<'static> {
    Builder::new(NOISE_PARAMS.parse().expect("valid Noise parameters"))
}

pub struct Handshaken {
    pub reader: Reader,
    pub writer: Writer,
    /// The other side's static public key.
    pub remote_static: [u8; 32],
    /// The handshake hash, which pairing proofs are bound to.
    pub hash: Vec<u8>,
    /// The payload of the last handshake message.
    pub payload: Vec<u8>,
}

async fn read_frame(stream: &mut (impl AsyncReadExt + Unpin)) -> Result<Vec<u8>, Error> {
    let len = stream.read_u16().await? as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_frame(stream: &mut (impl AsyncWriteExt + Unpin), bytes: &[u8]) -> Result<(), Error> {
    let mut frame = Vec::with_capacity(bytes.len() + 2);
    frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    frame.extend_from_slice(bytes);
    stream.write_all(&frame).await?;
    Ok(())
}

fn finish(stream: TcpStream, hs: HandshakeState, payload: Vec<u8>) -> Result<Handshaken, Error> {
    let remote_static: [u8; 32] = hs
        .get_remote_static()
        .and_then(|k| k.try_into().ok())
        .ok_or(Error::Protocol("no static key from the other side".into()))?;
    let hash = hs.get_handshake_hash().to_vec();
    let state = Arc::new(hs.into_stateless_transport_mode()?);
    let (read, write) = stream.into_split();
    Ok(Handshaken {
        reader: Reader { stream: read, state: state.clone(), nonce: 0 },
        writer: Writer { stream: Arc::new(tokio::sync::Mutex::new(write)), state, nonce: Arc::new(AtomicU64::new(0)) },
        remote_static,
        hash,
        payload,
    })
}

/// The client's side: -> e; <- e, ee, s, es; -> s, se with `payload`.
pub async fn initiate(mut stream: TcpStream, private_key: &[u8; 32], payload: &[u8]) -> Result<Handshaken, Error> {
    stream.set_nodelay(true)?;
    let mut hs = builder().local_private_key(private_key)?.prologue(PROLOGUE)?.build_initiator()?;
    let mut buf = vec![0u8; MAX_MESSAGE];
    let n = hs.write_message(&[], &mut buf)?;
    write_frame(&mut stream, &buf[..n]).await?;
    let msg = read_frame(&mut stream).await?;
    let mut scratch = vec![0u8; MAX_MESSAGE];
    hs.read_message(&msg, &mut scratch)?;
    let n = hs.write_message(payload, &mut buf)?;
    write_frame(&mut stream, &buf[..n]).await?;
    finish(stream, hs, Vec::new())
}

/// The server's side of [`initiate`].
pub async fn respond(mut stream: TcpStream, private_key: &[u8; 32]) -> Result<Handshaken, Error> {
    stream.set_nodelay(true)?;
    let mut hs = builder().local_private_key(private_key)?.prologue(PROLOGUE)?.build_responder()?;
    let mut buf = vec![0u8; MAX_MESSAGE];
    let msg = read_frame(&mut stream).await?;
    hs.read_message(&msg, &mut buf)?;
    let n = hs.write_message(&[], &mut buf)?;
    write_frame(&mut stream, &buf[..n]).await?;
    let msg = read_frame(&mut stream).await?;
    let n = hs.read_message(&msg, &mut buf)?;
    let payload = buf[..n].to_vec();
    finish(stream, hs, payload)
}

pub struct Reader {
    stream: OwnedReadHalf,
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

impl Reader {
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T, Error> {
        let frame = read_frame(&mut self.stream).await?;
        let mut plain = vec![0u8; frame.len()];
        let n = self.state.read_message(self.nonce, &frame, &mut plain)?;
        self.nonce += 1;
        Ok(serde_json::from_slice(&plain[..n])?)
    }
}

/// The sending half. Clones share one nonce counter and one socket.
#[derive(Clone)]
pub struct Writer {
    stream: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    state: Arc<StatelessTransportState>,
    nonce: Arc<AtomicU64>,
}

impl Writer {
    pub async fn send<T: Serialize>(&self, message: &T) -> Result<(), Error> {
        let json = serde_json::to_vec(message)?;
        if json.len() > MAX_PAYLOAD {
            return Err(Error::Protocol("message too long".into()));
        }
        let mut out = vec![0u8; json.len() + 16];
        // The lock is held from taking the nonce to writing, so messages go
        // out in nonce order.
        let mut stream = self.stream.lock().await;
        let nonce = self.nonce.fetch_add(1, Ordering::SeqCst);
        let n = self.state.write_message(nonce, &json, &mut out)?;
        write_frame(&mut *stream, &out[..n]).await
    }

    pub async fn shutdown(&self) {
        let _ = self.stream.lock().await.shutdown().await;
    }
}
