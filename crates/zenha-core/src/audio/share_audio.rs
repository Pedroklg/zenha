//! Stream sound: per-process WASAPI loopback, in 10 ms frames, stereo, 48 kHz.
//! Only compiles on Windows.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use wasapi::{
    AudioClient, Direction, SampleType, StreamMode, WaveFormat, deinitialize, initialize_mta,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use crate::backend::support::{drain_f32_le_to_i16, parse_target_id};
use crate::video::{CaptureKind, CaptureTarget};

pub(crate) const SHARE_AUDIO_SAMPLE_RATE: u32 = 48_000;
pub(crate) const SHARE_AUDIO_CHANNELS: u32 = 2;
const FRAME_SAMPLES: usize = 480 * SHARE_AUDIO_CHANNELS as usize;
const BYTES_PER_SAMPLE: usize = 4;
const FRAME_BYTES: usize = FRAME_SAMPLES * BYTES_PER_SAMPLE;
const MAX_BACKLOG_BYTES: usize = FRAME_BYTES * 20;
// Events stop while nothing plays, so the wait doubles as the stop-flag poll.
const EVENT_TIMEOUT_MS: u32 = 200;

/// Whose sound goes with a screen share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioScope {
    /// One application, child processes included.
    Process(u32),
    /// Everything except this app, so the voices it plays are not sent back to the room.
    AllButSelf,
}

impl AudioScope {
    /// A shared window carries the sound of its application, a screen the whole system.
    /// `ZENHA_SHARE_AUDIO_PID` (diagnostic) forces the sound of one process.
    pub(crate) fn for_target(target: &CaptureTarget) -> Option<Self> {
        if let Some(process) = std::env::var("ZENHA_SHARE_AUDIO_PID")
            .ok()
            .and_then(|value| value.trim().parse().ok())
        {
            return Some(Self::Process(process));
        }
        match parse_target_id(&target.id) {
            Some((CaptureKind::Window, window)) => {
                let process = window_process_id(window)?;
                (process != std::process::id()).then_some(Self::Process(process))
            }
            _ => Some(Self::AllButSelf),
        }
    }
}

fn window_process_id(window: u64) -> Option<u32> {
    let mut process = 0u32;
    #[expect(
        unsafe_code,
        reason = "Win32 GetWindowThreadProcessId has no safe wrapper in the windows crate"
    )]
    // SAFETY: the call only reads the handle value and writes the process id to a live local;
    // a stale or invalid handle makes it return 0 without touching memory.
    let thread = unsafe {
        GetWindowThreadProcessId(
            HWND(window as usize as *mut core::ffi::c_void),
            Some(&mut process),
        )
    };
    (thread != 0 && process != 0).then_some(process)
}

/// Capture thread; dropping it stops the capture.
pub(crate) struct ShareAudioCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ShareAudioCapture {
    /// `sink` receives interleaved stereo frames of 10 ms; nothing arrives while nothing plays.
    pub(crate) fn start(scope: AudioScope, mut sink: impl FnMut(&[i16]) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::Builder::new()
            .name("share-audio".to_owned())
            .spawn(move || {
                if initialize_mta().is_err() {
                    tracing::warn!("share audio: could not initialise COM");
                    return;
                }
                match capture(scope, &flag, &mut sink) {
                    Ok(()) => tracing::info!("share audio capture stopped"),
                    Err(err) => tracing::warn!(%err, "share audio capture failed"),
                }
                deinitialize();
            })
            .map_err(|err| tracing::warn!(%err, "could not start the share audio thread"))
            .ok();
        Self { stop, thread }
    }
}

impl Drop for ShareAudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture(
    scope: AudioScope,
    stop: &AtomicBool,
    sink: &mut impl FnMut(&[i16]),
) -> Result<(), wasapi::WasapiError> {
    let (process, include_tree) = match scope {
        AudioScope::Process(process) => (process, true),
        AudioScope::AllButSelf => (std::process::id(), false),
    };
    let mut client = AudioClient::new_application_loopback_client(process, include_tree)?;
    let format = WaveFormat::new(
        32,
        32,
        SampleType::Float,
        SHARE_AUDIO_SAMPLE_RATE as usize,
        SHARE_AUDIO_CHANNELS as usize,
        None,
    );
    client.initialize_client(
        &format,
        Direction::Capture,
        StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 0,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let reader = client.get_audiocaptureclient()?;
    client.start_stream()?;
    tracing::info!(?scope, "share audio capture started");

    let mut bytes: VecDeque<u8> = VecDeque::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut frame = [0i16; FRAME_SAMPLES];
    while !stop.load(Ordering::SeqCst) {
        while let Some(frames) = reader.get_next_packet_size()? {
            if frames == 0 {
                break;
            }
            reader.read_from_device_to_deque(&mut bytes)?;
        }
        if bytes.len() > MAX_BACKLOG_BYTES {
            let excess = (bytes.len() - MAX_BACKLOG_BYTES).div_ceil(FRAME_BYTES) * FRAME_BYTES;
            bytes.drain(..excess.min(bytes.len()));
        }
        while drain_f32_le_to_i16(&mut bytes, &mut frame, &mut scratch) {
            sink(&frame);
        }
        let _ = event.wait_for_event(EVENT_TIMEOUT_MS);
    }
    client.stop_stream()
}
