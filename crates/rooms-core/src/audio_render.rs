//! Saída de áudio própria por WASAPI: estéreo, 48 kHz, preenchida por um callback.
//! Só compila no Windows.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wasapi::{
    DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, deinitialize, initialize_mta,
};

pub(crate) const RENDER_SAMPLE_RATE: u32 = 48_000;
pub(crate) const RENDER_CHANNELS: usize = 2;
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const BUFFER_DURATION_HNS: i64 = 600_000;

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
                match render(device_id.as_deref(), &flag, &mut fill) {
                    Ok(()) => tracing::info!("audio output stopped"),
                    Err(err) => tracing::warn!(%err, "audio output failed"),
                }
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
    let device = match device_id {
        Some(id) => enumerator.get_device(id)?,
        None => enumerator.get_default_device(Direction::Render)?,
    };
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
        StreamMode::PollingShared {
            autoconvert: true,
            buffer_duration_hns: BUFFER_DURATION_HNS,
        },
        None,
    )?;
    let writer = client.get_audiorenderclient()?;
    client.start_stream()?;

    let mut samples: Vec<f32> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
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
        thread::sleep(POLL_INTERVAL);
    }
    client.stop_stream()
}
