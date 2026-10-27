//! The helper's end of Portputer Speakers: the driver sends what's played to
//! that device to the helper's Mach service, and the helper passes it on to
//! the processes that asked for it on its socket (the server at the login
//! window, which runs as root; no one else may).

use std::ffi::{CString, c_char};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portputer_core::platform::macos::LoopbackChunk;

/// The Mach service the driver looks up, from the daemon's launchd plist.
pub const SERVICE: &str = "io.github.aaron-gh.portputer.audio";
const MESSAGE_ID: i32 = 0x5050_5331;

#[repr(C)]
struct Header {
    bits: u32,
    size: u32,
    remote_port: u32,
    local_port: u32,
    voucher_port: u32,
    id: i32,
}

const MACH_RCV_MSG: i32 = 0x0000_0002;
const KERN_SUCCESS: i32 = 0;

unsafe extern "C" {
    static bootstrap_port: u32;
    fn bootstrap_check_in(bp: u32, service_name: *const c_char, sp: *mut u32) -> i32;
    fn mach_msg(msg: *mut Header, option: i32, send_size: u32, rcv_size: u32, rcv_name: u32, timeout: u32, notify: u32) -> i32;
}

pub type Subscribers = Arc<Mutex<Vec<UnixStream>>>;

/// Receives the driver's messages for as long as the helper runs.
pub fn start(subscribers: Subscribers) {
    std::thread::Builder::new()
        .name("portputer-loopback".into())
        .spawn(move || {
            let name = CString::new(SERVICE).unwrap();
            let mut port = 0u32;
            let result = unsafe { bootstrap_check_in(bootstrap_port, name.as_ptr(), &mut port) };
            if result != KERN_SUCCESS {
                tracing::warn!("can't receive Portputer Speakers: bootstrap_check_in {result}");
                return;
            }
            // Big enough for the driver's largest message and its trailer.
            let mut buffer = vec![0u64; 1024];
            let mut out = Vec::with_capacity(8192);
            let mut last: Option<std::time::Instant> = None;
            loop {
                let header = buffer.as_mut_ptr() as *mut Header;
                let size = (buffer.len() * 8) as u32;
                let result = unsafe { mach_msg(header, MACH_RCV_MSG, 0, size, port, 0, 0) };
                if result != KERN_SUCCESS {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const u8, size as usize) };
                let length = unsafe { (*header).size } as usize;
                if unsafe { (*header).id } != MESSAGE_ID || length < 36 || length > bytes.len() {
                    continue;
                }
                let word = |at: usize| u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
                let (rate, channels, frames) = (word(24), word(28), word(32));
                let count = (frames * channels) as usize;
                if channels == 0 || 36 + count * 4 > length {
                    continue;
                }
                let mut list = subscribers.lock().unwrap();
                if last.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
                    tracing::info!("receiving Portputer Speakers, {rate} Hz, {channels} channels, {} listening", list.len());
                }
                last = Some(std::time::Instant::now());
                if list.is_empty() {
                    continue;
                }
                let samples = bytes[36..36 + count * 4].chunks_exact(4).map(|b| f32::from_ne_bytes(b.try_into().unwrap())).collect();
                LoopbackChunk { rate, channels, samples }.encode(&mut out);
                list.retain_mut(|s| s.write_all(&out).is_ok());
            }
        })
        .ok();
}

/// Adds a connection that asked for the audio.
pub fn subscribe(subscribers: &Subscribers, stream: &UnixStream) {
    if let Ok(s) = stream.try_clone() {
        s.set_write_timeout(Some(Duration::from_millis(50))).ok();
        subscribers.lock().unwrap().push(s);
    }
}
