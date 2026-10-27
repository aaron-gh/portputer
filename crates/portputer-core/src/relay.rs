//! The internet relay (relay/, a Cloudflare Worker): each relayed connection
//! is a WebSocket, turned here into a byte stream the control protocol runs
//! over unchanged, Noise and all. The relay only passes bytes on.
//!
//! The server keeps a listening WebSocket open and is told of each client;
//! it then opens a WebSocket of its own for that client's connection.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::Error;
use crate::transport::BoxStream;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RelayConfig {
    /// Such as https://portputer-relay.example.workers.dev
    pub url: String,
    /// The Worker's RELAY_TOKEN.
    #[serde(default)]
    pub token: String,
}

impl RelayConfig {
    pub fn is_set(&self) -> bool {
        !self.url.trim().is_empty()
    }

    fn endpoint(&self, path: &str) -> String {
        let base = self.url.trim().trim_end_matches('/');
        let base = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else if base.starts_with("ws://") || base.starts_with("wss://") {
            base.to_string()
        } else {
            format!("wss://{base}")
        };
        let token: String = self
            .token
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        format!("{base}/{path}?token={token}")
    }
}

fn install_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn describe(e: tungstenite::Error) -> Error {
    match e {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            let message = match status {
                503 => "The computer isn't connected to the relay.".to_string(),
                403 => "The relay refused the token.".to_string(),
                410 => "The relayed connection had already closed.".to_string(),
                other => format!("The relay answered {other}."),
            };
            Error::Refused(message)
        }
        other => Error::Protocol(format!("relay: {other}")),
    }
}

/// Opens a WebSocket and turns it into a byte stream: each binary message
/// is a chunk of the stream.
async fn open_stream(url: &str) -> Result<BoxStream, Error> {
    install_crypto();
    let (ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(url))
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

/// A client's connection to a server through the relay.
pub async fn connect(relay: &RelayConfig, server_id: &str) -> Result<BoxStream, Error> {
    open_stream(&relay.endpoint(&format!("connect/{server_id}"))).await
}

#[derive(Deserialize)]
struct Incoming {
    conn: String,
}

/// Keeps the server listening at the relay, reconnecting after failures,
/// and hands each relayed client connection to `accept`. `on_state` says
/// whether it's listening, and why not.
pub async fn listen(
    relay: RelayConfig,
    server_id: String,
    accept: impl Fn(BoxStream) + Send + Sync + 'static,
    on_state: impl Fn(Result<(), String>) + Send + Sync + 'static,
) {
    install_crypto();
    let mut wait = Duration::from_secs(2);
    loop {
        let url = relay.endpoint(&format!("listen/{server_id}"));
        match tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(&url)).await {
            Ok(Ok((ws, _))) => {
                wait = Duration::from_secs(2);
                on_state(Ok(()));
                let (mut sink, mut source) = ws.split();
                let mut ping = tokio::time::interval(Duration::from_secs(30));
                loop {
                    tokio::select! {
                        message = source.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(incoming) = serde_json::from_str::<Incoming>(&text) {
                                    let url = relay.endpoint(&format!("accept/{server_id}/{}", incoming.conn));
                                    match open_stream(&url).await {
                                        Ok(stream) => accept(stream),
                                        Err(e) => tracing::warn!("relay accept: {e}"),
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                            Some(Ok(_)) => {}
                        },
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
        let r = RelayConfig { url: "https://relay.example.workers.dev/".into(), token: "a b&c".into() };
        assert_eq!(r.endpoint("listen/x"), "wss://relay.example.workers.dev/listen/x?token=a%20b%26c");
        let local = RelayConfig { url: "http://127.0.0.1:8787".into(), token: String::new() };
        assert_eq!(local.endpoint("connect/y"), "ws://127.0.0.1:8787/connect/y?token=");
    }
}
