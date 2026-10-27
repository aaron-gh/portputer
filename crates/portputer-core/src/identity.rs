//! Static keys, pairing codes and proofs, and the files they're kept in.
//!
//! Each server and each client has a Curve25519 key pair, which the Noise
//! handshake authenticates. A client pairs once with a code the server shows:
//! both sides prove they know it with an HMAC over the handshake hash, so a
//! man in the middle, who has a different handshake with each side, can't.
//! After that the server knows the client's public key and the client pins
//! the server's.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;

#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    #[serde(with = "hex32")]
    pub private: [u8; 32],
    #[serde(with = "hex32")]
    pub public: [u8; 32],
}

impl Identity {
    pub fn generate() -> Identity {
        let pair = crate::transport::builder().generate_keypair().expect("key generation");
        Identity {
            private: pair.private.try_into().expect("32-byte key"),
            public: pair.public.try_into().expect("32-byte key"),
        }
    }

    pub fn id(&self) -> String {
        id_of(&self.public)
    }

    pub fn load_or_create(path: &Path) -> Result<Identity, Error> {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(identity) = serde_json::from_slice::<Identity>(&bytes) {
                return Ok(identity);
            }
        }
        let identity = Identity::generate();
        write_private(path, &serde_json::to_vec_pretty(&identity)?)?;
        Ok(identity)
    }
}

/// A short, stable name for a public key: the first 16 bytes of its SHA-256,
/// in hex.
pub fn id_of(public: &[u8; 32]) -> String {
    hex::encode(&Sha256::digest(public)[..16])
}

/// The characters of pairing codes: no 0, 1, I or O, which are easily
/// confused, so 32 of them, 5 bits each.
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// Twelve characters: 60 bits.
const CODE_LENGTH: usize = 12;
pub const CODE_LIFETIME: Duration = Duration::from_secs(10 * 60);
const CODE_TRIES: u32 = 5;

pub struct PairingCode {
    code: String,
    expires: Instant,
    tries_left: u32,
}

impl PairingCode {
    pub fn new() -> PairingCode {
        let mut bytes = [0u8; CODE_LENGTH];
        getrandom::fill(&mut bytes).expect("random bytes");
        let code = bytes.iter().map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char).collect();
        PairingCode { code, expires: Instant::now() + CODE_LIFETIME, tries_left: CODE_TRIES }
    }

    /// The code in groups of four, as shown.
    pub fn display(&self) -> String {
        self.code.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("-")
    }

    pub fn valid(&self) -> bool {
        self.tries_left > 0 && Instant::now() < self.expires
    }

    pub fn expires_in(&self) -> Duration {
        self.expires.saturating_duration_since(Instant::now())
    }

    /// Checks a client's proof; each wrong one uses up a try.
    pub fn check(&mut self, hash: &[u8], proof_hex: &str) -> bool {
        if !self.valid() {
            return false;
        }
        let expected = proof(&self.code, "client", hash);
        let ok = hex::decode(proof_hex).map(|p| constant_time_eq(&p, &expected)).unwrap_or(false);
        if !ok {
            self.tries_left -= 1;
        }
        ok
    }

    pub fn server_proof(&self, hash: &[u8]) -> String {
        hex::encode(proof(&self.code, "server", hash))
    }
}

impl Default for PairingCode {
    fn default() -> Self {
        PairingCode::new()
    }
}

/// The code as typed, upper case, without spaces or dashes.
pub fn normalise_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

