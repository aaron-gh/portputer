//! Two allocations at a TURN server exchange packets. Needs a server:
//! PORTPUTER_TURN_TEST=host:port,username,password (coturn with
//! --allow-loopback-peers works locally).

use std::time::Duration;

use portputer_core::turn::{Allocation, Credentials};

#[test]
fn allocations_exchange_packets() {
    let Ok(setting) = std::env::var("PORTPUTER_TURN_TEST") else {
        eprintln!("PORTPUTER_TURN_TEST not set; skipped");
        return;
    };
    let parts: Vec<&str> = setting.split(',').collect();
    let credentials = Credentials {
        urls: vec![format!("turn:{}?transport=udp", parts[0])],
        username: parts[1].into(),
        credential: parts[2].into(),
    };
    let a = Allocation::new(&credentials).expect("allocate a");
    let b = Allocation::new(&credentials).expect("allocate b");
    a.bind(b.relayed()).expect("bind a");
    b.bind(a.relayed()).expect("bind b");
    for n in 0..20u8 {
        a.send(&[n; 100]).unwrap();
    }
    let mut got = vec![];
    while let Some(packet) = b.recv_timeout(Duration::from_millis(500)) {
        got.push(packet[0]);
        if got.len() == 20 {
            break;
        }
    }
    assert!(got.len() >= 18, "{} of 20 arrived", got.len());
    b.send(b"back").unwrap();
    assert_eq!(a.recv_timeout(Duration::from_secs(1)).as_deref(), Some(&b"back"[..]));
}
