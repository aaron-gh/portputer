//! portputer-agent: Portputer's server without a window.
//!
//!   portputer-agent serve [--login-window] [--store DIR] [--port N]
//!       Runs a server. With --login-window it's the one launchd starts at
//!       the Mac login window, before anyone logs in, with the identity and
//!       paired devices the helper copied from the app.
//!   portputer-agent helper
//!       The root daemon: restarts and shuts down for the app, and copies its
//!       pairings for the server at the login window.
//!   portputer-agent install | uninstall
//!       As root: puts the agent, the daemon and the login window agent in
//!       place, or takes them away. The Mac app runs these with an
//!       administrator's approval.
//!   portputer-agent install-driver [BUNDLE] | uninstall-driver
//!       As root: puts the Portputer Microphone driver in
//!       /Library/Audio/Plug-Ins/HAL (by default the one in the app beside
//!       the agent), or takes it away, and restarts Core Audio.

use std::path::PathBuf;
use std::sync::Arc;

use portputer_core::protocol::{DEFAULT_PORT, LOGIN_WINDOW_PORT, SessionKind};
use portputer_core::server::{Server, ServerConfig, ServerEvent, ServerObserver};

#[cfg(target_os = "macos")]
mod helper;
#[cfg(target_os = "macos")]
mod install;
#[cfg(target_os = "macos")]
mod loopback;

struct Log;

impl ServerObserver for Log {
    fn event(&self, event: ServerEvent) {
        tracing::info!("{event:?}");
    }
}

fn usage() -> ! {
    eprintln!("usage: portputer-agent serve [--login-window] [--store DIR] [--port N] | helper | install | uninstall | install-driver [BUNDLE] | uninstall-driver");
    std::process::exit(2)
}

fn default_store(login_window: bool) -> PathBuf {
    #[cfg(target_os = "macos")]
    if login_window {
        return PathBuf::from(portputer_core::platform::macos::SYSTEM_STORE);
    }
    let _ = login_window;
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join("Library/Application Support/Portputer/server")
    } else if cfg!(windows) {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".into());
        PathBuf::from(base).join("Portputer").join("server")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".local/share/portputer/server")
    }
}

fn serve(args: &[String]) {
    let mut login_window = false;
    let mut store = None;
    let mut port = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--login-window" => login_window = true,
            "--store" => store = it.next().map(PathBuf::from),
            "--port" => port = it.next().and_then(|p| p.parse().ok()),
            _ => usage(),
        }
    }
    // At the login window, launchd stops the server with SIGTERM when someone
    // logs in: the outputs it routed through Portputer Speakers go back first.
    // SIGTERM is blocked before any thread starts and waited for on one.
    #[cfg(target_os = "macos")]
    if login_window {
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
            std::thread::spawn(move || {
                let mut signal = 0;
                libc::sigwait(&set, &mut signal);
                portputer_core::audio::loopback::stop();
                tracing::info!("stopped");
                std::process::exit(0);
            });
        }
    }
    let session = if login_window { SessionKind::LoginWindow } else { SessionKind::User };
    let store_dir = store.unwrap_or_else(|| default_store(login_window));
    let port = port.unwrap_or(if login_window { LOGIN_WINDOW_PORT } else { DEFAULT_PORT });
    let name = portputer_core::discovery::computer_name();
    let server = match Server::start(ServerConfig { store_dir: store_dir.clone(), session, port, name, observer: Some(Arc::new(Log)) }) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("can't start: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!("serving {} on port {} from {}", server.id(), server.port(), store_dir.display());
    #[cfg(target_os = "macos")]
    tracing::info!("input: {}", portputer_core::platform::macos::input_permission_detail());
    if std::env::var_os("PORTPUTER_PAIR").is_some() {
        // For testing without the app: prints a pairing code.
        println!("Pairing code: {}", server.new_pairing_code());
    }
    loop {
        std::thread::park();
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        #[cfg(target_os = "macos")]
        Some("helper") => helper::run(),
        #[cfg(target_os = "macos")]
        Some("install") => install::install(),
        #[cfg(target_os = "macos")]
        Some("uninstall") => install::uninstall(),
        #[cfg(target_os = "macos")]
        Some("install-driver") => install::install_driver(args.get(1).map(String::as_str)),
        #[cfg(target_os = "macos")]
        Some("uninstall-driver") => install::uninstall_driver(),
        #[cfg(target_os = "macos")]
        Some("probe-audio") => println!("{}", portputer_core::audio::feed::probe()),
        #[cfg(target_os = "macos")]
        Some("inject") => {
            let number = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).and_then(|n| n.parse().ok());
            match number("--probe") {
                Some(n) => println!("{}", portputer_core::platform::macos::probe_strategy(n)),
                None => portputer_core::platform::macos::run_injector(number("--strategy").unwrap_or(0)),
            }
        }
        #[cfg(target_os = "macos")]
        Some("hid-probe") => println!("{}", portputer_core::platform::macos::probe_hid()),
        #[cfg(target_os = "macos")]
        Some("posting-probe") => println!("{}", portputer_core::platform::macos::posting_probe()),
        #[cfg(target_os = "macos")]
        Some("restore-outputs") => println!("{}", if portputer_core::audio::loopback::recover() { "Outputs put back." } else { "Nothing to put back." }),
        #[cfg(target_os = "macos")]
        Some("test-speakers") => match portputer_core::audio::loopback::test() {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        _ => usage(),
    }
}
