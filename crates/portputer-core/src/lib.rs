//! Portputer's core: the protocol, encryption, discovery, the server and
//! client, audio transport and the key tables. The native apps are built on
//! it through `portputer-ffi`; the headless server is `portputer-agent`.

pub mod audio;
pub mod client;
pub mod clipboard;
pub mod discovery;
pub mod files;
pub mod identity;
pub mod jitter;
pub mod keys;
pub mod platform;
pub mod protocol;
pub mod relay;
pub mod server;
pub mod transport;
pub mod turn;
pub mod udp;
pub mod wol;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("encryption: {0}")]
    Noise(#[from] snow::Error),
    #[error("bad message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Protocol(String),
    #[error("discovery: {0}")]
    Discovery(String),
    #[error("audio: {0}")]
    Audio(String),
    #[error("A different computer answered at that address.")]
    WrongServer,
    #[error("{0}")]
    Refused(String),
    #[error("The connection is closed.")]
    Closed,
}
