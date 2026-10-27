//! The client's audio buffer: puts packets back in sequence order, holds
//! audio to absorb network jitter, conceals lost packets, and resamples to
//! the output device's rate, running slightly fast or slow to keep the buffer
//! at its target as the two computers' clocks drift apart.
//!
//! The target is set in milliseconds, or automatic (0): the measured jitter
//! of the last few seconds plus a margin. Audio held beyond the target after
//! a burst (a stall on a TCP path, then everything at once) is dropped, with
//! a crossfade, instead of being played late.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

/// Packets further ahead than this restart the stream (the server restarted
/// or the client fell far behind).
const MAX_AHEAD: u32 = 400;
/// The most the playback speed is changed to correct drift.
const MAX_CORRECTION: f64 = 0.01;
/// Lost packets in a row that are concealed by repeating the last one, quieter
/// each time, before silence.
const CONCEAL_REPEATS: u32 = 3;
/// Automatic target: the jitter percentile covered, the margin added, and
/// the limits.
const AUTO_PERCENTILE: f64 = 0.98;
const AUTO_MARGIN_MS: f64 = 6.0;
const AUTO_MIN_MS: f64 = 12.0;
const AUTO_MAX_MS: f64 = 300.0;
/// Seconds of arrival times the automatic target is measured over.
const AUTO_WINDOW_SECS: f64 = 4.0;
/// Seconds of playback the buffer's low point is measured over before excess
/// audio is dropped.
const DRAIN_WINDOW_SECS: f64 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JitterStats {
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub underruns: u64,
    /// Packets dropped to remove excess delay.
    pub dropped: u64,
    /// Audio waiting to be played, in milliseconds.
    pub buffered_ms: u32,
    /// The buffer's current target, in milliseconds.
    pub target_ms: u32,
}

