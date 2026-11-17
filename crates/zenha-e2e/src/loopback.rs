//! WASAPI loopback probe of the test output device. Compiles on Windows only.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use wasapi::{
    Device, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, deinitialize,
    initialize_mta,
};

use crate::dsp::{ToneMeasurement, analyze_tone, downmix};
use crate::probe::{AudioProbe, RenderDevice};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const BUFFER_DURATION_HNS: i64 = 2_000_000;
const BYTES_PER_SAMPLE: usize = 4;

/// `target` is a case-insensitive part of the name of the device to test on.
pub struct WasapiProbe {
    target: Option<String>,
}

impl WasapiProbe {
    pub fn from_env() -> Self {
        let target = std::env::var("ZENHA_E2E_OUTPUT")
            .or_else(|_| std::env::var("ROOMS_E2E_OUTPUT"))
            .ok()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty());
        Self { target }
    }
}

fn test_device(enumerator: &DeviceEnumerator, target: Option<&str>) -> anyhow::Result<Device> {
    let Some(target) = target else {
        return enumerator
            .get_default_device(Direction::Render)
            .context("getting the default render device");
    };
    let collection = enumerator
        .get_device_collection(Direction::Render)
        .context("listing render devices")?;
    let count = collection
        .get_nbr_devices()
        .context("counting render devices")?;
    for index in 0..count {
        let device = collection
            .get_device_at_index(index)
            .context("reading a render device")?;
        let name = device.get_friendlyname().unwrap_or_default();
        if name.to_lowercase().contains(target) {
            return Ok(device);
        }
    }
    bail!("no active render device has `{target}` in its name (ZENHA_E2E_OUTPUT)")
}

struct Captured {
    mono: Vec<f32>,
    sample_rate: u32,
    elapsed: Duration,
}

/// COM is initialised on a fresh thread so no apartment state leaks into the caller.
fn on_com_thread<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    thread::spawn(move || {
        let init = initialize_mta();
        if init.is_err() {
            bail!("could not initialise COM: {init:?}");
        }
        let result = work();
        deinitialize();
        result
    })
    .join()
    .map_err(|_| anyhow!("the probe thread panicked"))?
}

fn capture_render(target: Option<String>, duration: Duration) -> anyhow::Result<Captured> {
    let enumerator = DeviceEnumerator::new().context("creating the device enumerator")?;
    let device = test_device(&enumerator, target.as_deref())?;
    let mut client = device
        .get_iaudioclient()
        .context("activating the audio client")?;
    let format = client.get_mixformat().context("reading the mix format")?;
    let is_float32 =
        format.get_bitspersample() == 32 && matches!(format.get_subformat(), Ok(SampleType::Float));
    if !is_float32 {
        bail!(
            "unsupported mix format: {} bits, {:?}",
            format.get_bitspersample(),
            format.get_subformat()
        );
    }
    let sample_rate = format.get_samplespersec();
    let channels = usize::from(format.get_nchannels());
    client
        .initialize_client(
            &format,
            Direction::Capture,
            StreamMode::PollingShared {
                autoconvert: true,
                buffer_duration_hns: BUFFER_DURATION_HNS,
            },
            None,
        )
        .context("initialising loopback capture")?;
    let capture = client
        .get_audiocaptureclient()
        .context("getting the capture client")?;
    client.start_stream().context("starting the stream")?;

    let started = Instant::now();
    let mut bytes: VecDeque<u8> = VecDeque::new();
    while started.elapsed() < duration {
        while let Some(frames) = capture
            .get_next_packet_size()
            .context("reading the packet size")?
        {
            if frames == 0 {
                break;
            }
            capture
                .read_from_device_to_deque(&mut bytes)
                .context("reading a packet")?;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let elapsed = started.elapsed();
    client.stop_stream().context("stopping the stream")?;

    let raw: Vec<u8> = bytes.into_iter().collect();
    let interleaved: Vec<f32> = raw
        .chunks_exact(BYTES_PER_SAMPLE)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    Ok(Captured {
        mono: downmix(&interleaved, channels),
        sample_rate,
        elapsed,
    })
}

/// Body of the `side-tone` subcommand: plays until stdin closes, so the tone dies with the
/// harness. It runs as its own process so a bot can share the sound of exactly this process.
pub fn side_tone_main(hz: f64, amplitude: f64) -> anyhow::Result<()> {
    let target = std::env::var("ZENHA_E2E_OUTPUT")
        .or_else(|_| std::env::var("ROOMS_E2E_OUTPUT"))
        .ok()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty());
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut sink);
        flag.store(true, Ordering::SeqCst);
    });
    on_com_thread(move || play_tone(target.as_deref(), hz, amplitude, &stop))
}

