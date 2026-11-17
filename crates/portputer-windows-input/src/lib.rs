//! Bounded local input protocol. No network listener, file operations, shell,
//! pairing, clipboard or power commands are accepted by the privileged worker.
use serde::{Deserialize, Serialize};

pub const MAX_INPUTS: usize = 64;
pub const MAX_FRAME: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyInput {
    pub vk: u16,
    pub scan: u16,
    pub flags: u32,
}

impl KeyInput {
    pub fn valid(self) -> bool {
        // EXTENDEDKEY, KEYUP, UNICODE, SCANCODE only. Neither hardware packet
        // pointers nor arbitrary INPUT structures cross the privilege boundary.
        if self.flags & !0x0f != 0 {
            return false;
        }
        let unicode = self.flags & 4 != 0;
        let scan = self.flags & 8 != 0;
        if unicode {
            return !scan && self.vk == 0 && self.flags & 1 == 0;
        }
        if scan {
            return self.vk == 0 && self.scan <= 0xff;
        }
        self.scan == 0 && self.vk > 0 && self.vk <= 0xff
    }
    pub fn up(mut self) -> Self {
        self.flags |= 2;
        self
    }
    pub fn identity(self) -> (u16, u16, u32) {
        (self.vk, self.scan, self.flags & !2)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum Request {
    Input { events: Vec<KeyInput> },
    Release {},
    Probe {},
}
impl Request {
    pub fn valid(&self) -> bool {
        match self {
            Self::Input { events } => {
                !events.is_empty() && events.len() <= MAX_INPUTS && events.iter().all(|k| k.valid())
            }
            _ => true,
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub ok: bool,
    pub desktop: String,
    pub session: u32,
}

/// Delimiter framing with an absolute bound, including across partial reads.
pub fn take_frame(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, &'static str> {
    if let Some(end) = buffer.iter().position(|b| *b == b'\n') {
        if end > MAX_FRAME {
            return Err("oversized frame");
        }
        let mut frame: Vec<_> = buffer.drain(..=end).collect();
        frame.pop();
        Ok(Some(frame))
    } else if buffer.len() > MAX_FRAME {
        Err("oversized frame")
    } else {
        Ok(None)
    }
}

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub mod setup;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_flags_and_mixed_encodings() {
        assert!(
            !KeyInput {
                vk: 0,
                scan: 1,
                flags: 0x10
            }
            .valid()
        );
        assert!(
            !KeyInput {
                vk: 0,
                scan: 1,
                flags: 12
            }
            .valid()
        );
        assert!(
            !KeyInput {
                vk: 65,
                scan: 0,
                flags: 4
            }
            .valid()
        );
        assert!(
            !KeyInput {
                vk: 0,
                scan: 256,
                flags: 8
            }
            .valid()
        );
        assert!(
            KeyInput {
                vk: 0,
                scan: 0xd83d,
                flags: 4
            }
            .valid()
        );
        assert!(
            KeyInput {
                vk: 0,
                scan: 0x52,
                flags: 9
            }
            .valid()
        );
    }
    #[test]
    fn bounds_batches_and_rejects_unknown_operations() {
        let key = KeyInput {
            vk: 0,
            scan: 30,
            flags: 8,
        };
        assert!(
            Request::Input {
                events: vec![key; 64]
            }
            .valid()
        );
        assert!(
            !Request::Input {
                events: vec![key; 65]
            }
            .valid()
        );
        assert!(!Request::Input { events: vec![] }.valid());
        assert!(serde_json::from_str::<Request>(r#"{"op":"Shell","command":"x"}"#).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"Probe","extra":"x"}"#).is_err());
    }
    #[test]
    fn handles_partial_frames_and_overflow() {
        let mut b = b"one".to_vec();
        assert_eq!(take_frame(&mut b).unwrap(), None);
        b.extend_from_slice(b"\ntwo\n");
        assert_eq!(take_frame(&mut b).unwrap().unwrap(), b"one");
        assert_eq!(take_frame(&mut b).unwrap().unwrap(), b"two");
        let mut b = vec![b'x'; MAX_FRAME + 1];
        assert!(take_frame(&mut b).is_err());
        b.push(b'\n');
        assert!(take_frame(&mut b).is_err());
    }
    #[test]
    fn release_preserves_extended_key_identity() {
        let k = KeyInput {
            vk: 0,
            scan: 0x52,
            flags: 9,
        };
        assert_eq!(k.identity(), k.up().identity());
        assert_eq!(k.up().flags, 11);
    }
}