pub struct JitterBuffer {
    rate: u32,
    channels: usize,
    frames_per_packet: usize,
    target_frames: usize,
    auto: bool,
    /// Arrival time and sequence number of the first packet: the reference
    /// for each packet's transit time.
    epoch: Option<(Instant, u64)>,
    /// Recent transit times in seconds, relative to the epoch.
    transits: VecDeque<f64>,
    pushes: u32,
    /// The lowest buffer level seen in the drain window, and the source
    /// frames played in it.
    window_min: usize,
    window_frames: usize,
    /// Packets to skip at the next packet boundary.
    drop: usize,
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
        let mut buffer = JitterBuffer {
            rate,
            channels,
            frames_per_packet: frames_per_packet.max(1) as usize,
            target_frames: 0,
            auto: false,
            epoch: None,
            transits: VecDeque::new(),
            pushes: 0,
            window_min: usize::MAX,
            window_frames: 0,
            drop: 0,
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
        };
        buffer.set_target_ms(target_ms);
        buffer
    }

    /// Milliseconds of audio to hold, or 0 for automatic.
    pub fn set_target_ms(&mut self, ms: u32) {
        self.auto = ms == 0;
        let ms = if self.auto { self.auto_target_ms().unwrap_or(40.0) } else { ms as f64 };
        self.set_target_frames(ms);
    }

    fn set_target_frames(&mut self, ms: f64) {
        self.target_frames = ((self.rate as f64 * ms / 1000.0) as usize).max(self.frames_per_packet);
        self.stats.target_ms = (self.target_frames as u64 * 1000 / self.rate.max(1) as u64) as u32;
    }

    fn packet_secs(&self) -> f64 {
        self.frames_per_packet as f64 / self.rate.max(1) as f64
    }

    /// The jitter of the recent transit times plus the margin, once there
    /// are enough of them.
    fn auto_target_ms(&self) -> Option<f64> {
        if self.transits.len() < 50 {
            return None;
        }
        let mut sorted: Vec<f64> = self.transits.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let high = sorted[((sorted.len() - 1) as f64 * AUTO_PERCENTILE) as usize];
        let jitter_ms = (high - sorted[0]) * 1000.0;
        Some((jitter_ms + AUTO_MARGIN_MS).clamp(AUTO_MIN_MS, AUTO_MAX_MS))
    }

    /// Records a packet's transit time and, every 20 packets, moves the
    /// automatic target: up at once, down gradually.
    fn measure(&mut self, seq: u64, at: Instant) {
        let (start, first) = *self.epoch.get_or_insert((at, seq));
        let transit = at.duration_since(start).as_secs_f64() - (seq as i64 - first as i64) as f64 * self.packet_secs();
        let capacity = ((AUTO_WINDOW_SECS / self.packet_secs()) as usize).clamp(50, 4000);
        if self.transits.len() >= capacity {
            self.transits.pop_front();
        }
        self.transits.push_back(transit);
        self.pushes += 1;
        if self.auto && self.pushes % 20 == 0 {
            if let Some(want) = self.auto_target_ms() {
                let current = self.target_frames as f64 * 1000.0 / self.rate as f64;
                self.set_target_frames(if want > current { want } else { current + (want - current) * 0.1 });
            }
        }
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
        self.push_at(seq, samples, Instant::now());
    }

    pub fn push_at(&mut self, seq: u32, samples: Vec<i16>, at: Instant) {
        let seq = self.unwrap(seq);
        self.last_seq = Some(self.last_seq.map_or(seq, |l| l.max(seq)));
        self.stats.received += 1;
        // Late packets count too: they show the target is too small.
        self.measure(seq, at);
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
                self.epoch = None;
                self.transits.clear();
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
        let Some(mut next) = self.next else { return false };
        self.position = 0;
        // Excess delay: skip ahead, fading from the packet that was due.
        let mut fade_from = None;
        if self.drop > 0 {
            let to = next + std::mem::take(&mut self.drop) as u64;
            if self.packets.range(to..).next().is_some() {
                fade_from = Some(self.packets.remove(&next).unwrap_or_else(|| self.last_packet.clone()));
                self.packets = self.packets.split_off(&to);
                self.stats.dropped += to - next;
                next = to;
            }
        }
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
        if let Some(from) = fade_from {
            let frames = (self.current.len() / self.channels).max(1);
            for (i, sample) in self.current.iter_mut().enumerate() {
                let t = (i / self.channels) as f32 / frames as f32;
                let old = from.get(i).copied().unwrap_or(0) as f32;
                *sample = (old * (1.0 - t) + *sample as f32 * t) as i16;
            }
        }
        self.next = Some(next + 1);
        true
    }

    /// Tracks the buffer's low point while playing; when it stayed more than
    /// a packet above the target for the whole window, the excess is dropped.
    fn check_excess(&mut self, level: usize, source_frames: usize) {
        if !self.playing {
            self.window_min = usize::MAX;
            self.window_frames = 0;
            return;
        }
        self.window_min = self.window_min.min(level);
        self.window_frames += source_frames;
        if self.window_frames as f64 >= self.rate as f64 * DRAIN_WINDOW_SECS {
            if self.window_min > self.target_frames + self.frames_per_packet {
                self.drop = (self.window_min - self.target_frames) / self.frames_per_packet;
            }
            self.window_min = usize::MAX;
            self.window_frames = 0;
        }
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
        self.check_excess(level as usize, (out.len() / out_channels) * self.rate as usize / out_rate.max(1) as usize);
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
        self.epoch = None;
        self.transits.clear();
        self.drop = 0;
        self.window_min = usize::MAX;
        self.window_frames = 0;
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
    fn automatic_target_follows_jitter() {
        let start = Instant::now();
        let ms = |n: f64| start + std::time::Duration::from_secs_f64(n / 1000.0);
        // Steady arrivals every 5 ms: the minimum target.
        let mut jb = JitterBuffer::new(48000, 2, 240, 0);
        for seq in 0..1000u32 {
            jb.push_at(seq, packet(1), ms(seq as f64 * 5.0));
        }
        assert_eq!(jb.stats.target_ms, AUTO_MIN_MS as u32);
        // Every fourth packet 30 ms late: about 36 ms.
        let mut jb = JitterBuffer::new(48000, 2, 240, 0);
        for seq in 0..800u32 {
            let late = if seq % 4 == 0 { 30.0 } else { 0.0 };
            jb.push_at(seq, packet(1), ms(seq as f64 * 5.0 + late));
        }
        assert!((34..=38).contains(&jb.stats.target_ms), "{}", jb.stats.target_ms);
    }

    #[test]
    fn drops_excess_after_a_burst() {
        let mut jb = JitterBuffer::new(48000, 2, 240, 20);
        let mut out = vec![0f32; 240 * 2];
        let mut seq = 0u32;
        for _ in 0..4 {
            jb.push(seq, packet(1));
            seq += 1;
        }
        // A stall delivers 200 ms at once on top of the 20 ms target.
        for _ in 0..40 {
            jb.push(seq, packet(1));
            seq += 1;
        }
        // A second of playback with packets arriving in step.
        for _ in 0..200 {
            jb.fill(&mut out, 2, 48000);
            jb.push(seq, packet(1));
            seq += 1;
        }
        assert!(jb.stats.dropped >= 30, "dropped {}", jb.stats.dropped);
        assert!(jb.stats.buffered_ms <= 30, "buffered {} ms", jb.stats.buffered_ms);
        assert_eq!(jb.stats.underruns, 0);
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
