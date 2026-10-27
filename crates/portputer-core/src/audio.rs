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
            let mut on_audio = on_audio;
            let err = |e| tracing::warn!("capture: {e}");
            let stream = match supported.sample_format() {
                cpal::SampleFormat::I16 => {
                    let mut buf = Vec::new();
                    device.build_input_stream::<i16, _, _>(
                        config,
                        move |data, _| {
                            buf.clear();
                            buf.extend(data.iter().map(|s| *s as f32 / 32768.0));
                            on_audio(&buf, ch);
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
                            on_audio(&buf, ch);
                        },
                        err,
                        None,
                    )
                }
                _ => device.build_input_stream::<f32, _, _>(config, move |data, _| on_audio(data, ch), err, None),
            }
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
                .sharing_mode(AudioSharingMode::Shared)
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
        let uid = CFString::from_str(FEED_UID);
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
}
