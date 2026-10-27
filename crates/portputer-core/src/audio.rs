//! Capturing the computer's sound output (servers) and playing the stream
//! (clients). Capture is cpal's loopback recording: a Core Audio process tap
//! on macOS 14.2 and later, WASAPI loopback on Windows. Playback is cpal on
//! computers and AAudio on Android.

use std::sync::{Arc, Mutex};

use crate::Error;
use crate::jitter::JitterBuffer;

/// A stream that stops when dropped. The streams live on their own threads,
/// as cpal's streams can't move between threads on every system.
pub struct StreamThread {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StreamThread {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct Capture {
    pub rate: u32,
    pub channels: u16,
    _thread: StreamThread,
}

/// Starts capturing what the computer plays. `on_audio` gets interleaved
/// samples and the channel count, on the audio thread, so it mustn't block.
#[cfg(not(target_os = "android"))]
pub fn start_capture(on_audio: impl FnMut(&[f32], usize) + Send + 'static) -> Result<Capture, Error> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    if std::env::var_os("PORTPUTER_TEST_TONE").is_some() {
        return Ok(test_tone(on_audio));
    }
    #[cfg(target_os = "macos")]
    if loopback::in_use() {
        return loopback::start(on_audio);
    }

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(u32, u16), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new().name("portputer-capture".into()).spawn(move || {
        let start = || -> Result<(cpal::Stream, u32, u16), String> {
            let host = cpal::default_host();
            let device = host.default_output_device().ok_or("no audio output device")?;
            let supported = device.default_output_config().map_err(|e| e.to_string())?;
            let config = supported.config();
            let (rate, channels) = (config.sample_rate, config.channels);
            let ch = channels as usize;
            let on_audio = Arc::new(Mutex::new(on_audio));
            let build = |config: cpal::StreamConfig| {
                let on_audio = on_audio.clone();
                let err = |e| tracing::warn!("capture: {e}");
                match supported.sample_format() {
                    cpal::SampleFormat::I16 => {
                        let mut buf = Vec::new();
                        device.build_input_stream::<i16, _, _>(
                            config,
                            move |data, _| {
                                buf.clear();
                                buf.extend(data.iter().map(|s| *s as f32 / 32768.0));
                                (on_audio.lock().unwrap())(&buf, ch);
                            },
                            err,
                            None,
                        )
                    }
                    cpal::SampleFormat::I32 => {
                        let mut buf = Vec::new();
                        device.build_input_stream::<i32, _, _>(
                            config,
                            move |data, _| {
                                buf.clear();
                                buf.extend(data.iter().map(|s| *s as f32 / 2147483648.0));
                                (on_audio.lock().unwrap())(&buf, ch);
                            },
                            err,
                            None,
                        )
                    }
                    _ => device.build_input_stream::<f32, _, _>(config, move |data, _| (on_audio.lock().unwrap())(data, ch), err, None),
                }
            };
            // 5 ms device buffers, the packet size, where the device takes
            // them; its default otherwise.
            let small = match supported.buffer_size() {
                cpal::SupportedBufferSize::Range { min, max } => {
                    Some(cpal::StreamConfig { buffer_size: cpal::BufferSize::Fixed((rate / 200).clamp(*min, *max)), ..config.clone() })
                }
                _ => None,
            };
            let stream = match small.map(&build) {
                Some(Ok(stream)) => stream,
                _ => build(config).map_err(|e| e.to_string())?,
            };
            stream.play().map_err(|e| e.to_string())?;
            Ok((stream, rate, channels))
        };
        match start() {
            Ok((stream, rate, channels)) => {
                let _ = ready_tx.send(Ok((rate, channels)));
                let _ = stop_rx.recv();
                drop(stream);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    })?;
    match ready_rx.recv() {
        Ok(Ok((rate, channels))) => Ok(Capture {
            rate,
            channels,
            _thread: StreamThread { stop: Some(stop_tx), thread: Some(thread) },
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(Error::Audio(e))
        }
        Err(_) => Err(Error::Audio("the capture thread ended".into())),
    }
}

/// A 440 Hz tone in place of the computer's sound, for testing the stream
/// without capture permission: set PORTPUTER_TEST_TONE.
pub fn test_tone(mut on_audio: impl FnMut(&[f32], usize) + Send + 'static) -> Capture {
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let mut phase = 0f32;
        let mut buf = vec![0f32; 480 * 2];
        let start = std::time::Instant::now();
        let mut sent = 0u64;
        loop {
            match stop_rx.recv_timeout(std::time::Duration::from_millis(2)) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                _ => return,
            }
            // Keep to real time: 480 frames every 10 ms.
            while sent * 10 <= start.elapsed().as_millis() as u64 {
                for frame in buf.chunks_mut(2) {
                    let v = (phase * std::f32::consts::TAU).sin() * 0.2;
                    frame[0] = v;
                    frame[1] = v;
                    phase = (phase + 440.0 / 48000.0).fract();
                }
                on_audio(&buf, 2);
                sent += 1;
            }
        }
    });
    Capture { rate: 48000, channels: 2, _thread: StreamThread { stop: Some(stop_tx), thread: Some(thread) } }
}

#[cfg(target_os = "android")]
pub fn start_capture(_on_audio: impl FnMut(&[f32], usize) + Send + 'static) -> Result<Capture, Error> {
    Err(Error::Audio("Android doesn't serve audio".into()))
}

pub struct Playback {
    _thread: StreamThread,
}

/// Plays a jitter buffer through the default output device.
pub fn start_playback(buffer: Arc<Mutex<JitterBuffer>>) -> Result<Playback, Error> {
    start_playback_on(buffer, None)
}

/// Plays a jitter buffer through the named output device, or the default.
#[cfg(not(target_os = "android"))]
pub fn start_playback_on(buffer: Arc<Mutex<JitterBuffer>>, device_name: Option<String>) -> Result<Playback, Error> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new().name("portputer-playback".into()).spawn(move || {
        let start = || -> Result<cpal::Stream, String> {
            let host = cpal::default_host();
            let device = match &device_name {
                Some(name) => host
                    .output_devices()
                    .map_err(|e| e.to_string())?
                    .find(|d| d.description().is_ok_and(|desc| desc.name() == name))
                    .ok_or_else(|| format!("no audio device named {name}"))?,
                None => host.default_output_device().ok_or("no audio output device")?,
            };
            let supported = device.default_output_config().map_err(|e| e.to_string())?;
            let mut config = supported.config();
            // Small device buffers, for latency, where the device allows.
            if let cpal::SupportedBufferSize::Range { min, max } = supported.buffer_size() {
                let want = (config.sample_rate / 200).clamp(*min, *max);
                config.buffer_size = cpal::BufferSize::Fixed(want);
            }
            let (rate, ch) = (config.sample_rate, config.channels as usize);
            let stream = device
                .build_output_stream::<f32, _, _>(
                    config,
                    move |data, _| match buffer.lock() {
                        Ok(mut b) => b.fill(data, ch, rate),
                        Err(_) => data.iter_mut().for_each(|s| *s = 0.0),
                    },
                    |e| tracing::warn!("playback: {e}"),
                    None,
                )
                .map_err(|e| e.to_string())?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(stream)
        };
        match start() {
            Ok(stream) => {
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv();
                drop(stream);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    })?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Playback { _thread: StreamThread { stop: Some(stop_tx), thread: Some(thread) } }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(Error::Audio(e))
        }
        Err(_) => Err(Error::Audio("the playback thread ended".into())),
    }
}