pub fn proof(code: &str, side: &str, hash: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(normalise_code(code).as_bytes()).expect("any key length");
    mac.update(side.as_bytes());
    mac.update(hash);
    mac.finalize().into_bytes().to_vec()
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PairedClient {
    pub id: String,
    #[serde(with = "hex32")]
    pub public_key: [u8; 32],
    pub name: String,
    pub paired_at: u64,
    /// Lets this client in at the internet relay.
    #[serde(default)]
    pub relay_token: String,
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("random bytes");
    hex::encode(bytes)
}

/// What a server keeps on disk: its identity and the clients paired with it.
pub struct ServerStore {
    dir: PathBuf,
}

impl ServerStore {
    pub fn new(dir: impl Into<PathBuf>) -> ServerStore {
        ServerStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn identity(&self) -> Result<Identity, Error> {
        Identity::load_or_create(&self.dir.join("identity.json"))
    }

    /// Read every time it's needed, so another process's changes (the app's,
    /// for the server at the login window) take effect without a restart.
    pub fn paired(&self) -> Vec<PairedClient> {
        std::fs::read(self.dir.join("paired.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// The server's room at the internet relay, made the first time.
    pub fn relay_home(&self) -> crate::relay::RelayHome {
        if let Some(home) = std::fs::read(self.dir.join("relay.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            return home;
        }
        let home = crate::relay::RelayHome::new();
        let _ = self.set_relay_home(&home);
        home
    }

    pub fn set_relay_home(&self, home: &crate::relay::RelayHome) -> Result<(), Error> {
        write_private(&self.dir.join("relay.json"), &serde_json::to_vec_pretty(home)?)
    }

    /// A paired client's relay token, made for clients paired before there
    /// were tokens.
    pub fn relay_token(&self, public_key: &[u8; 32]) -> Option<String> {
        let mut list = self.paired();
        let client = list.iter_mut().find(|c| &c.public_key == public_key)?;
        if client.relay_token.is_empty() {
            client.relay_token = new_token();
            let token = client.relay_token.clone();
            self.save_paired(&list).ok()?;
            return Some(token);
        }
        Some(client.relay_token.clone())
    }

    /// The hashes of the paired clients' relay tokens, which the relay lets in.
    pub fn relay_allowed(&self) -> Vec<String> {
        self.paired().iter().filter(|c| !c.relay_token.is_empty()).map(|c| crate::relay::token_hash(&c.relay_token)).collect()
    }

    pub fn is_paired(&self, public_key: &[u8; 32]) -> bool {
        self.paired().iter().any(|c| &c.public_key == public_key)
    }

    pub fn add_paired(&self, public_key: [u8; 32], name: &str) -> Result<(), Error> {
        let mut list = self.paired();
        list.retain(|c| c.public_key != public_key);
        list.push(PairedClient { id: id_of(&public_key), public_key, name: name.to_string(), paired_at: now_secs(), relay_token: new_token() });
        self.save_paired(&list)
    }

    pub fn remove_paired(&self, id: &str) -> Result<(), Error> {
        let mut list = self.paired();
        list.retain(|c| c.id != id);
        self.save_paired(&list)
    }

    fn save_paired(&self, list: &[PairedClient]) -> Result<(), Error> {
        write_private(&self.dir.join("paired.json"), &serde_json::to_vec_pretty(list)?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct KnownServer {
    pub id: String,
    pub name: String,
    #[serde(with = "hex32")]
    pub public_key: [u8; 32],
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    /// Hardware addresses, for Wake on LAN.
    #[serde(default)]
    pub macs: Vec<String>,
    #[serde(default)]
    pub last_connected: u64,
    /// The server's room at the internet relay and this client's token there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<crate::relay::RelayAccess>,
}

#[derive(Serialize, Deserialize)]
struct ClientFile {
    identity: Identity,
    #[serde(default)]
    servers: Vec<KnownServer>,
}

/// What a client keeps on disk: its identity and the servers it's paired with.
pub struct ClientStore {
    path: PathBuf,
}

impl ClientStore {
    pub fn new(dir: impl AsRef<Path>) -> ClientStore {
        ClientStore { path: dir.as_ref().join("client.json") }
    }

    fn load(&self) -> Result<ClientFile, Error> {
        if let Ok(bytes) = std::fs::read(&self.path) {
            if let Ok(file) = serde_json::from_slice::<ClientFile>(&bytes) {
                return Ok(file);
            }
        }
        let file = ClientFile { identity: Identity::generate(), servers: vec![] };
        self.save(&file)?;
        Ok(file)
    }

    fn save(&self, file: &ClientFile) -> Result<(), Error> {
        write_private(&self.path, &serde_json::to_vec_pretty(file)?)
    }

    pub fn identity(&self) -> Result<Identity, Error> {
        Ok(self.load()?.identity)
    }

    pub fn servers(&self) -> Vec<KnownServer> {
        self.load().map(|f| f.servers).unwrap_or_default()
    }

    pub fn server(&self, id: &str) -> Option<KnownServer> {
        self.servers().into_iter().find(|s| s.id == id)
    }

    pub fn upsert(&self, server: KnownServer) -> Result<(), Error> {
        let mut file = self.load()?;
        file.servers.retain(|s| s.id != server.id);
        file.servers.push(server);
        self.save(&file)
    }

    pub fn update(&self, id: &str, change: impl FnOnce(&mut KnownServer)) -> Result<(), Error> {
        let mut file = self.load()?;
        if let Some(s) = file.servers.iter_mut().find(|s| s.id == id) {
            change(s);
        }
        self.save(&file)
    }

    pub fn forget(&self, id: &str) -> Result<(), Error> {
        let mut file = self.load()?;
        file.servers.retain(|s| s.id != id);
        self.save(&file)
    }
}

/// Writes a file only this account can read, where the system allows.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let bytes = hex::decode(text).map_err(serde::de::Error::custom)?;
        bytes.try_into().map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_proofs() {
        let mut code = PairingCode::new();
        let hash = [7u8; 32];
        let shown = code.display();
        assert_eq!(shown.len(), 14);
        let good = hex::encode(proof(&shown.to_lowercase(), "client", &hash));
        let bad = hex::encode(proof("WRONGWRONGXX", "client", &hash));
        assert!(!code.check(&hash, &bad));
        assert!(code.check(&hash, &good));
        for _ in 0..4 {
            code.check(&hash, &bad);
        }
        assert!(!code.check(&hash, &good), "tries are used up");
    }

    #[test]
    fn stores() {
        let dir = std::env::temp_dir().join(format!("portputer-test-{}", now_secs() ^ std::process::id() as u64));
        let server = ServerStore::new(dir.join("server"));
        let a = server.identity().unwrap();
        assert_eq!(a.public, server.identity().unwrap().public);
        let client = Identity::generate();
        server.add_paired(client.public, "Phone").unwrap();
        assert!(server.is_paired(&client.public));
        server.remove_paired(&client.id()).unwrap();
        assert!(!server.is_paired(&client.public));
        let store = ClientStore::new(dir.join("client"));
        store.upsert(KnownServer { id: a.id(), name: "Mac".into(), public_key: a.public, ..Default::default() }).unwrap();
        assert_eq!(store.server(&a.id()).unwrap().name, "Mac");
        std::fs::remove_dir_all(dir).ok();
    }
}
