//! The helper daemon. It runs as root and does what the app can't: restart
//! now, restart past the FileVault unlock screen, and copy the server's
//! identity and paired devices to where the server at the login window reads
//! them. It answers local processes on a Unix socket, checking who they are
//! from the socket: power actions for the person at the console or an
//! administrator, copying pairings for administrators only, and what's
//! played to Portputer Speakers for root only (see loopback.rs).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use portputer_core::identity::{Identity, PairedClient, write_private};
use portputer_core::platform::macos::{HELPER_SOCKET, HelperReply, HelperRequest, SYSTEM_STORE, privileged_power};
use portputer_core::protocol::PowerAction;

const ADMIN_GID: libc::gid_t = 80;

pub fn run() -> ! {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("the helper runs as root, from launchd");
        std::process::exit(1);
    }
    let _ = std::fs::remove_file(HELPER_SOCKET);
    let listener = match UnixListener::bind(HELPER_SOCKET) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("can't listen on {HELPER_SOCKET}: {e}");
            std::process::exit(1);
        }
    };
    // Anyone may connect; each request is checked against who sent it.
    let _ = std::fs::set_permissions(HELPER_SOCKET, std::fs::Permissions::from_mode(0o666));
    tracing::info!("helper listening on {HELPER_SOCKET}");
    let subscribers = crate::loopback::Subscribers::default();
    crate::loopback::start(subscribers.clone());
    for stream in listener.incoming().flatten() {
        let subscribers = subscribers.clone();
        std::thread::spawn(move || handle(stream, &subscribers));
    }
    std::process::exit(0)
}

fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let ok = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (ok == 0).then_some(uid)
}

fn is_admin(uid: libc::uid_t) -> bool {
    if uid == 0 {
        return true;
    }
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return false;
        }
        let mut groups = vec![0 as libc::c_int; 256];
        let mut count = groups.len() as libc::c_int;
        let r = libc::getgrouplist((*pw).pw_name, (*pw).pw_gid as libc::c_int, groups.as_mut_ptr(), &mut count);
        r == 0 && groups[..count.max(0) as usize].contains(&(ADMIN_GID as libc::c_int))
    }
}

fn console_uid() -> Option<libc::uid_t> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/dev/console").ok().map(|m| m.uid())
}

fn handle(stream: UnixStream, subscribers: &crate::loopback::Subscribers) {
    let Some(uid) = peer_uid(&stream) else { return };
    let mut line = String::new();
    let mut reader = BufReader::new(&stream);
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let request = serde_json::from_str::<HelperRequest>(&line);
    let audio = matches!(&request, Ok(r) if r.audio);
    let reply = match request {
        Ok(request) if request.audio => {
            if uid == 0 { Ok(()) } else { Err("Only Portputer's server at the login window can do that.".into()) }
        }
        Ok(request) => answer(uid, request),
        Err(e) => Err(format!("bad request: {e}")),
    };
    let subscribe = audio && reply.is_ok();
    let reply = match reply {
        Ok(()) => HelperReply { ok: true, error: String::new() },
        Err(error) => HelperReply { ok: false, error },
    };
    let mut out = serde_json::to_string(&reply).unwrap_or_default();
    out.push('\n');
    if (&stream).write_all(out.as_bytes()).is_ok() && subscribe {
        crate::loopback::subscribe(subscribers, &stream);
    }
}

fn answer(uid: libc::uid_t, request: HelperRequest) -> Result<(), String> {
    if let Some((identity, paired)) = request.sync {
        if !is_admin(uid) {
            return Err("Only an administrator can set up Portputer at the login window.".into());
        }
        let identity: Identity = serde_json::from_str(&identity).map_err(|e| format!("identity: {e}"))?;
        let paired: Vec<PairedClient> = serde_json::from_str(&paired).map_err(|e| format!("paired devices: {e}"))?;
        let dir = Path::new(SYSTEM_STORE);
        write_private(&dir.join("identity.json"), &serde_json::to_vec_pretty(&identity).unwrap()).map_err(|e| e.to_string())?;
        write_private(&dir.join("paired.json"), &serde_json::to_vec_pretty(&paired).unwrap()).map_err(|e| e.to_string())?;
        match request.relay.and_then(|r| serde_json::from_str::<portputer_core::relay::RelayHome>(&r).ok()) {
            Some(relay) => write_private(&dir.join("relay.json"), &serde_json::to_vec_pretty(&relay).unwrap()).map_err(|e| e.to_string())?,
            None => {
                let _ = std::fs::remove_file(dir.join("relay.json"));
            }
        }
        tracing::info!("copied {} paired devices for the login window", paired.len());
        return Ok(());
    }
    let Some(action) = request.action else { return Err("nothing asked".into()) };
    if !(is_admin(uid) || console_uid() == Some(uid)) {
        return Err("Only the person logged in or an administrator can do that.".into());
    }
    if matches!(action, PowerAction::LogOut | PowerAction::Lock) {
        return Err("not done by the helper".into());
    }
    tracing::info!("{action:?} for uid {uid}");
    privileged_power(action, request.user.as_deref(), request.password.as_deref())
}