/// Plays a jitter buffer through AAudio in low-latency mode.
#[cfg(target_os = "android")]
pub fn start_playback_on(buffer: Arc<Mutex<JitterBuffer>>, _device_name: Option<String>) -> Result<Playback, Error> {
    use ndk::audio::{
        AudioCallbackResult, AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode,
        AudioStreamBuilder,
    };

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let rate = buffer.lock().map(|b| b.rate()).unwrap_or(48000);
    let thread = std::thread::Builder::new().name("portputer-playback".into()).spawn(move || {
        let start = || -> Result<ndk::audio::AudioStream, String> {
            let stream = AudioStreamBuilder::new()
                .map_err(|e| e.to_string())?
                .direction(AudioDirection::Output)
                .performance_mode(AudioPerformanceMode::LowLatency)
                // Exclusive (MMAP) where the device has it; AAudio opens a
                // shared stream otherwise.
                .sharing_mode(AudioSharingMode::Exclusive)
                .format(AudioFormat::PCM_Float)
                .channel_count(2)
                .sample_rate(rate as i32)
                .data_callback(Box::new(move |stream, data, frames| {
                    let channels = stream.channel_count().max(1) as usize;
                    let out_rate = stream.sample_rate().max(1) as u32;
                    let len = frames as usize * channels;
                    let out = unsafe { std::slice::from_raw_parts_mut(data as *mut f32, len) };
                    match buffer.lock() {
                        Ok(mut b) => b.fill(out, channels, out_rate),
                        Err(_) => out.iter_mut().for_each(|s| *s = 0.0),
                    }
                    AudioCallbackResult::Continue
                }))
                .open_stream()
                .map_err(|e| e.to_string())?;
            // Two bursts: the smallest buffer that doesn't glitch.
            let _ = stream.set_buffer_size_in_frames(stream.frames_per_burst() * 2);
            stream.request_start().map_err(|e| e.to_string())?;
            Ok(stream)
        };
        match start() {
            Ok(stream) => {
                let _ = ready_tx.send(Ok(()));
                let _ = stop_rx.recv();
                let _ = stream.request_stop();
                drop(stream);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    })?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Playback { _thread: StreamThread { stop: Some(stop_tx), thread: Some(thread) } }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(Error::Audio(e))
        }
        Err(_) => Err(Error::Audio("the playback thread ended".into())),
    }
}

