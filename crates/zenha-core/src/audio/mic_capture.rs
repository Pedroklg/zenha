//! Captura do microfone por WASAPI em quadros de 10 ms, mono, 48 kHz. Só compila no Windows.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wasapi::{
    DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, deinitialize, initialize_mta,
};

use crate::audio::noise::DENOISE_FRAME;
use crate::backend::support::drain_f32_le_to_i16;

pub(crate) const CAPTURE_SAMPLE_RATE: u32 = 48_000;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const BUFFER_DURATION_HNS: i64 = 1_000_000;
const BYTES_PER_SAMPLE: usize = 4;
const FRAME_BYTES: usize = DENOISE_FRAME * BYTES_PER_SAMPLE;
// Past this backlog the oldest audio is dropped so a stall never turns into lasting delay.
const MAX_BACKLOG_BYTES: usize = FRAME_BYTES * 20;

/// Capture thread; dropping it stops the capture.
pub(crate) struct MicCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MicCapture {
    /// `build` runs on the capture thread and returns the sink of each frame, so the sink
    /// may own values that cannot change threads. `device_id` of `None` is the Windows default.
    pub(crate) fn start<F>(
        device_id: Option<String>,
        build: impl FnOnce() -> F + Send + 'static,
    ) -> Self
    where
        F: FnMut(&mut [i16]),
    {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::Builder::new()
            .name("mic-capture".to_owned())
            .spawn(move || {
                if initialize_mta().is_err() {
                    tracing::warn!("microphone capture: could not initialise COM");
                    return;
                }
                let mut sink = build();
                match capture(device_id.as_deref(), &flag, &mut sink) {
                    Ok(()) => tracing::info!("microphone capture stopped"),
                    Err(err) => tracing::warn!(%err, "microphone capture failed"),
                }
                drop(sink);
                deinitialize();
            })
            .map_err(|err| tracing::warn!(%err, "could not start the microphone capture thread"))
            .ok();
        Self { stop, thread }
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture(
    device_id: Option<&str>,
    stop: &AtomicBool,
    sink: &mut impl FnMut(&mut [i16]),
) -> Result<(), wasapi::WasapiError> {
    let enumerator = DeviceEnumerator::new()?;
    let device = match device_id {
        Some(id) => enumerator.get_device(id)?,
        None => enumerator.get_default_device(Direction::Capture)?,
    };
    tracing::info!(
        device = %device.get_friendlyname().unwrap_or_default(),
        "microphone capture started"
    );
    let mut client = device.get_iaudioclient()?;
    // Windows converts rate and channels for us in shared mode.
    let format = WaveFormat::new(
        32,
        32,
        SampleType::Float,
        CAPTURE_SAMPLE_RATE as usize,
        1,
        None,
    );
    client.initialize_client(
        &format,
        Direction::Capture,
        StreamMode::PollingShared {
            autoconvert: true,
            buffer_duration_hns: BUFFER_DURATION_HNS,
        },
        None,
    )?;
    let reader = client.get_audiocaptureclient()?;
    client.start_stream()?;

    let mut bytes: VecDeque<u8> = VecDeque::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut frame = [0i16; DENOISE_FRAME];
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
            sink(&mut frame);
        }
        thread::sleep(POLL_INTERVAL);
    }
    client.stop_stream()
}
