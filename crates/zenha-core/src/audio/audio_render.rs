//! Own audio output through WASAPI: stereo, 48 kHz, filled by a callback.
//! Only compiles on Windows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wasapi::{
    DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, deinitialize, initialize_mta,
};

use crate::audio::default_device::device_or_default;
use crate::audio::reopen::{DEVICE_BACKOFF, run_reopening};

pub(crate) const RENDER_SAMPLE_RATE: u32 = 48_000;
pub(crate) const RENDER_CHANNELS: usize = 2;
const EVENT_TIMEOUT_MS: u32 = 50;
const BUFFER_DURATION_HNS: i64 = 600_000;
const CUE_IDLE_TIMEOUT: Duration = Duration::from_secs(3);
const CUE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const CUE_STALE_GRACE: Duration = Duration::from_secs(2);

struct ActiveCue {
    samples: Vec<f32>,
    cursor: usize,
}

#[derive(Default)]
struct CueMixer {
    active: Vec<ActiveCue>,
}

impl CueMixer {
    fn push(&mut self, samples: Vec<f32>) {
        if !samples.is_empty() {
            self.active.push(ActiveCue { samples, cursor: 0 });
        }
    }

    fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    fn clear(&mut self) {
        self.active.clear();
    }

    fn fill(&mut self, out: &mut [f32]) {
        if self.active.is_empty() {
            out.fill(0.0);
            return;
        }
        for frame in out.chunks_mut(RENDER_CHANNELS) {
            let mut sum = 0.0f32;
            for cue in &mut self.active {
                if let Some(&sample) = cue.samples.get(cue.cursor) {
                    sum += sample;
                    cue.cursor += 1;
                }
            }
            frame.fill(sum.clamp(-1.0, 1.0));
        }
        self.active.retain(|cue| cue.cursor < cue.samples.len());
    }
}

struct CueRequest {
    device: Option<String>,
    samples: Vec<f32>,
}

static CUE_SENDER: Mutex<Option<Sender<CueRequest>>> = Mutex::new(None);

pub(crate) fn play_once(device: Option<String>, samples: Vec<f32>) {
    if samples.is_empty() {
        return;
    }
    let mut guard = CUE_SENDER.lock().unwrap_or_else(PoisonError::into_inner);
    let request = match guard.as_ref() {
        Some(tx) => match tx.send(CueRequest { device, samples }) {
            Ok(()) => return,
            Err(mpsc::SendError(request)) => request,
        },
        None => CueRequest { device, samples },
    };
    let (tx, rx) = mpsc::channel();
    if tx.send(request).is_err() {
        return;
    }
    match thread::Builder::new()
        .name("audio-cue".to_owned())
        .spawn(move || cue_worker(rx))
    {
        Ok(_) => *guard = Some(tx),
        Err(err) => {
            *guard = None;
            tracing::warn!(%err, "could not start the audio cue worker");
        }
    }
}

