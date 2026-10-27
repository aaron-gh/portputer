//! Wake on LAN: the magic packet, six 0xFF bytes and the hardware address
//! sixteen times, sent to the broadcast address of every network this
//! device is on.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let bytes: Vec<u8> = text.split([':', '-']).filter_map(|p| u8::from_str_radix(p, 16).ok()).collect();
    bytes.try_into().ok()
}

pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// Sends the packet for each address. True if any was sent.
pub fn wake(macs: &[String]) -> bool {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return false };
    if socket.set_broadcast(true).is_err() {
        return false;
    }
    let mut targets: Vec<Ipv4Addr> = vec![Ipv4Addr::BROADCAST];
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        let loopback = iface.is_loopback();
        if let if_addrs::IfAddr::V4(v4) = iface.addr {
            if let Some(b) = v4.broadcast {
                if !loopback && !targets.contains(&b) {
                    targets.push(b);
                }
            }
        }
    }
    let mut sent = false;
    for mac in macs.iter().filter_map(|m| parse_mac(m)) {
        let packet = magic_packet(mac);
        for target in &targets {
            for port in [9, 7] {
                sent |= socket.send_to(&packet, SocketAddr::from((*target, port))).is_ok();
            }
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    #[test]
    fn packet() {
        let mac = super::parse_mac("a1:b2:c3:d4:e5:f6").unwrap();
        let p = super::magic_packet(mac);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[6..12], &mac);
        assert!(super::parse_mac("nope").is_none());
    }
}
