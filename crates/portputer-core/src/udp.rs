//! Audio packets: a 6-byte header (magic, kind, 32-bit sequence number) and
//! a ChaCha20-Poly1305 ciphertext, with the header as associated data. The
//! nonce is the kind followed by the sequence number, so the server's audio
//! and the client's keep-alives never share one.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

pub const MAGIC: u8 = 0x50;
/// Server to client: PCM, 16-bit little-endian, interleaved.
pub const KIND_AUDIO: u8 = 1;
/// Client to server: empty, every 500 ms, so the server knows where to send.
pub const KIND_KEEPALIVE: u8 = 2;
pub const HEADER: usize = 6;
pub const KEEPALIVE_INTERVAL_MS: u64 = 500;
/// The server stops sending this long after the last keep-alive.
pub const KEEPALIVE_TIMEOUT_MS: u64 = 5000;

#[derive(Clone)]
pub struct PacketKey(ChaCha20Poly1305);

impl PacketKey {
    pub fn new(key: &[u8; 32]) -> PacketKey {
        PacketKey(ChaCha20Poly1305::new(&Key::from(*key)))
    }

    pub fn random() -> ([u8; 32], PacketKey) {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("random bytes");
        (key, PacketKey::new(&key))
    }

    fn nonce(kind: u8, seq: u32) -> Nonce {
        let mut n = [0u8; 12];
        n[0] = kind;
        n[8..].copy_from_slice(&seq.to_be_bytes());
        Nonce::from(n)
    }

    pub fn seal(&self, kind: u8, seq: u32, payload: &[u8]) -> Vec<u8> {
        let mut header = [0u8; HEADER];
        header[0] = MAGIC;
        header[1] = kind;
        header[2..6].copy_from_slice(&seq.to_be_bytes());
        let sealed = self
            .0
            .encrypt(&Self::nonce(kind, seq), Payload { msg: payload, aad: &header })
            .expect("encryption of a short packet");
        let mut out = Vec::with_capacity(HEADER + sealed.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&sealed);
        out
    }

    /// The kind, sequence number and payload of a genuine packet.
    pub fn open(&self, packet: &[u8]) -> Option<(u8, u32, Vec<u8>)> {
        if packet.len() < HEADER + 16 || packet[0] != MAGIC {
            return None;
        }
        let kind = packet[1];
        let seq = u32::from_be_bytes(packet[2..6].try_into().ok()?);
        let plain = self
            .0
            .decrypt(&Self::nonce(kind, seq), Payload { msg: &packet[HEADER..], aad: &packet[..HEADER] })
            .ok()?;
        Some((kind, seq, plain))
    }
}

pub fn encode_pcm(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

pub fn decode_pcm(bytes: &[u8]) -> Vec<i16> {
    bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tamper() {
        let (_, key) = PacketKey::random();
        let packet = key.seal(KIND_AUDIO, 42, &encode_pcm(&[1, -2, 3]));
        let (kind, seq, payload) = key.open(&packet).unwrap();
        assert_eq!((kind, seq), (KIND_AUDIO, 42));
        assert_eq!(decode_pcm(&payload), vec![1, -2, 3]);
        let mut changed = packet.clone();
        changed[5] ^= 1;
        assert!(key.open(&changed).is_none());
        let (_, other) = PacketKey::random();
        assert!(other.open(&packet).is_none());
    }
}
