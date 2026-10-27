//! The internet relay (relay/, a Cloudflare Worker): each relayed connection
//! is a WebSocket, turned here into a byte stream the control protocol runs
//! over unchanged, Noise and all. The relay only passes bytes on.
//!
//! Nothing is typed. Each computer makes its own room (a random 256-bit
//! name) and room key the first time it runs, and gives each device it pairs
//! with the room and a token of that device's own, over the encrypted
//! connection. At the relay, the computer claims its room with the key and
//! keeps the hashes of its paired devices' tokens there; only those tokens
//! get in, and unpairing a device takes its token out. Every refusal looks
//! the same as a room that doesn't exist.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::Error;
use crate::protocol::SessionKind;
use crate::transport::BoxStream;

/// The relay the apps use unless a computer is set to another: the
/// PORTPUTER_RELAY_URL the build had, from relay/relay-url.txt.
pub fn default_url() -> Option<&'static str> {
    option_env!("PORTPUTER_RELAY_URL").filter(|u| !u.trim().is_empty())
}

fn random_hex() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("random bytes");
    hex::encode(bytes)
}

/// A device's token as the relay keeps it.
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// A computer's place at the relay, made once and kept with its identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayHome {
    /// On unless turned off.
    pub enabled: bool,
    /// Another relay than the built-in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_url: Option<String>,
    pub room: String,
    /// Proves to the relay that the room is this computer's. Never sent to
    /// devices.
    pub key: String,
}

impl RelayHome {
    pub fn new() -> RelayHome {
        RelayHome { enabled: true, custom_url: None, room: random_hex(), key: random_hex() }
    }

    pub fn url(&self) -> Option<String> {
        self.custom_url.clone().filter(|u| !u.trim().is_empty()).or_else(|| default_url().map(String::from))
    }
}

impl Default for RelayHome {
    fn default() -> Self {
        RelayHome::new()
    }
}

/// What a paired device needs to reach its computer through the relay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RelayAccess {
    pub url: String,
    pub room: String,
    pub token: String,
}

fn endpoint(url: &str, path: &str, query: &[(&str, &str)]) -> String {
    let base = url.trim().trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if base.starts_with("ws://") || base.starts_with("wss://") {
        base.to_string()
    } else {
        format!("wss://{base}")
    };
    let encode = |s: &str| -> String {
        s.bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect()
    };
    let query: Vec<String> = query.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect();
    format!("{base}/{path}?{}", query.join("&"))
}

fn install_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn describe(e: tungstenite::Error) -> Error {
    match e {
        tungstenite::Error::Http(response) => Error::Refused(match response.status().as_u16() {
            503 => "The computer isn't connected to the relay.".to_string(),
            404 => "The relay refused this device: it isn't paired with the computer any more, or the computer hasn't been through the relay yet.".to_string(),
            410 => "The relayed connection had already closed.".to_string(),
            other => format!("The relay answered {other}."),
        }),
        other => Error::Protocol(format!("relay: {other}")),
    }
}

/// Opens a WebSocket and turns it into a byte stream: each binary message
/// is a chunk of the stream.
async fn open_stream(url: &str) -> Result<BoxStream, Error> {
    install_crypto();
    let (ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async_with_config(url, None, true))
        .await
        .map_err(|_| Error::Protocol("the relay didn't answer".into()))?
        .map_err(describe)?;
    let (ours, theirs) = tokio::io::duplex(256 * 1024);
    tokio::spawn(async move {
        let (mut sink, mut source) = ws.split();
        let (mut read, mut write) = tokio::io::split(theirs);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            tokio::select! {
                message = source.next() => match message {
                    Some(Ok(Message::Binary(data))) => {
                        if write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
                n = read.read(&mut buf) => match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                },
            }
        }
        let _ = sink.close().await;
        let _ = write.shutdown().await;
    });
    Ok(Box::new(ours))
}

/// A device's connection to its computer through the relay.
pub async fn connect(access: &RelayAccess) -> Result<BoxStream, Error> {
    open_stream(&endpoint(&access.url, &format!("connect/{}", access.room), &[("token", &access.token)])).await
}

#[derive(Deserialize)]
struct Incoming {
    conn: String,
}

