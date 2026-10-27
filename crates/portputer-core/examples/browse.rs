//! Lists Portputer servers on the network for a few seconds.
//!   cargo run -p portputer-core --example browse [seconds]

fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let _browser = portputer_core::discovery::Browser::start(|event| println!("{event:?}")).expect("browse");
    std::thread::sleep(std::time::Duration::from_secs(seconds));
}