fn play_tone(
    target: Option<&str>,
    hz: f64,
    amplitude: f64,
    stop: &AtomicBool,
) -> anyhow::Result<()> {
    const RATE: usize = 48_000;
    let enumerator = DeviceEnumerator::new().context("creating the device enumerator")?;
    let test_id = test_device(&enumerator, target)?.get_id()?;
    let default_id = enumerator
        .get_default_device(Direction::Render)
        .and_then(|device| device.get_id())
        .context("getting the default render device")?;
    let collection = enumerator
        .get_device_collection(Direction::Render)
        .context("listing render devices")?;
    let mut side = None;
    for index in 0..collection.get_nbr_devices()? {
        let device = collection.get_device_at_index(index)?;
        let id = device.get_id()?;
        if id != test_id && id != default_id {
            side = Some(device);
            break;
        }
    }
    let Some(device) = side else {
        bail!("no third output device to play the side tone on");
    };
    let name = device.get_friendlyname().unwrap_or_default();
    let mut client = device
        .get_iaudioclient()
        .context("activating the audio client")?;
    let format = WaveFormat::new(32, 32, SampleType::Float, RATE, 2, None);
    client
        .initialize_client(
            &format,
            Direction::Render,
            StreamMode::PollingShared {
                autoconvert: true,
                buffer_duration_hns: 1_000_000,
            },
            None,
        )
        .context("initialising the side tone")?;
    let writer = client
        .get_audiorenderclient()
        .context("getting the render client")?;
    client.start_stream().context("starting the side tone")?;
    println!("ready {name}");

    let step = std::f64::consts::TAU * hz / RATE as f64;
    let mut phase = 0.0f64;
    let mut bytes: Vec<u8> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let frames = client.get_available_space_in_frames()?;
        bytes.clear();
        for _ in 0..frames {
            let sample = ((amplitude * phase.sin()) as f32).to_le_bytes();
            bytes.extend_from_slice(&sample);
            bytes.extend_from_slice(&sample);
            phase = (phase + step) % std::f64::consts::TAU;
        }
        if !bytes.is_empty() {
            writer.write_to_device(&bytes, None)?;
        }
        thread::sleep(POLL_INTERVAL);
    }
    client.stop_stream()?;
    Ok(())
}

fn list_render_devices(target: Option<String>) -> anyhow::Result<Vec<RenderDevice>> {
    let enumerator = DeviceEnumerator::new().context("creating the device enumerator")?;
    let default_id = test_device(&enumerator, target.as_deref())?
        .get_id()
        .context("reading the test device id")?;
    let collection = enumerator
        .get_device_collection(Direction::Render)
        .context("listing render devices")?;
    let count = collection
        .get_nbr_devices()
        .context("counting render devices")?;
    let mut devices = Vec::new();
    for index in 0..count {
        let device = collection
            .get_device_at_index(index)
            .context("reading a render device")?;
        let id = device.get_id().context("reading a device id")?;
        let name = device
            .get_friendlyname()
            .unwrap_or_else(|err| format!("<unnamed: {err}>"));
        devices.push(RenderDevice {
            is_default: id == default_id,
            id,
            name,
        });
    }
    Ok(devices)
}

/// Keeps the latest capture that had sound but not the tone, for offline inspection.
fn dump_capture(captured: &Captured) {
    let path = std::path::Path::new("dev-logs")
        .join("e2e")
        .join("capture-without-tone.wav");
    let data_len = (captured.mono.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + captured.mono.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&captured.sample_rate.to_le_bytes());
    out.extend_from_slice(&(captured.sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in &captured.mono {
        let value = (f64::from(*sample).clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    if let Err(err) = std::fs::write(&path, out) {
        tracing::warn!(%err, "could not save the capture");
    }
}

impl AudioProbe for WasapiProbe {
    fn measure_tone(&self, hz: f64, duration: Duration) -> anyhow::Result<ToneMeasurement> {
        let target = self.target.clone();
        let captured = on_com_thread(move || capture_render(target, duration))?;
        let mut measurement = analyze_tone(&captured.mono, captured.sample_rate, hz);
        if measurement.total_rms > 0.005 && measurement.tone_ratio < 0.5 {
            dump_capture(&captured);
        }
        let expected = f64::from(captured.sample_rate) * captured.elapsed.as_secs_f64();
        measurement.coverage = if expected > 0.0 {
            (captured.mono.len() as f64 / expected).min(1.0)
        } else {
            0.0
        };
        Ok(measurement)
    }

    fn render_devices(&self) -> anyhow::Result<Vec<RenderDevice>> {
        let target = self.target.clone();
        on_com_thread(move || list_render_devices(target))
    }

    fn pinned(&self) -> bool {
        self.target.is_some()
    }
}