/// Records the default input device (the microphone). `on_audio` gets
/// interleaved samples, the channel count and the sample rate, on the audio
/// thread.
#[cfg(not(target_os = "android"))]
pub fn start_input(mut on_audio: impl FnMut(&[f32], usize, u32) + Send + 'static) -> Result<Capture, Error> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(u32, u16), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new().name("portputer-microphone".into()).spawn(move || {
        let start = || -> Result<(cpal::Stream, u32, u16), String> {
            let device = cpal::default_host().default_input_device().ok_or("no microphone")?;
            let config = device.default_input_config().map_err(|e| e.to_string())?.config();
            let (rate, channels) = (config.sample_rate, config.channels);
            let ch = channels as usize;
            let stream = device
                .build_input_stream::<f32, _, _>(config, move |data, _| on_audio(data, ch, rate), |e| tracing::warn!("microphone: {e}"), None)
                .map_err(|e| e.to_string())?;
            stream.play().map_err(|e| e.to_string())?;
            Ok((stream, rate, channels))
        };
        match start() {
            Ok((stream, rate, channels)) => {
                let _ = ready_tx.send(Ok((rate, channels)));
                let _ = stop_rx.recv();
                drop(stream);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    })?;
    match ready_rx.recv() {
        Ok(Ok((rate, channels))) => Ok(Capture { rate, channels, _thread: StreamThread { stop: Some(stop_tx), thread: Some(thread) } }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(Error::Audio(e))
        }
        Err(_) => Err(Error::Audio("the microphone thread ended".into())),
    }
}

