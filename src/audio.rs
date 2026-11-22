//! Microphone recording, resampling, the "robot voice" effect, and playback.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SizedSample};
use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStreamHandle, Sink, Source};

// ---------------------------------------------------------------- recording

/// Everything the microphone callback shares with the rest of the app.
#[derive(Clone)]
struct Shared {
    buf: Arc<Mutex<Vec<f32>>>,   // audio of the current question (only filled while capturing)
    capturing: Arc<AtomicBool>,  // true between begin() and end()
    noise_bits: Arc<AtomicU32>,  // running estimate of the room noise level (f32 bits)
    broken: Arc<AtomicBool>,     // set if the microphone stream reports an error
}

/// A microphone that stays open for the whole life of the app.
/// While idle it only tracks the background noise level (no audio is stored).
/// `begin()` .. `end()` collect the audio of one question, with no start-up delay.
pub struct Mic {
    _stream: cpal::Stream,
    shared: Shared,
    rate: u32,
}

impl Mic {
    pub fn open() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("No microphone found"))?;
        let supported = device
            .default_input_config()
            .context("Could not read microphone settings")?;
        let rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let shared = Shared {
            buf: Arc::new(Mutex::new(Vec::new())),
            capturing: Arc::new(AtomicBool::new(false)),
            noise_bits: Arc::new(AtomicU32::new(0.005f32.to_bits())),
            broken: Arc::new(AtomicBool::new(false)),
        };
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, shared.clone(), channels)?,
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, shared.clone(), channels)?,
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, shared.clone(), channels)?,
            other => return Err(anyhow!("Unsupported microphone format: {other:?}")),
        };
        stream.play().context("Could not start the microphone")?;
        Ok(Self { _stream: stream, shared, rate })
    }

    /// Start collecting audio for one question (the microphone is already running).
    pub fn begin(&self) {
        self.shared.buf.lock().unwrap().clear();
        self.shared.capturing.store(true, Ordering::Relaxed);
    }

    /// Stop collecting and return mono samples plus their sample rate.
    pub fn end(&self) -> (Vec<f32>, u32) {
        self.shared.capturing.store(false, Ordering::Relaxed);
        let samples = std::mem::take(&mut *self.shared.buf.lock().unwrap());
        (samples, self.rate)
    }

    /// Loudness (RMS) of the last `seconds` of the current recording.
    pub fn recent_rms(&self, seconds: f32) -> f32 {
        let b = self.shared.buf.lock().unwrap();
        let n = ((self.rate as f32 * seconds) as usize).min(b.len());
        if n == 0 {
            return 0.0;
        }
        let tail = &b[b.len() - n..];
        (tail.iter().map(|x| x * x).sum::<f32>() / n as f32).sqrt()
    }

    /// Background noise level measured while idle.
    pub fn noise_floor(&self) -> f32 {
        f32::from_bits(self.shared.noise_bits.load(Ordering::Relaxed))
    }

    /// True if the microphone stream has failed (for example it was unplugged).
    pub fn is_broken(&self) -> bool {
        self.shared.broken.load(Ordering::Relaxed)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Shared,
    channels: usize,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let broken = shared.broken.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let capturing = shared.capturing.load(Ordering::Relaxed);
            let mut guard = if capturing { Some(shared.buf.lock().unwrap()) } else { None };
            let mut sum_sq = 0.0f32;
            let mut n = 0usize;
            for frame in data.chunks(channels) {
                let s: f32 = frame.iter().map(|&x| f32::from_sample(x)).sum::<f32>() / channels as f32;
                sum_sq += s * s;
                n += 1;
                if let Some(b) = guard.as_mut() {
                    b.push(s); // mono
                }
            }
            drop(guard);

            // While idle, keep a slow running estimate of the room noise (ignoring loud sounds)
            if !capturing && n > 0 {
                let rms = (sum_sq / n as f32).sqrt();
                let noise = f32::from_bits(shared.noise_bits.load(Ordering::Relaxed));
                if rms < (noise * 2.0).max(0.05) {
                    let updated = noise * 0.97 + rms * 0.03;
                    shared.noise_bits.store(updated.to_bits(), Ordering::Relaxed);
                }
            }
        },
        move |e| {
            eprintln!("microphone error: {e}");
            broken.store(true, Ordering::Relaxed);
        },
        None,
    )?;
    Ok(stream)
}

/// Simple resampler (averaging) – good enough for speech going into Whisper (16 kHz).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let n = (input.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let start = pos as usize;
            let end = ((pos + ratio).ceil() as usize).min(input.len()).max(start + 1);
            input[start..end].iter().sum::<f32>() / (end - start) as f32
        })
        .collect()
}

// ---------------------------------------------------------------- playback

/// Decodes MP3 bytes to f32 samples. Returns (samples, channels, sample_rate).
pub fn decode(bytes: Vec<u8>) -> Result<(Vec<f32>, u16, u32)> {
    let decoder = Decoder::new(Cursor::new(bytes)).context("Could not decode the voice audio")?;
    let channels = decoder.channels();
    let rate = decoder.sample_rate();
    let samples: Vec<f32> = decoder.convert_samples::<f32>().collect();
    Ok((samples, channels, rate))
}

/// Same idea as playRobotVoice() in the web page: 40 Hz ring modulation mixed under the
/// clean voice, high-pass 120 Hz, low-pass 7 kHz, then a soft limiter.
pub fn robot_effect(samples: &mut [f32], channels: u16, rate: u32, amount: f32) {
    let ch = channels.max(1) as usize;
    let rate = rate as f32;
    let tau = std::f32::consts::TAU;
    let a_hp = 1.0 - (-tau * 120.0 / rate).exp();
    let a_lp = 1.0 - (-tau * 7000.0 / rate).exp();
    let dry = 1.0 - amount * 0.5;

    let mut hp_state = vec![0.0f32; ch];
    let mut lp_state = vec![0.0f32; ch];

    for (i, frame) in samples.chunks_mut(ch).enumerate() {
        let osc = (tau * 40.0 * i as f32 / rate).sin();
        for (c, s) in frame.iter_mut().enumerate() {
            let x = *s;
            let mixed = (x * dry + x * osc * amount) * 0.9;
            hp_state[c] += a_hp * (mixed - hp_state[c]);
            let hp = mixed - hp_state[c];
            lp_state[c] += a_lp * (hp - lp_state[c]);
            *s = soft_limit(lp_state[c]);
        }
    }
}

fn soft_limit(x: f32) -> f32 {
    let threshold = 0.8;
    let a = x.abs();
    let y = if a > threshold { threshold + (a - threshold) / 20.0 } else { a };
    y.min(1.0).copysign(x)
}

/// Plays the samples and blocks until they finish.
pub fn play(handle: &OutputStreamHandle, samples: Vec<f32>, channels: u16, rate: u32) -> Result<()> {
    let sink = Sink::try_new(handle).context("Could not open the audio output")?;
    sink.append(SamplesBuffer::new(channels, rate, samples));
    sink.sleep_until_end();
    Ok(())
}
