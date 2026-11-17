//! Manual VM-only network test. Never starts audio, microphone or clipboard.
use portputer_core::client::{ClientConfig, Session, SessionEvent, SessionObserver, known_servers};
use std::{sync::Arc, time::Duration};
struct Quiet;
impl SessionObserver for Quiet {
    fn event(&self, event: SessionEvent) {
        if let SessionEvent::Error{code,message}=event {eprintln!("Server error {code}: {message}");}
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 4 {
        return Err("HOST STORE pair|shift|lock [PAIR_CODE]".into());
    }
    let store = std::path::PathBuf::from(&args[2]);
    let id = known_servers(&store).first().map(|s| s.id.clone());
    let session = Session::connect(
        &args[1],
        47800,
        id,
        ClientConfig {
            store_dir: store,
            name: "Portputer VM test client".into(),
            observer: Arc::new(Quiet),
            local_clipboard: false,
        },
    )?;
    match args[3].as_str() {
        "pair" => {
            session.pair(args.get(4).ok_or("missing pairing code")?)?;
            println!("VM test client paired");
        }
        "shift" => {
            if !session.info().is_some_and(|s| s.trusted) {
                return Err("not paired".into());
            }
            session.tap("ShiftLeft", vec![]);
            session.release_all();
            std::thread::sleep(Duration::from_millis(500));
            println!("Harmless Shift tap sent over authenticated Portputer connection");
        }
        "lock" => {
            if !session.info().is_some_and(|s|s.trusted){return Err("not paired".into());}
            session.power(portputer_core::protocol::PowerAction::Lock,None,None);
            std::thread::sleep(Duration::from_secs(2));
            println!("Power Lock sent over authenticated Portputer connection");
        }
        _ => return Err("unsupported test command".into()),
    }
    session.close();
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}