/// Records the microphone through AAudio: 48 kHz mono, low latency. Needs the
/// app's RECORD_AUDIO permission.
#[cfg(target_os = "android")]
pub fn start_input(mut on_audio: impl FnMut(&[f32], usize, u32) + Send + 'static) -> Result<Capture, Error> {
    use ndk::audio::{AudioCallbackResult, AudioDirection, AudioFormat, AudioPerformanceMode, AudioSharingMode, AudioStreamBuilder};

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(u32, u16), String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new().name("portputer-microphone".into()).spawn(move || {
        let start = || -> Result<ndk::audio::AudioStream, String> {
            let stream = AudioStreamBuilder::new()
                .map_err(|e| e.to_string())?
                .direction(AudioDirection::Input)
                .performance_mode(AudioPerformanceMode::LowLatency)
                .sharing_mode(AudioSharingMode::Shared)
                .format(AudioFormat::PCM_Float)
                .channel_count(1)
                .sample_rate(48000)
                .data_callback(Box::new(move |stream, data, frames| {
                    let channels = stream.channel_count().max(1) as usize;
                    let rate = stream.sample_rate().max(1) as u32;
                    let samples = unsafe { std::slice::from_raw_parts(data as *const f32, frames as usize * channels) };
                    on_audio(samples, channels, rate);
                    AudioCallbackResult::Continue
                }))
                .open_stream()
                .map_err(|e| e.to_string())?;
            stream.request_start().map_err(|e| e.to_string())?;
            Ok(stream)
        };
        match start() {
            Ok(stream) => {
                let rate = stream.sample_rate() as u32;
                let _ = ready_tx.send(Ok((rate, 1)));
                let _ = stop_rx.recv();
                let _ = stream.request_stop();
                drop(stream);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        }
    })?;
    match ready_rx.recv() {
        Ok(Ok((rate, channels))) => Ok(Capture { rate, channels, _thread: StreamThread { stop: Some(stop_tx), thread: Some(thread) } }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(Error::Audio(e))
        }
        Err(_) => Err(Error::Audio("the microphone thread ended".into())),
    }
}

/// Turns any input into 48 kHz mono, by linear interpolation.
pub struct ToMono48k {
    position: f64,
    last: f32,
}

impl Default for ToMono48k {
    fn default() -> Self {
        ToMono48k { position: 0.0, last: 0.0 }
    }
}

impl ToMono48k {
    pub fn push(&mut self, samples: &[f32], channels: usize, rate: u32, out: &mut Vec<f32>) {
        let channels = channels.max(1);
        let step = rate as f64 / 48000.0;
        let mono: Vec<f32> = samples.chunks(channels).map(|f| f.iter().sum::<f32>() / f.len() as f32).collect();
        if rate == 48000 {
            out.extend_from_slice(&mono);
            return;
        }
        // Positions are in input frames, relative to the start of this chunk,
        // with frame -1 being the last one of the chunk before.
        while self.position < mono.len() as f64 - 1.0 {
            let i = self.position.floor();
            let frac = (self.position - i) as f32;
            let a = if i < 0.0 { self.last } else { mono[i as usize] };
            let b = mono[(i + 1.0) as usize];
            out.push(a + (b - a) * frac);
            self.position += step;
        }
        self.position -= mono.len() as f64;
        self.last = *mono.last().unwrap_or(&0.0);
    }
}

/// Playing into Portputer Microphone Feed, the hidden output device of the
/// Portputer Microphone driver (macos/Driver), with a Core Audio IOProc of
/// our own: hidden devices aren't listed, so cpal can't open them.
#[cfg(target_os = "macos")]
pub mod feed {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::{Arc, Mutex};

    use objc2_core_audio::{
        AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
        AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, kAudioHardwarePropertyTranslateUIDToDevice,
        kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    };
    use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
    use objc2_core_foundation::CFString;

    use crate::Error;
    use crate::jitter::JitterBuffer;

    pub const FEED_UID: &str = "io.github.aaron-gh.portputer.microphone-feed";

    fn device() -> Option<AudioObjectID> {
        device_for(FEED_UID)
    }

    /// The device with this UID, if Core Audio has it.
    pub(crate) fn device_for(uid: &str) -> Option<AudioObjectID> {
        let uid = CFString::from_str(uid);
        let uid_ref: *const CFString = &*uid;
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyTranslateUIDToDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut id: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                std::mem::size_of::<*const CFString>() as u32,
                &uid_ref as *const *const CFString as *const c_void,
                NonNull::from(&mut size),
                NonNull::from(&mut id).cast(),
            )
        };
        (status == 0 && id != 0).then_some(id)
    }

    /// Whether the driver is installed.
    pub fn installed() -> bool {
        device().is_some()
    }

    pub struct Feed {
        device: AudioObjectID,
        proc_id: AudioDeviceIOProcID,
        context: *mut Arc<Mutex<JitterBuffer>>,
    }

    // The IOProc's context is only touched by Core Audio's thread and freed
    // after the IOProc is destroyed.
    unsafe impl Send for Feed {}

    unsafe extern "C-unwind" fn io_proc(
        _device: AudioObjectID,
        _now: NonNull<AudioTimeStamp>,
        _input: NonNull<AudioBufferList>,
        _input_time: NonNull<AudioTimeStamp>,
        output: NonNull<AudioBufferList>,
        _output_time: NonNull<AudioTimeStamp>,
        context: *mut c_void,
    ) -> i32 {
        let buffer = unsafe { &*(context as *const Arc<Mutex<JitterBuffer>>) };
        let list = unsafe { output.as_ptr().as_mut().unwrap() };
        let buffers = unsafe { std::slice::from_raw_parts_mut(list.mBuffers.as_mut_ptr(), list.mNumberBuffers as usize) };
        for b in buffers {
            if b.mData.is_null() {
                continue;
            }
            let samples = unsafe { std::slice::from_raw_parts_mut(b.mData as *mut f32, b.mDataByteSize as usize / 4) };
            match buffer.lock() {
                Ok(mut j) => j.fill(samples, b.mNumberChannels.max(1) as usize, 48000),
                Err(_) => samples.iter_mut().for_each(|s| *s = 0.0),
            }
        }
        0
    }

    pub fn start(buffer: Arc<Mutex<JitterBuffer>>) -> Result<Feed, Error> {
        let device = device().ok_or_else(|| Error::Audio("The Portputer Microphone driver isn't installed on this Mac.".into()))?;
        let context = Box::into_raw(Box::new(buffer));
        let mut proc_id: AudioDeviceIOProcID = None;
        let status = unsafe { AudioDeviceCreateIOProcID(device, Some(io_proc), context as *mut c_void, NonNull::from(&mut proc_id)) };
        if status != 0 {
            drop(unsafe { Box::from_raw(context) });
            return Err(Error::Audio(format!("Core Audio error {status} opening the microphone driver")));
        }
        let status = unsafe { AudioDeviceStart(device, proc_id) };
        if status != 0 {
            unsafe { AudioDeviceDestroyIOProcID(device, proc_id) };
            drop(unsafe { Box::from_raw(context) });
            return Err(Error::Audio(format!("Core Audio error {status} starting the microphone driver")));
        }
        Ok(Feed { device, proc_id, context })
    }

    impl Drop for Feed {
        fn drop(&mut self) {
            unsafe {
                AudioDeviceStop(self.device, self.proc_id);
                AudioDeviceDestroyIOProcID(self.device, self.proc_id);
                drop(Box::from_raw(self.context));
            }
        }
    }

    /// What this process may do with audio, for the login window's server,
    /// which runs as root with no one to grant permissions: plays a tone into
    /// the driver's hidden output for a second and records it back from
    /// Portputer Microphone, then sets the default output device to the one
    /// it already is. Returns a line for the log.
    pub fn probe() -> String {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use objc2_core_audio::{AudioObjectSetPropertyData, kAudioHardwarePropertyDefaultOutputDevice};

        if !installed() {
            return "audio probe: the driver isn't installed".into();
        }
        // A 440 Hz tone, 5 ms packets, kept ahead of the feed's playback.
        let jitter = Arc::new(Mutex::new(JitterBuffer::new(48000, 1, 240, 20)));
        let tone = jitter.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let writer = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut seq = 0u32;
            let mut phase = 0f32;
            while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                while (seq as u128) * 5 <= start.elapsed().as_millis() + 40 {
                    let packet: Vec<i16> = (0..240)
                        .map(|_| {
                            phase = (phase + 440.0 / 48000.0).fract();
                            ((phase * std::f32::consts::TAU).sin() * 8000.0) as i16
                        })
                        .collect();
                    tone.lock().unwrap().push(seq, packet);
                    seq += 1;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        let feed = start(jitter);
        let peak = Arc::new(Mutex::new((0f32, 0usize)));
        let record = || -> Result<cpal::Stream, String> {
            let device = cpal::default_host()
                .input_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.description().is_ok_and(|desc| desc.name() == "Portputer Microphone"))
                .ok_or("no Portputer Microphone input")?;
            let config = device.default_input_config().map_err(|e| e.to_string())?.config();
            let seen = peak.clone();
            let stream = device
                .build_input_stream::<f32, _, _>(
                    config,
                    move |data, _| {
                        let mut p = seen.lock().unwrap();
                        p.0 = data.iter().fold(p.0, |m, s| m.max(s.abs()));
                        p.1 += 1;
                    },
                    |e| tracing::warn!("audio probe: {e}"),
                    None,
                )
                .map_err(|e| e.to_string())?;
            stream.play().map_err(|e| e.to_string())?;
            Ok(stream)
        };
        let recording = record();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = writer.join();
        let (level, callbacks) = *peak.lock().unwrap();
        let input = match (&feed, &recording) {
            (Err(e), _) => format!("feed failed: {e}"),
            (_, Err(e)) => format!("recording failed: {e}"),
            _ if callbacks == 0 => "recording started but delivered nothing".into(),
            _ => format!("recorded {callbacks} buffers, peak {level:.3} ({})", if level > 0.05 { "the tone came back" } else { "silence" }),
        };
        drop(recording);
        drop(feed);

        let address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let mut output: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        let get = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut output).cast(),
            )
        };
        let set = unsafe {
            AudioObjectSetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&address),
                0,
                std::ptr::null(),
                std::mem::size_of::<AudioObjectID>() as u32,
                NonNull::from(&output).cast(),
            )
        };
        format!("audio probe: {input}; default output {output} (read {get}, set to itself {set})")
    }
}

