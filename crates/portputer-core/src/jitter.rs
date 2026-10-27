//! The client's audio buffer: puts packets back in sequence order, holds a
//! set amount of audio to absorb network jitter, conceals lost packets, and
//! resamples to the output device's rate, running slightly fast or slow to
//! keep the buffer at its target as the two computers' clocks drift apart.

use std::collections::BTreeMap;

/// Packets further ahead than this restart the stream (the server restarted
/// or the client fell far behind).
const MAX_AHEAD: u32 = 400;
/// The most the playback speed is changed to correct drift.
const MAX_CORRECTION: f64 = 0.01;
/// Lost packets in a row that are concealed by repeating the last one, quieter
/// each time, before silence.
const CONCEAL_REPEATS: u32 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JitterStats {
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub underruns: u64,
    /// Audio waiting to be played, in milliseconds.
    pub buffered_ms: u32,
}

pub struct JitterBuffer {
    rate: u32,
    channels: usize,
    frames_per_packet: usize,
    target_frames: usize,
    packets: BTreeMap<u64, Vec<i16>>,
    /// The next packet to play, unwrapped to 64 bits.
    next: Option<u64>,
    last_seq: Option<u64>,
    current: Vec<i16>,
    position: usize,
    playing: bool,
    concealed: u32,
    last_packet: Vec<i16>,
    // Resampler state: the frames either side of the output position.
    prev: Vec<f32>,
    cur: Vec<f32>,
    frac: f64,
    level: f64,
    pub gain: f32,
    pub stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(rate: u32, channels: u16, frames_per_packet: u32, target_ms: u32) -> JitterBuffer {
        let channels = channels.max(1) as usize;
        JitterBuffer {
            rate,
            channels,
            frames_per_packet: frames_per_packet as usize,
            target_frames: (rate as usize * target_ms as usize / 1000).max(frames_per_packet as usize),
            packets: BTreeMap::new(),
            next: None,
            last_seq: None,
            current: Vec::new(),
            position: 0,
            playing: false,
            concealed: 0,
            last_packet: Vec::new(),
            prev: vec![0.0; channels],
            cur: vec![0.0; channels],
            frac: 0.0,
            level: 0.0,
            gain: 1.0,
            stats: JitterStats::default(),
        }
    }

