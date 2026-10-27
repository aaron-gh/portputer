//! Finding servers on the local network with multicast DNS, as
//! `_portputer._tcp`. The TXT record carries the server's ID (from its public
//! key), its system, its session and its hardware addresses for Wake on LAN.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::Error;
use crate::keys::Os;
use crate::protocol::{SERVICE_TYPE, SessionKind};

#[derive(Clone, Debug, PartialEq)]
pub struct ServerAd {
    /// The mDNS instance's full name, which [`BrowseEvent::Lost`] refers to.
    pub fullname: String,
    pub id: String,
    pub name: String,
    pub os: Os,
    pub session: SessionKind,
    pub user: String,
    pub addresses: Vec<String>,
    pub port: u16,
    pub macs: Vec<String>,
}

pub struct Announcement {
    daemon: ServiceDaemon,
    fullname: String,
}

pub struct AnnounceInfo<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub session: SessionKind,
    pub user: &'a str,
    pub port: u16,
}

pub fn announce(info: AnnounceInfo) -> Result<Announcement, Error> {
    let daemon = ServiceDaemon::new().map_err(|e| Error::Discovery(e.to_string()))?;
    let host: String = info.name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let instance = match info.session {
        SessionKind::User => info.name.to_string(),
        SessionKind::LoginWindow => format!("{} (Login Window)", info.name),
    };
    let session = match info.session {
        SessionKind::User => "user",
        SessionKind::LoginWindow => "login_window",
    };
    let os = match Os::current() {
        Os::Mac => "mac",
        Os::Windows => "windows",
        Os::Other => "other",
    };
    let macs = hardware_addresses().join(",");
    let properties = [
        ("id", info.id),
        ("name", info.name),
        ("os", os),
        ("session", session),
        ("user", info.user),
        ("macs", macs.as_str()),
        ("ver", env!("CARGO_PKG_VERSION")),
    ];
    let service = ServiceInfo::new(SERVICE_TYPE, &instance, &format!("{host}.local."), "", info.port, &properties[..])
        .map_err(|e| Error::Discovery(e.to_string()))?
        .enable_addr_auto();
    let fullname = service.get_fullname().to_string();
    daemon.register(service).map_err(|e| Error::Discovery(e.to_string()))?;
    Ok(Announcement { daemon, fullname })
}

impl Drop for Announcement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

#[derive(Clone, Debug)]
pub enum BrowseEvent {
    Found(ServerAd),
    Lost(String),
}

pub struct Browser {
    daemon: ServiceDaemon,
    stop: Arc<AtomicBool>,
}

impl Browser {
    /// Calls `on_event` from a background thread until dropped.
    pub fn start(on_event: impl Fn(BrowseEvent) + Send + 'static) -> Result<Browser, Error> {
        let daemon = ServiceDaemon::new().map_err(|e| Error::Discovery(e.to_string()))?;
        let receiver = daemon.browse(SERVICE_TYPE).map_err(|e| Error::Discovery(e.to_string()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new().name("portputer-browse".into()).spawn(move || {
            while let Ok(event) = receiver.recv() {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                match event {
                    ServiceEvent::ServiceResolved(service) => {
                        let prop = |k: &str| service.get_property_val_str(k).unwrap_or("").to_string();
                        let id = prop("id");
                        if id.is_empty() {
                            continue;
                        }
                        let mut addresses: Vec<String> = service
                            .get_addresses()
                            .iter()
                            .map(|a| a.to_ip_addr())
                            .filter(|a| !a.is_loopback())
                            .map(|a| a.to_string())
                            .collect();
                        // IPv4 first: link-local IPv6 needs a scope to connect.
                        addresses.sort_by_key(|a| a.contains(':'));
                        let os = match prop("os").as_str() {
                            "mac" => Os::Mac,
                            "windows" => Os::Windows,
                            _ => Os::Other,
                        };
                        let session = if prop("session") == "login_window" { SessionKind::LoginWindow } else { SessionKind::User };
                        let name = Some(prop("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| {
                            service.get_fullname().split('.').next().unwrap_or("").to_string()
                        });
                        on_event(BrowseEvent::Found(ServerAd {
                            fullname: service.get_fullname().to_string(),
                            id,
                            name,
                            os,
                            session,
                            user: prop("user"),
                            addresses,
                            port: service.get_port(),
                            macs: prop("macs").split(',').filter(|m| !m.is_empty()).map(String::from).collect(),
                        }));
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => on_event(BrowseEvent::Lost(fullname)),
                    _ => {}
                }
            }
        })?;
        Ok(Browser { daemon, stop })
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}

/// This computer's display name.
pub fn computer_name() -> String {
    let run = |cmd: &str, args: &[&str]| -> Option<String> {
        let out = std::process::Command::new(cmd).args(args).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !text.is_empty()).then_some(text)
    };
    let name = if cfg!(target_os = "macos") {
        run("scutil", &["--get", "ComputerName"])
    } else if cfg!(windows) {
        std::env::var("COMPUTERNAME").ok()
    } else {
        run("hostname", &[])
    };
    name.unwrap_or_else(|| "Computer".into())
}

/// The hardware addresses of the network interfaces with an IPv4 address,
/// for Wake on LAN.
pub fn hardware_addresses() -> Vec<String> {
    let mut out = vec![];
    let names: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && i.ip().is_ipv4())
        .map(|i| i.name)
        .collect();
    for name in names {
        if let Ok(Some(mac)) = mac_address::mac_address_by_name(&name) {
            let text = mac.to_string().to_lowercase();
            if mac.bytes() != [0; 6] && !out.contains(&text) {
                out.push(text);
            }
        }
    }
    out
}