/// Computer audio at the Mac login window. macOS lets no one there record
/// system audio (taps and inputs both need permissions that nobody at the
/// login window can grant), so Portputer's driver does it: while a device
/// listens, Portputer Speakers is made the default output, beside the
/// speakers in a private multi-output device (or alone, with Mute Computer
/// Speakers), and the driver passes what's played to it to the helper
/// daemon, which passes it on here. The outputs are put back afterwards.
#[cfg(target_os = "macos")]
pub mod loopback {
    use std::ptr::NonNull;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use objc2_core_audio::{
        AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice, AudioObjectGetPropertyData, AudioObjectID,
        AudioObjectPropertyAddress, AudioObjectPropertySelector, AudioObjectSetPropertyData, kAudioDevicePropertyDeviceUID,
        kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
        kAudioHardwarePropertyDefaultSystemOutputDevice, kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
        kAudioObjectSystemObject,
    };
    use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};

    use super::{Capture, StreamThread};
    use crate::Error;
    use crate::platform::macos::{HELPER_SOCKET, LoopbackChunk, helper_audio};

    pub const SPEAKERS_UID: &str = "io.github.aaron-gh.portputer.speakers";

    static IN_USE: AtomicBool = AtomicBool::new(false);

    /// Set by the server at the login window: its capture goes through here.
    pub fn use_for_capture(on: bool) {
        IN_USE.store(on, Ordering::Relaxed);
    }

    pub fn in_use() -> bool {
        IN_USE.load(Ordering::Relaxed)
    }

    /// Whether the driver has Portputer Speakers and the helper is there.
    pub fn available() -> bool {
        super::feed::device_for(SPEAKERS_UID).is_some() && std::path::Path::new(HELPER_SOCKET).exists()
    }

    struct Routing {
        speakers: AudioObjectID,
        previous: AudioObjectID,
        previous_system: AudioObjectID,
        aggregate: Option<AudioObjectID>,
    }

    static ROUTING: Mutex<Option<Routing>> = Mutex::new(None);

    fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress { mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain }
    }

    fn get_device(selector: AudioObjectPropertySelector) -> AudioObjectID {
        let a = address(selector);
        let mut id: AudioObjectID = 0;
        let mut size = std::mem::size_of::<AudioObjectID>() as u32;
        unsafe {
            AudioObjectGetPropertyData(kAudioObjectSystemObject as AudioObjectID, NonNull::from(&a), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut id).cast())
        };
        id
    }

    fn set_device(selector: AudioObjectPropertySelector, id: AudioObjectID) -> i32 {
        let a = address(selector);
        unsafe {
            AudioObjectSetPropertyData(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&a),
                0,
                std::ptr::null(),
                std::mem::size_of::<AudioObjectID>() as u32,
                NonNull::from(&id).cast(),
            )
        }
    }

    fn uid_of(device: AudioObjectID) -> Option<CFRetained<CFString>> {
        let a = address(kAudioDevicePropertyDeviceUID);
        let mut uid: *const CFString = std::ptr::null();
        let mut size = std::mem::size_of::<*const CFString>() as u32;
        let status = unsafe { AudioObjectGetPropertyData(device, NonNull::from(&a), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut uid).cast()) };
        if status != 0 {
            return None;
        }
        NonNull::new(uid as *mut CFString).map(|p| unsafe { CFRetained::from_raw(p) })
    }

    fn rate_of(device: AudioObjectID) -> f64 {
        let a = address(kAudioDevicePropertyNominalSampleRate);
        let mut rate = 0f64;
        let mut size = std::mem::size_of::<f64>() as u32;
        unsafe { AudioObjectGetPropertyData(device, NonNull::from(&a), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut rate).cast()) };
        rate
    }

    fn running_somewhere(device: AudioObjectID) -> bool {
        let a = address(objc2_core_audio::kAudioDevicePropertyDeviceIsRunningSomewhere);
        let mut running: u32 = 0;
        let mut size = 4u32;
        unsafe { AudioObjectGetPropertyData(device, NonNull::from(&a), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut running).cast()) };
        running != 0
    }

    /// The devices in an aggregate device, for checking it.
    fn members_of(aggregate: AudioObjectID) -> Vec<AudioObjectID> {
        let a = address(objc2_core_audio::kAudioAggregateDevicePropertyActiveSubDeviceList);
        let mut ids = [0 as AudioObjectID; 8];
        let mut size = std::mem::size_of_val(&ids) as u32;
        unsafe { AudioObjectGetPropertyData(aggregate, NonNull::from(&a), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::from(&mut ids).cast()) };
        ids[..size as usize / 4].to_vec()
    }

    /// The multi-output device's UID: one at a time, so one left by a server
    /// that stopped without putting the outputs back is replaced.
    const MULTI_OUTPUT_UID: &str = "io.github.aaron-gh.portputer.multi-output";

    /// A multi-output device: the output in use, which keeps the clock, and
    /// Portputer Speakers. Not private: macOS ignores a private one as the
    /// default output.
    fn make_aggregate(output: AudioObjectID) -> Result<AudioObjectID, String> {
        if let Some(old) = super::feed::device_for(MULTI_OUTPUT_UID) {
            unsafe { AudioHardwareDestroyAggregateDevice(old) };
        }
        let output_uid = uid_of(output).ok_or("the output device has no UID")?;
        let key = |k: &str| CFString::from_str(k);
        let sub = |uid: &CFString, drift: bool| -> CFRetained<CFDictionary<CFString, CFType>> {
            let (k_uid, k_drift) = (key("uid"), key("drift"));
            let flag = CFNumber::new_i32(drift as i32);
            CFDictionary::from_slices(&[&*k_uid, &*k_drift], &[uid.as_ref(), flag.as_ref()])
        };
        let speakers_uid = CFString::from_str(SPEAKERS_UID);
        let subdevices = CFArray::from_retained_objects(&[sub(&output_uid, false), sub(&speakers_uid, true)]);
        let (name, uid) = (CFString::from_str("Portputer Output"), CFString::from_str(MULTI_OUTPUT_UID));
        let (yes, no) = (CFNumber::new_i32(1), CFNumber::new_i32(0));
        let keys = [key("name"), key("uid"), key("private"), key("stacked"), key("master"), key("subdevices")];
        let key_refs: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
        let values: [&CFType; 6] = [name.as_ref(), uid.as_ref(), no.as_ref(), yes.as_ref(), output_uid.as_ref(), subdevices.as_ref()];
        let description = CFDictionary::<CFString, CFType>::from_slices(&key_refs, &values);
        let mut id: AudioObjectID = 0;
        let status = unsafe { AudioHardwareCreateAggregateDevice(description.as_opaque(), NonNull::from(&mut id)) };
        if status != 0 || id == 0 {
            return Err(format!("Core Audio error {status} making the multi-output device"));
        }
        Ok(id)
    }

    /// Plays to Portputer Speakers alone (muted) or beside the speakers.
    fn route(routing: &mut Routing, muted: bool) -> Result<(), String> {
        let target = if muted {
            routing.speakers
        } else {
            match routing.aggregate {
                Some(a) => a,
                None => {
                    let a = make_aggregate(routing.previous)?;
                    routing.aggregate = Some(a);
                    a
                }
            }
        };
        let status = set_device(kAudioHardwarePropertyDefaultOutputDevice, target);
        if status != 0 {
            return Err(format!("Core Audio error {status} setting the output device"));
        }
        set_device(kAudioHardwarePropertyDefaultSystemOutputDevice, target);
        Ok(())
    }

    fn restore() {
        if let Some(routing) = ROUTING.lock().unwrap().take() {
            set_device(kAudioHardwarePropertyDefaultOutputDevice, routing.previous);
            set_device(kAudioHardwarePropertyDefaultSystemOutputDevice, routing.previous_system);
            if let Some(a) = routing.aggregate {
                unsafe { AudioHardwareDestroyAggregateDevice(a) };
            }
        }
    }

    /// Mute Computer Speakers while listening at the login window.
    pub fn set_muted(muted: bool) -> Result<(), String> {
        match ROUTING.lock().unwrap().as_mut() {
            Some(routing) => route(routing, muted),
            None => Ok(()),
        }
    }

    /// Checks the routing in a logged-in session: a quiet tone for a second
    /// to the speakers and Portputer Speakers together, then the outputs as
    /// they were. The helper logs when the driver's audio reaches it.
    pub fn test() -> Result<String, String> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let speakers = super::feed::device_for(SPEAKERS_UID).ok_or("the driver has no Portputer Speakers; update it")?;
        let previous = get_device(kAudioHardwarePropertyDefaultOutputDevice);
        let mut routing = Routing { speakers, previous, previous_system: get_device(kAudioHardwarePropertyDefaultSystemOutputDevice), aggregate: None };
        route(&mut routing, false)?;
        let aggregate = routing.aggregate;
        *ROUTING.lock().unwrap() = Some(routing);
        let played = (|| -> Result<String, String> {
            let device = cpal::default_host().default_output_device().ok_or("no output device")?;
            let config = device.default_output_config().map_err(|e| e.to_string())?.config();
            let (rate, channels) = (config.sample_rate as f32, config.channels as usize);
            let mut phase = 0f32;
            let stream = device
                .build_output_stream::<f32, _, _>(
                    config,
                    move |data, _| {
                        for frame in data.chunks_mut(channels) {
                            phase = (phase + 660.0 / rate).fract();
                            let v = (phase * std::f32::consts::TAU).sin() * 0.05;
                            frame.iter_mut().for_each(|s| *s = v);
                        }
                    },
                    |e| tracing::warn!("test tone: {e}"),
                    None,
                )
                .map_err(|e| e.to_string())?;
            stream.play().map_err(|e| e.to_string())?;
            std::thread::sleep(std::time::Duration::from_millis(500));
            let running = running_somewhere(speakers);
            let members = aggregate.map(members_of).unwrap_or_default();
            std::thread::sleep(std::time::Duration::from_millis(500));
            Ok(format!("members {members:?}, Portputer Speakers running: {running}, default output {}", get_device(kAudioHardwarePropertyDefaultOutputDevice)))
        })();
        let rate = rate_of(speakers);
        restore();
        Ok(format!("multi-output device {aggregate:?}, Portputer Speakers {speakers} at {rate} Hz: {}; outputs put back", played?))
    }

    pub fn start(mut on_audio: impl FnMut(&[f32], usize) + Send + 'static) -> Result<Capture, Error> {
        let speakers = super::feed::device_for(SPEAKERS_UID)
            .ok_or_else(|| Error::Audio("Computer Audio at the login window needs the Portputer Microphone driver, from Portputer's Settings, Microphone.".into()))?;
        let mut stream = helper_audio().map_err(Error::Audio)?;
        restore();
        let previous = get_device(kAudioHardwarePropertyDefaultOutputDevice);
        let mut routing = Routing { speakers, previous, previous_system: get_device(kAudioHardwarePropertyDefaultSystemOutputDevice), aggregate: None };
        if let Err(e) = route(&mut routing, false) {
            if let Some(a) = routing.aggregate {
                unsafe { AudioHardwareDestroyAggregateDevice(a) };
            }
            return Err(Error::Audio(e));
        }
        *ROUTING.lock().unwrap() = Some(routing);
        let rate = rate_of(speakers).max(1.0) as u32;
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::Builder::new().name("portputer-loopback".into()).spawn(move || {
            loop {
                if !matches!(stop_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)) {
                    break;
                }
                match LoopbackChunk::read(&mut stream) {
                    Ok(chunk) => on_audio(&chunk.samples, chunk.channels as usize),
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                    Err(e) => {
                        tracing::warn!("Portputer Speakers: {e}");
                        break;
                    }
                }
            }
            restore();
        })?;
        Ok(Capture { rate, channels: 2, _thread: StreamThread { stop: Some(stop_tx), thread: Some(thread) } })
    }
}