    pub fn set_target_ms(&mut self, ms: u32) {
        self.target_frames = (self.rate as usize * ms as usize / 1000).max(self.frames_per_packet);
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Turns a 32-bit sequence number into a 64-bit one near the last seen.
    fn unwrap(&self, seq: u32) -> u64 {
        match self.last_seq {
            None => seq as u64 + (1 << 32),
            Some(last) => {
                let base = last & !0xFFFF_FFFF;
                let candidates = [base.wrapping_sub(1 << 32), base, base + (1 << 32)];
                candidates
                    .iter()
                    .map(|b| b + seq as u64)
                    .min_by_key(|c| c.abs_diff(last))
                    .unwrap()
            }
        }
    }

    pub fn push(&mut self, seq: u32, samples: Vec<i16>) {
        let seq = self.unwrap(seq);
        self.last_seq = Some(self.last_seq.map_or(seq, |l| l.max(seq)));
        self.stats.received += 1;
        match self.next {
            None => self.next = Some(seq),
            Some(next) if seq < next => {
                self.stats.late += 1;
                return;
            }
            Some(next) if seq - next > MAX_AHEAD as u64 => {
                self.packets.clear();
                self.next = Some(seq);
                self.playing = false;
            }
            _ => {}
        }
        self.packets.insert(seq, samples);
    }

    fn buffered_frames(&self) -> usize {
        let queued: usize = self.packets.values().map(|p| p.len() / self.channels).sum();
        queued + (self.current.len() / self.channels).saturating_sub(self.position)
    }

    /// The next frame of the source, after reordering and concealment.
    fn source_frame(&mut self, out: &mut [f32]) {
        if self.position * self.channels >= self.current.len() {
            if !self.advance() {
                out.iter_mut().for_each(|s| *s = 0.0);
                return;
            }
        }
        let base = self.position * self.channels;
        for (c, o) in out.iter_mut().enumerate() {
            *o = self.current.get(base + c).copied().unwrap_or(0) as f32 / 32768.0;
        }
        self.position += 1;
    }

    /// Moves to the next packet. False while buffering.
    fn advance(&mut self) -> bool {
        if !self.playing {
            if self.buffered_frames() < self.target_frames || self.next.is_none() {
                return false;
            }
            self.playing = true;
        }
        let Some(next) = self.next else { return false };
        self.position = 0;
        if let Some(packet) = self.packets.remove(&next) {
            self.concealed = 0;
            self.last_packet = packet.clone();
            self.current = packet;
        } else if self.packets.is_empty() {
            // Nothing after it has arrived either: the stream stalled.
            self.stats.underruns += 1;
            self.playing = false;
            self.current.clear();
            return false;
        } else {
            self.stats.lost += 1;
            self.concealed += 1;
            let gain = if self.concealed <= CONCEAL_REPEATS { 0.5f32.powi(self.concealed as i32) } else { 0.0 };
            self.current = self.last_packet.iter().map(|s| (*s as f32 * gain) as i16).collect();
            if self.current.is_empty() {
                self.current = vec![0; self.frames_per_packet * self.channels];
            }
        }
        self.next = Some(next + 1);
        true
    }

    /// Fills an interleaved output buffer at the device's rate and channels.
    pub fn fill(&mut self, out: &mut [f32], out_channels: usize, out_rate: u32) {
        let out_channels = out_channels.max(1);
        // Smoothed buffer level, for drift correction only while playing.
        let level = self.buffered_frames() as f64;
        self.level = if self.level == 0.0 { level } else { self.level * 0.995 + level * 0.005 };
        let correction = if self.playing {
            let error_secs = (self.level - self.target_frames as f64) / self.rate as f64;
            (error_secs * 0.2).clamp(-MAX_CORRECTION, MAX_CORRECTION)
        } else {
            0.0
        };
        let step = self.rate as f64 / out_rate.max(1) as f64 * (1.0 + correction);
        let mut frame = vec![0f32; self.channels];
        for chunk in out.chunks_mut(out_channels) {
            for c in 0..chunk.len() {
                let src = if self.channels == 1 { 0 } else { c };
                chunk[c] = if src < self.channels {
                    let a = self.prev[src];
                    let b = self.cur[src];
                    (a + (b - a) * self.frac as f32) * self.gain
                } else {
                    0.0
                };
            }
            if out_channels == 1 && self.channels > 1 {
                let sum: f32 = (0..self.channels).map(|c| self.prev[c] + (self.cur[c] - self.prev[c]) * self.frac as f32).sum();
                chunk[0] = sum / self.channels as f32 * self.gain;
            }
            self.frac += step;
            while self.frac >= 1.0 {
                self.frac -= 1.0;
                std::mem::swap(&mut self.prev, &mut self.cur);
                self.source_frame(&mut frame);
                self.cur.copy_from_slice(&frame);
            }
        }
        self.stats.buffered_ms = (self.buffered_frames() as u64 * 1000 / self.rate.max(1) as u64) as u32;
    }

    pub fn reset(&mut self) {
        self.packets.clear();
        self.next = None;
        self.last_seq = None;
        self.current.clear();
        self.position = 0;
        self.playing = false;
        self.level = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(value: i16) -> Vec<i16> {
        vec![value; 240 * 2]
    }

    #[test]
    fn reorders_and_conceals() {
        let mut jb = JitterBuffer::new(48000, 2, 240, 20);
        // 4 packets reach the 20 ms target; they arrive out of order and 2 is lost.
        for seq in [0u32, 3, 1, 4, 5] {
            jb.push(seq, packet(seq as i16 * 1000 + 1000));
        }
        let mut out = vec![0f32; 240 * 2];
        let mut firsts = vec![];
        for _ in 0..6 {
            jb.fill(&mut out, 2, 48000);
            firsts.push((out[100] * 32768.0).round() as i32);
        }
        assert_eq!(firsts[0], 1000);
        assert_eq!(firsts[1], 2000);
        assert_eq!(firsts[2], 1000, "packet 2 is concealed as half of packet 1");
        assert_eq!(firsts[3], 4000);
        assert_eq!(jb.stats.lost, 1);
        jb.push(1, packet(9));
        assert_eq!(jb.stats.late, 1);
    }

    #[test]
    fn waits_for_target_then_underruns() {
        let mut jb = JitterBuffer::new(48000, 2, 240, 40);
        jb.push(0, packet(100));
        let mut out = vec![1f32; 480];
        jb.fill(&mut out, 2, 48000);
        assert!(out.iter().all(|s| *s == 0.0), "silent until the target is buffered");
        for seq in 1..8 {
            jb.push(seq, packet(100));
        }
        for _ in 0..12 {
            jb.fill(&mut out, 2, 48000);
        }
        assert_eq!(jb.stats.underruns, 1);
    }

    #[test]
    fn sequence_wraps() {
        let mut jb = JitterBuffer::new(48000, 1, 10, 1);
        jb.push(u32::MAX, vec![1; 10]);
        jb.push(0, vec![2; 10]);
        assert_eq!(jb.stats.late, 0);
        assert_eq!(jb.packets.len(), 2);
    }

    #[test]
    fn resamples() {
        let mut jb = JitterBuffer::new(48000, 2, 480, 10);
        for seq in 0..10 {
            jb.push(seq, packet(1000));
        }
        let mut out = vec![0f32; 441 * 2];
        jb.fill(&mut out, 2, 44100);
        jb.fill(&mut out, 2, 44100);
        assert!((out[400] * 32768.0 - 1000.0).abs() < 1.0);
    }
}
