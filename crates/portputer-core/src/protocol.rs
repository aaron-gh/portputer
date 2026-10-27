//! The messages client and server exchange over the control connection, as
//! JSON inside Noise transport messages. See docs/PROTOCOL.md.

use serde::{Deserialize, Serialize};

use crate::keys::Os;

pub const PROTOCOL_VERSION: u32 = 1;
pub const SERVICE_TYPE: &str = "_portputer._tcp.local.";
/// The port a server in a user session listens on, when it's free.
pub const DEFAULT_PORT: u16 = 47800;
/// The port of the server that runs before login, so both can run at once.
pub const LOGIN_WINDOW_PORT: u16 = 47801;

/// Which session the server runs in: a logged-in user's, or the login window
/// before anyone has logged in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    User,
    LoginWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAction {
    Restart,
    Shutdown,
    Sleep,
    LogOut,
    Lock,
    /// Restart now, without waiting for apps to quit.
    ForceRestart,
    /// Restart once without the FileVault (or BitLocker) unlock screen, so
    /// the server comes back up at the login window. Needs the password.
    UnlockedRestart,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// The payload of the third handshake message.
    Hello { name: String, protocol: u32 },
    /// Proof of the pairing code: HMAC-SHA256 over "client" and the
    /// handshake hash, keyed with the normalised code, in hex.
    Pair { proof: String },
    /// `repeat` marks the key-downs a held key repeats with.
    Key {
        code: String,
        down: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        repeat: bool,
    },
    /// Presses and releases a key with modifiers held.
    Tap { code: String, modifiers: Vec<String> },
    Text { text: String },
    ReleaseAll,
    /// `mute` keeps the sound from the computer's own speakers while it
    /// streams.
    AudioStart {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mute: bool,
    },
    AudioStop,
    MuteSpeakers { muted: bool },
    Power {
        action: PowerAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    Ping { n: u64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        server_id: String,
        name: String,
        os: Os,
        session: SessionKind,
        #[serde(default)]
        user: String,
        paired: bool,
        version: String,
        /// The power actions this server can carry out.
        power: Vec<PowerAction>,
        audio: bool,
        /// Whether the computer's speakers can be muted while it streams.
        #[serde(default)]
        mute: bool,
    },
    /// The server's half of the pairing proof: "server" and the handshake
    /// hash.
    Paired { proof: String },
    Error { code: String, message: String },
    /// Something the person at the client should know, such as a missing
    /// permission on the server.
    Notice { code: String, message: String },
    AudioReady {
        port: u16,
        /// The ChaCha20-Poly1305 key for the audio packets, in hex.
        key: String,
        sample_rate: u32,
        channels: u16,
        frames: u32,
    },
    AudioStopped { reason: String },
    Pong { n: u64 },
}

pub mod errors {
    pub const NOT_PAIRED: &str = "not_paired";
    pub const PAIRING_FAILED: &str = "pairing_failed";
    pub const NO_PAIRING_CODE: &str = "no_pairing_code";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const POWER_FAILED: &str = "power_failed";
    pub const AUDIO_FAILED: &str = "audio_failed";
    pub const MUTE_FAILED: &str = "mute_failed";
    pub const BAD_MESSAGE: &str = "bad_message";
    pub const VERSION: &str = "version";
}

pub mod notices {
    pub const INPUT_PERMISSION: &str = "input_permission";
    pub const AUDIO_PERMISSION: &str = "audio_permission";
    pub const UNKNOWN_KEY: &str = "unknown_key";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape() {
        let m = ClientMessage::Key { code: "KeyA".into(), down: true, repeat: false };
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"{"type":"key","code":"KeyA","down":true}"#);
        let p = ClientMessage::Power { action: PowerAction::UnlockedRestart, user: None, password: None };
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"type":"power","action":"unlocked_restart"}"#);
        let back: ClientMessage = serde_json::from_str(r#"{"type":"release_all"}"#).unwrap();
        assert_eq!(back, ClientMessage::ReleaseAll);
    }
}
