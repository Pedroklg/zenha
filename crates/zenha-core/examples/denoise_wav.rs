//! Offline A/B denoiser for 16-bit PCM mono 48 kHz WAV files using RNNoise and the noise gate.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use zenha_core::{DENOISE_FRAME, Denoiser, NoiseGate, RnnoiseDenoiser};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;
const PCM_FORMAT: u16 = 1;
const SPEECH_THRESHOLD: f32 = 0.55;
const SILENCE_DB: f64 = -100.0;
const USAGE: &str = "usage: denoise_wav <input.wav> <output.wav> [--gate <db>]";

struct Args {
    input: PathBuf,
    output: PathBuf,
    gate_db: Option<f32>,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let mut positional = Vec::new();
    let mut gate_db = None;
    while let Some(arg) = args.next() {
        if arg == "--gate" {
            let Some(raw) = args.next() else {
                return Err("missing value for --gate".into());
            };
            let text = raw.to_str().ok_or("--gate value must be valid UTF-8")?;
            let db: f32 = text
                .parse()
                .map_err(|_| format!("invalid --gate value: {text}"))?;
            if !db.is_finite() {
                return Err("--gate value must be finite".into());
            }
            gate_db = Some(db);
        } else if arg == "-h" || arg == "--help" {
            return Err(USAGE.into());
        } else if arg.to_str().is_some_and(|s| s.starts_with('-')) {
            return Err(format!("unknown option: {}", arg.to_string_lossy()).into());
        } else {
            positional.push(PathBuf::from(arg));
        }
    }
    let mut positional = positional.into_iter();
    let (Some(input), Some(output), None) =
        (positional.next(), positional.next(), positional.next())
    else {
        return Err(USAGE.into());
    };
    Ok(Args {
        input,
        output,
        gate_db,
    })
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice: [u8; 2] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u16::from_le_bytes(slice))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice: [u8; 4] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(slice))
}

fn read_wav(path: &Path) -> Result<Vec<i16>, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("input is not a RIFF WAVE file".into());
    }

    let mut format_ok = false;
    let mut samples = None;
    let mut pos = 12usize;

    while pos < bytes.len() {
        let Some(id) = bytes.get(pos..pos.saturating_add(4)) else {
            return Err("truncated WAV chunk header".into());
        };
        let Some(size) = read_u32_le(&bytes, pos.saturating_add(4)) else {
            return Err("truncated WAV chunk size".into());
        };
        let data_start = pos.checked_add(8).ok_or("WAV chunk offset overflow")?;
        let chunk_len = usize::try_from(size).map_err(|_| "WAV chunk too large")?;
        let data_end = data_start
            .checked_add(chunk_len)
            .ok_or("WAV chunk length overflow")?;
        let Some(chunk) = bytes.get(data_start..data_end) else {
            return Err("truncated WAV chunk payload".into());
        };

        if id == b"fmt " {
            let audio_format = read_u16_le(chunk, 0).ok_or("truncated fmt chunk")?;
            let channels = read_u16_le(chunk, 2).ok_or("truncated fmt chunk")?;
            let sample_rate = read_u32_le(chunk, 4).ok_or("truncated fmt chunk")?;
            let bits_per_sample = read_u16_le(chunk, 14).ok_or("truncated fmt chunk")?;
            if audio_format != PCM_FORMAT
                || channels != CHANNELS
                || sample_rate != SAMPLE_RATE
                || bits_per_sample != BITS_PER_SAMPLE
            {
                return Err(format!(
                    "expected 16-bit PCM mono 48 kHz WAV, got format={audio_format} ch={channels} sr={sample_rate} bits={bits_per_sample}"
                )
                .into());
            }
            format_ok = true;
        } else if id == b"data" {
            if !chunk.len().is_multiple_of(2) {
                return Err("odd number of bytes in 16-bit PCM data chunk".into());
            }
            let mut pcm = Vec::with_capacity(chunk.len() / 2);
            for pair in chunk.chunks_exact(2) {
                if let [lo, hi] = *pair {
                    pcm.push(i16::from_le_bytes([lo, hi]));
                }
            }
            samples = Some(pcm);
        }

        pos = data_end.saturating_add(chunk_len % 2);
    }

    if !format_ok {
        return Err("WAV file is missing a fmt chunk".into());
    }
    samples.ok_or_else(|| "WAV file is missing a data chunk".into())
}

fn write_wav(path: &Path, samples: &[i16]) -> Result<(), Box<dyn Error>> {
    let data_bytes = u32::try_from(samples.len().saturating_mul(2))
        .map_err(|_| "output audio exceeds WAV 4 GB limit")?;
    let riff_size = data_bytes
        .checked_add(36)
        .ok_or("output audio exceeds WAV 4 GB limit")?;
    let byte_rate = SAMPLE_RATE * u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
    let block_align = CHANNELS * (BITS_PER_SAMPLE / 8);

    let mut out = Vec::with_capacity(44usize.saturating_add(data_bytes as usize));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&PCM_FORMAT.to_le_bytes());
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(path, out)?;
    Ok(())
}

fn rms_dbfs(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return SILENCE_DB;
    }
    let energy: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
    let rms = (energy / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * (rms / 32768.0).log10()).max(SILENCE_DB)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let mut samples = read_wav(&args.input)?;
    let input_db = rms_dbfs(&samples);

    let mut denoiser = RnnoiseDenoiser::new();
    let mut gate = NoiseGate::new();
    let mut total_frames = 0u64;
    let mut speech_frames = 0u64;
    let mut gate_open_frames = 0u64;

    let start = Instant::now();
    for frame in samples.chunks_exact_mut(DENOISE_FRAME) {
        let speech = denoiser.process(frame);
        if speech.is_some_and(|p| p >= SPEECH_THRESHOLD) {
            speech_frames += 1;
        }
        let reading = gate.process(frame, args.gate_db, speech);
        if reading.open {
            gate_open_frames += 1;
        }
        total_frames += 1;
    }
    let elapsed = start.elapsed();

    let output_db = rms_dbfs(&samples);
    write_wav(&args.output, &samples)?;

    let avg_ms = if total_frames > 0 {
        elapsed.as_secs_f64() * 1000.0 / total_frames as f64
    } else {
        0.0
    };
    let budget_pct = (avg_ms / 10.0) * 100.0;
    let speech_pct = if total_frames > 0 {
        speech_frames as f64 * 100.0 / total_frames as f64
    } else {
        0.0
    };
    let gate_open_pct = if total_frames > 0 {
        gate_open_frames as f64 * 100.0 / total_frames as f64
    } else {
        0.0
    };

    println!(
        "frames: {total_frames} ({:.2} s)",
        total_frames as f64 * 0.01
    );
    println!("avg time: {avg_ms:.3} ms/frame ({budget_pct:.1}% of 10 ms budget)");
    println!("input RMS: {input_db:.1} dBFS");
    println!("output RMS: {output_db:.1} dBFS");
    println!("speech >= {SPEECH_THRESHOLD:.2}: {speech_pct:.1}% ({speech_frames}/{total_frames})");
    match args.gate_db {
        Some(db) => {
            println!(
                "gate ({db:.1} dB) open: {gate_open_pct:.1}% ({gate_open_frames}/{total_frames})"
            )
        }
        None => {
            println!("gate (off) open: {gate_open_pct:.1}% ({gate_open_frames}/{total_frames})")
        }
    }
    Ok(())
}