fn cue_worker(rx: mpsc::Receiver<CueRequest>) {
    let Ok(first) = rx.recv() else {
        return;
    };
    let mixer = Arc::new(Mutex::new(CueMixer::default()));
    let mut stream: Option<OutputStream> = None;
    let mut current_device: Option<String> = None;
    let mut idle_since: Option<Instant> = None;
    let mut stale_after = Instant::now();
    let mut pending = Some(first);

    loop {
        if let Some(req) = pending.take() {
            idle_since = None;
            if stream.is_some() && current_device != req.device {
                // Drop the previous stream first so only one render thread drains the mixer.
                stream = None;
            }
            let cue_duration = Duration::from_micros(
                (req.samples.len() as u64).saturating_mul(1_000_000)
                    / u64::from(RENDER_SAMPLE_RATE),
            );
            stale_after = stale_after.max(Instant::now() + cue_duration + CUE_STALE_GRACE);
            mixer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(req.samples);
            if stream.is_none() {
                current_device = req.device.clone();
                let shared = mixer.clone();
                stream = Some(OutputStream::start(req.device, move |out: &mut [f32]| {
                    shared
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .fill(out);
                }));
            }
            continue;
        }

        let is_empty = {
            let mut guard = mixer.lock().unwrap_or_else(PoisonError::into_inner);
            if !guard.is_empty() && Instant::now() >= stale_after {
                guard.clear();
            }
            guard.is_empty()
        };
        let wait = if is_empty {
            let since = *idle_since.get_or_insert_with(Instant::now);
            let remaining = CUE_IDLE_TIMEOUT.saturating_sub(since.elapsed());
            if remaining.is_zero() {
                stream = None;
                let mut guard = CUE_SENDER.lock().unwrap_or_else(PoisonError::into_inner);
                match rx.try_recv() {
                    Ok(req) => {
                        pending = Some(req);
                        continue;
                    }
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                        *guard = None;
                        return;
                    }
                }
            }
            remaining
        } else {
            idle_since = None;
            CUE_POLL_INTERVAL
        };

        match rx.recv_timeout(wait) {
            Ok(req) => pending = Some(req),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Render thread; dropping it stops the playback.
pub(crate) struct OutputStream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OutputStream {
    /// `fill` writes interleaved stereo samples in -1..1 for every block the device asks for.
    /// `device_id` of `None` is the Windows default output.
    pub(crate) fn start(
        device_id: Option<String>,
        mut fill: impl FnMut(&mut [f32]) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::Builder::new()
            .name("audio-render".to_owned())
            .spawn(move || {
                if initialize_mta().is_err() {
                    tracing::warn!("audio output: could not initialise COM");
                    return;
                }
                run_reopening("audio output", &flag, DEVICE_BACKOFF, || {
                    render(device_id.as_deref(), &flag, &mut fill)
                });
                deinitialize();
            })
            .map_err(|err| tracing::warn!(%err, "could not start the audio output thread"))
            .ok();
        Self { stop, thread }
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn render(
    device_id: Option<&str>,
    stop: &AtomicBool,
    fill: &mut impl FnMut(&mut [f32]),
) -> Result<(), wasapi::WasapiError> {
    let enumerator = DeviceEnumerator::new()?;
    let device = device_or_default(&enumerator, device_id, Direction::Render)?;
    tracing::info!(
        device = %device.get_friendlyname().unwrap_or_default(),
        "audio output started"
    );
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(
        32,
        32,
        SampleType::Float,
        RENDER_SAMPLE_RATE as usize,
        RENDER_CHANNELS,
        None,
    );
    client.initialize_client(
        &format,
        Direction::Render,
        StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: BUFFER_DURATION_HNS,
        },
        None,
    )?;
    let event = client.set_get_eventhandle()?;
    let writer = client.get_audiorenderclient()?;

    let mut samples: Vec<f32> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut write_available = || -> Result<(), wasapi::WasapiError> {
        let frames = client.get_available_space_in_frames()? as usize;
        if frames > 0 {
            samples.clear();
            samples.resize(frames * RENDER_CHANNELS, 0.0);
            fill(&mut samples);
            bytes.clear();
            for sample in &samples {
                bytes.extend_from_slice(&sample.clamp(-1.0, 1.0).to_le_bytes());
            }
            writer.write_to_device(&bytes, None)?;
        }
        Ok(())
    };
    // Fill the initial buffer before starting the stream so playback begins with audio.
    write_available()?;
    client.start_stream()?;

    while !stop.load(Ordering::SeqCst) {
        let _ = event.wait_for_event(EVENT_TIMEOUT_MS);
        if stop.load(Ordering::SeqCst) {
            break;
        }
        write_available()?;
    }
    client.stop_stream()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_duplicates_mono_to_stereo_and_pads_with_zeros() {
        let mut mixer = CueMixer::default();
        mixer.push(vec![0.25, -0.5, 0.75]);
        assert!(!mixer.is_empty());

        let mut first = [1.0f32; 4];
        mixer.fill(&mut first);
        assert_eq!(first, [0.25, 0.25, -0.5, -0.5]);
        assert!(!mixer.is_empty());

        let mut second = [1.0f32; 6];
        mixer.fill(&mut second);
        assert_eq!(second, [0.75, 0.75, 0.0, 0.0, 0.0, 0.0]);
        assert!(mixer.is_empty());
    }

    #[test]
    fn mixer_sums_overlapping_cues_clamps_and_drains_finished() {
        let mut mixer = CueMixer::default();
        mixer.push(vec![0.7, -0.8, 0.3, 0.1]);
        mixer.push(vec![0.6, -0.5]);

        let mut first = [0.0f32; 4];
        mixer.fill(&mut first);
        assert_eq!(first, [1.0, 1.0, -1.0, -1.0]);
        assert_eq!(mixer.active.len(), 1);

        let mut second = [1.0f32; 4];
        mixer.fill(&mut second);
        assert_eq!(second, [0.3, 0.3, 0.1, 0.1]);
        assert!(mixer.is_empty());
    }

    #[test]
    fn empty_mixer_fills_silence_and_ignores_empty_cues() {
        let mut mixer = CueMixer::default();
        mixer.push(Vec::new());
        assert!(mixer.is_empty());

        let mut out = [0.5f32; 4];
        mixer.fill(&mut out);
        assert_eq!(out, [0.0, 0.0, 0.0, 0.0]);
    }
}