/// Keeps the computer listening in its room, reconnecting after failures,
/// with the relay kept up to date with `allowed` (paired devices' token
/// hashes). Each relayed connection is handed to `accept`; `on_state` says
/// whether it's listening, and why not.
pub async fn listen(
    url: String,
    home: RelayHome,
    session: SessionKind,
    mut allowed: tokio::sync::watch::Receiver<Vec<String>>,
    accept: impl Fn(BoxStream) + Send + Sync + 'static,
    on_state: impl Fn(Result<(), String>) + Send + Sync + 'static,
) {
    install_crypto();
    let session_name = match session {
        SessionKind::User => "user",
        SessionKind::LoginWindow => "login_window",
    };
    let mut wait = Duration::from_secs(2);
    loop {
        let listen_url = endpoint(&url, &format!("listen/{}", home.room), &[("key", &home.key), ("session", session_name)]);
        match tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async_with_config(&listen_url, None, true)).await {
            Ok(Ok((ws, _))) => {
                wait = Duration::from_secs(2);
                let (mut sink, mut source) = ws.split();
                let allow = |tokens: &Vec<String>| Message::Text(serde_json::json!({ "type": "allow", "tokens": tokens }).to_string().into());
                let first = allow(&allowed.borrow_and_update());
                if sink.send(first).await.is_ok() {
                    on_state(Ok(()));
                }
                let mut ping = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tokio::select! {
                        message = source.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(incoming) = serde_json::from_str::<Incoming>(&text) {
                                    let url = endpoint(&url, &format!("accept/{}/{}", home.room, incoming.conn), &[("key", &home.key)]);
                                    match open_stream(&url).await {
                                        Ok(stream) => accept(stream),
                                        Err(e) => tracing::warn!("relay accept: {e}"),
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                            Some(Ok(_)) => {}
                        },
                        changed = allowed.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            let update = allow(&allowed.borrow_and_update());
                            if sink.send(update).await.is_err() {
                                break;
                            }
                        }
                        _ = ping.tick() => {
                            if sink.send(Message::Text("ping".into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                on_state(Err("The connection to the relay closed.".into()));
            }
            Ok(Err(e)) => on_state(Err(describe(e).to_string())),
            Err(_) => on_state(Err("The relay didn't answer.".into())),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        assert_eq!(
            endpoint("https://relay.example.workers.dev/", "connect/ab", &[("token", "a b&c")]),
            "wss://relay.example.workers.dev/connect/ab?token=a%20b%26c"
        );
        assert_eq!(endpoint("http://127.0.0.1:8787", "listen/x", &[("key", "k"), ("session", "user")]), "ws://127.0.0.1:8787/listen/x?key=k&session=user");
        let home = RelayHome::new();
        assert_eq!(home.room.len(), 64);
        assert_ne!(home.room, home.key);
        assert_eq!(token_hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}

/// Whether the computer is listening at the relay now, for a paired device.
pub fn status(access: &RelayAccess) -> Result<bool, Error> {
    let base = access.url.trim().trim_end_matches('/');
    let body: serde_json::Value = serde_json::from_str(&get_json(&format!("{base}/status/{}?token={}", access.room, access.token))?)?;
    Ok(body["online"].as_bool().unwrap_or(false))
}

fn get_json(url: &str) -> Result<String, Error> {
    install_crypto();
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().new_agent();
    let mut response = agent.get(url).call().map_err(|e| Error::Protocol(format!("relay: {e}")))?;
    response.body_mut().read_to_string().map_err(|e| Error::Protocol(format!("relay: {e}")))
}

/// PORTPUTER_TURN_TEST=host:port,username,password stands in for the
/// relay's TURN credentials, for tests against a local TURN server.
fn test_credentials() -> Option<crate::turn::Credentials> {
    let setting = std::env::var("PORTPUTER_TURN_TEST").ok()?;
    let mut parts = setting.splitn(3, ',');
    let (server, username, credential) = (parts.next()?, parts.next()?, parts.next()?);
    Some(crate::turn::Credentials { urls: vec![format!("turn:{server}?transport=udp")], username: username.into(), credential: credential.into() })
}

/// TURN credentials for a paired device, from its computer's room. Fails
/// when the relay has no TURN service set up.
pub fn turn_credentials(access: &RelayAccess) -> Result<crate::turn::Credentials, Error> {
    if let Some(credentials) = test_credentials() {
        return Ok(credentials);
    }
    let base = access.url.trim().trim_end_matches('/');
    Ok(serde_json::from_str(&get_json(&format!("{base}/turn/{}?token={}", access.room, access.token))?)?)
}

/// TURN credentials for the computer, with its room key.
pub fn turn_credentials_for_home(home: &RelayHome) -> Result<crate::turn::Credentials, Error> {
    if let Some(credentials) = test_credentials() {
        return Ok(credentials);
    }
    let url = home.url().filter(|_| home.enabled).ok_or_else(|| Error::Protocol("the relay is off".into()))?;
    let base = url.trim().trim_end_matches('/');
    Ok(serde_json::from_str(&get_json(&format!("{base}/turn/{}?key={}", home.room, home.key))?)?)
}
