//! A server and a client in one process: pairing, pinning and reconnecting
//! as a known device. No keys are sent, as they would reach this computer.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use portputer_core::client::{ClientConfig, Session, SessionEvent, SessionObserver, SessionState};
use portputer_core::protocol::SessionKind;
use portputer_core::server::{Server, ServerConfig};

#[derive(Default)]
struct Events(Mutex<Vec<SessionEvent>>);

impl SessionObserver for Events {
    fn event(&self, event: SessionEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("portputer-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start_server(dir: &std::path::Path) -> Server {
    Server::start(ServerConfig {
        store_dir: dir.to_path_buf(),
        // The login window session doesn't ask for permissions.
        session: SessionKind::LoginWindow,
        port: 0,
        name: "Test Computer".into(),
        observer: None,
    })
    .unwrap()
}

#[test]
fn pairs_then_reconnects_as_known() {
    let server_dir = temp("server");
    let client_dir = temp("client");
    let server = start_server(&server_dir);
    let events = Arc::new(Events::default());
    let config = || ClientConfig { store_dir: client_dir.clone(), name: "Test Phone".into(), observer: events.clone(), local_clipboard: false };

    let session = Session::connect("127.0.0.1", server.port(), None, config()).unwrap();
    let info = session.info().unwrap();
    assert!(!info.trusted);
    assert_eq!(info.id, server.id());
    assert_eq!(info.name, "Test Computer");

    // No code showing, then a wrong code.
    assert!(session.pair("AAAA-BBBB-CCCC").is_err());
    let code = server.new_pairing_code();
    assert!(session.pair("2222-3333-4444").is_err());
    session.pair(&code.to_lowercase()).unwrap();
    assert!(session.info().unwrap().trusted);
    assert_eq!(server.paired().len(), 1);
    assert_eq!(server.paired()[0].name, "Test Phone");
    session.close();
    std::thread::sleep(Duration::from_millis(300));
    assert!(events.0.lock().unwrap().iter().any(|e| matches!(e, SessionEvent::State(SessionState::Closed { .. }))));

    let again = Session::connect("127.0.0.1", server.port(), Some(server.id()), config()).unwrap();
    assert!(again.info().unwrap().trusted, "known both ways without a code");
    again.close();

    // Another computer at the same address isn't taken for this one.
    let expected = server.id();
    drop(server);
    let other_dir = temp("other");
    let other = start_server(&other_dir);
    let result = Session::connect("127.0.0.1", other.port(), Some(expected), config());
    assert!(matches!(result, Err(portputer_core::Error::WrongServer)));
    // And without an expected ID it's an unknown computer that needs pairing.
    let unknown = Session::connect("127.0.0.1", other.port(), None, config()).unwrap();
    assert!(!unknown.info().unwrap().trusted);
    unknown.close();

    for d in [server_dir, client_dir, other_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn streams_audio() {
    // SAFETY: set before any thread reads it.
    unsafe { std::env::set_var("PORTPUTER_TEST_TONE", "1") };
    let server_dir = temp("audio-server");
    let client_dir = temp("audio-client");
    let server = start_server(&server_dir);
    let events = Arc::new(Events::default());
    let config = ClientConfig { store_dir: client_dir.clone(), name: "Test Phone".into(), observer: events.clone(), local_clipboard: false };
    let session = Session::connect("127.0.0.1", server.port(), None, config).unwrap();
    let code = server.new_pairing_code();
    session.pair(&code).unwrap();
    session.set_volume(0.0);
    session.set_speakers_muted(true);
    session.start_audio();
    std::thread::sleep(Duration::from_millis(1500));
    let started = events.0.lock().unwrap().iter().any(|e| matches!(e, SessionEvent::AudioStarted { sample_rate: 48000 }));
    let errors: Vec<_> = events.0.lock().unwrap().iter().filter(|e| matches!(e, SessionEvent::Error { .. })).cloned().collect();
    assert!(started, "audio started: {errors:?}");
    let stats = session.audio_stats().unwrap();
    // 5 ms packets for about a second.
    assert!(stats.received > 150, "{stats:?}");
    assert_eq!(stats.late, 0);
    session.set_speakers_muted(false);
    session.stop_audio();
    std::thread::sleep(Duration::from_millis(200));
    assert!(session.audio_stats().is_none());
    session.close();
    for d in [server_dir, client_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn transfers_files() {
    // SAFETY: set before the server starts.
    unsafe {
        std::env::set_var("PORTPUTER_NO_PROMPT", "1");
    }
    let server_dir = temp("files-server");
    let client_dir = temp("files-client");
    let shared = temp("files-shared");
    let downloads = temp("files-downloads");
    let received = temp("files-received");
    for d in [&shared, &downloads, &received] {
        std::fs::create_dir_all(d).unwrap();
    }
    unsafe {
        std::env::set_var("PORTPUTER_DOWNLOADS", &downloads);
    }
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(shared.join("report.bin"), &data).unwrap();
    std::fs::create_dir_all(shared.join("Folder")).unwrap();

    let server = Server::start(ServerConfig {
        store_dir: server_dir.clone(),
        session: SessionKind::User,
        port: 0,
        name: "Test Computer".into(),
        observer: None,
    })
    .unwrap();
    let events = Arc::new(Events::default());
    let config = ClientConfig { store_dir: client_dir.clone(), name: "Test Phone".into(), observer: events.clone(), local_clipboard: false };
    let session = Session::connect("127.0.0.1", server.port(), None, config).unwrap();
    assert!(session.list_files(shared.to_str().unwrap()).is_err(), "nothing before pairing");
    session.pair(&server.new_pairing_code()).unwrap();
    assert!(session.info().unwrap().files);

    let listing = session.list_files(shared.to_str().unwrap()).unwrap();
    let portputer_core::files::TransferReply::Listing { entries, .. } = listing else { panic!("{listing:?}") };
    assert_eq!(entries.iter().map(|e| (e.name.as_str(), e.dir)).collect::<Vec<_>>(), vec![("Folder", true), ("report.bin", false)]);
    assert_eq!(entries[1].size, 300_000);

    let progress = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let p = progress.clone();
    let got = session
        .download(entries[1].path.as_str(), &received, move |done, _| p.store(done, std::sync::atomic::Ordering::Relaxed))
        .unwrap();
    assert_eq!(std::fs::read(&got).unwrap(), data);
    assert_eq!(progress.load(std::sync::atomic::Ordering::Relaxed), 300_000);

    let remote = session.upload(&got, Some("../sneaky.bin".into()), None, |_, _| {}).unwrap();
    assert_eq!(std::path::Path::new(&remote), downloads.join("sneaky.bin"));
    assert_eq!(std::fs::read(downloads.join("sneaky.bin")).unwrap(), data);
    let again = session.upload(&got, Some("sneaky.bin".into()), None, |_, _| {}).unwrap();
    assert_eq!(std::path::Path::new(&again), downloads.join("sneaky 2.bin"));

    session.close();
    for d in [server_dir, client_dir, shared, downloads, received] {
        let _ = std::fs::remove_dir_all(d);
    }
}
