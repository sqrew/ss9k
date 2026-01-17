//! Audio capture, processing, and transcription for SS9K
//!
//! This module handles:
//! - Microphone device detection (platform-specific)
//! - Audio stream building
//! - Sample rate conversion (resampling to 16kHz for Whisper)
//! - Whisper transcription

use anyhow::Result;
use cpal::Sample;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext};

use crate::Config;

pub const WHISPER_SAMPLE_RATE: u32 = 16000;

pub type AudioBuffer = Arc<Mutex<Vec<f32>>>;

/// Global callback counter (shared with main for recording state)
pub static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Check if a device name looks like a microphone (Linux)
#[cfg(target_os = "linux")]
pub fn is_microphone(name: &str) -> bool {
    name.contains("Microphone") && name.contains("CARD")
}

/// Check if a device name looks like a microphone (Windows)
#[cfg(target_os = "windows")]
pub fn is_microphone(name: &str) -> bool {
    name.to_lowercase().contains("microphone")
}

/// Check if a device name looks like a microphone (macOS)
#[cfg(target_os = "macos")]
pub fn is_microphone(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("microphone") || lower.contains("input") || lower.contains("mic")
}

/// Check if a device name looks like a microphone (other platforms)
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn is_microphone(_name: &str) -> bool {
    true
}

/// Build an audio input stream with the given sample type
pub fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: AudioBuffer,
    channels: usize,
    recording: Arc<std::sync::atomic::AtomicBool>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);

            if recording.load(Ordering::SeqCst) {
                if let Ok(mut buf) = buffer.lock() {
                    for chunk in data.chunks(channels) {
                        let sum: f32 = chunk.iter().map(|&s| <f32 as Sample>::from_sample(s)).sum();
                        buf.push(sum / channels as f32);
                    }
                }
            }
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

/// Resample audio from one sample rate to another
pub fn resample_audio(input: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
    if from_rate == to_rate {
        return Ok(input.to_vec());
    }

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let ratio = to_rate as f64 / from_rate as f64;
    let mut resampler = SincFixedIn::<f32>::new(
        ratio,
        2.0,
        params,
        input.len(),
        1,
    )?;

    let waves_in = vec![input.to_vec()];
    let waves_out = resampler.process(&waves_in, None)?;

    Ok(waves_out.into_iter().next().unwrap_or_default())
}

/// Transcribe audio using Whisper
pub fn transcribe(ctx: &WhisperContext, audio: &[f32], config: &Config) -> Result<String> {
    let mut state = ctx.create_state()?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(config.threads as i32);
    params.set_language(Some(&config.language));
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state.full(params, audio)?;

    let num_segments = state.full_n_segments()?;
    let mut result = String::new();

    for i in 0..num_segments {
        if let Ok(segment) = state.full_get_segment_text(i) {
            result.push_str(&segment);
        }
    }

    Ok(result.trim().to_string())
}
