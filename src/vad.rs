//! Voice Activity Detection module using Silero VAD
//!
//! State machine:
//! - Idle: Not listening (VAD disabled or paused)
//! - Listening: VAD active, waiting for speech
//! - Speaking: Speech detected, accumulating audio
//! - Silence: Speech ended, waiting for silence timeout

use std::time::{Duration, Instant};
use voice_activity_detector::VoiceActivityDetector;

/// VAD sample rate - Silero v5 works best at 16kHz
pub const VAD_SAMPLE_RATE: u32 = 16000;

/// VAD chunk size for 16kHz (fixed by Silero v5)
pub const VAD_CHUNK_SIZE: usize = 512;

/// VAD state machine states
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VadState {
    /// Not listening (paused by hotkey or disabled)
    Idle,
    /// Listening for speech
    Listening,
    /// Speech detected, recording
    Speaking,
    /// Speech ended, waiting for silence timeout before processing
    SilenceDetected,
}

/// Events emitted by the VAD state machine
#[derive(Debug, Clone)]
pub enum VadEvent {
    /// Transitioned to a new state
    StateChanged(VadState),
    /// Have enough audio for wake word check (~1.2s)
    /// Main thread should check for wake word in this audio
    WakeWordCheckReady(Vec<f32>),
    /// Ready to process accumulated audio
    ReadyToProcess(Vec<f32>),
}

/// Voice Activity Detector wrapper with state machine
pub struct Vad {
    detector: VoiceActivityDetector,
    state: VadState,
    /// Accumulated audio buffer (16kHz f32 samples)
    audio_buffer: Vec<f32>,
    /// Rolling buffer of recent audio for catching speech start
    pre_buffer: Vec<f32>,
    /// Max size of pre_buffer in samples (speech_pad_ms worth)
    pre_buffer_max: usize,
    /// When silence was first detected
    silence_start: Option<Instant>,
    /// When speech first started
    speech_start: Option<Instant>,
    /// Configuration
    sensitivity: f32,
    silence_ms: u64,
    min_speech_ms: u64,
    speech_pad_ms: u64,
    /// Buffer for incomplete chunks
    chunk_buffer: Vec<f32>,
    /// Wake word mode enabled
    wake_word_enabled: bool,
    /// Number of samples needed for wake word check (~1.2s at 16kHz)
    wake_word_check_samples: usize,
    /// Whether wake word check has been emitted for current utterance
    wake_word_check_emitted: bool,
}

impl Vad {
    /// Create a new VAD instance
    ///
    /// # Arguments
    /// * `sensitivity` - Threshold 0.0-1.0, higher = more sensitive (lower threshold)
    /// * `silence_ms` - How long to wait after speech stops before processing
    /// * `min_speech_ms` - Minimum speech duration before it's considered valid
    /// * `speech_pad_ms` - Extra padding at end of speech to catch trailing words
    pub fn new(sensitivity: f32, silence_ms: u64, min_speech_ms: u64, speech_pad_ms: u64) -> Result<Self, voice_activity_detector::Error> {
        let detector = VoiceActivityDetector::builder()
            .sample_rate(VAD_SAMPLE_RATE)
            .chunk_size(VAD_CHUNK_SIZE)
            .build()?;

        // Pre-buffer size: use speech_pad_ms for start padding too
        let pre_buffer_max = (VAD_SAMPLE_RATE as u64 * speech_pad_ms / 1000) as usize;

        Ok(Self {
            detector,
            state: VadState::Idle,
            audio_buffer: Vec::with_capacity(VAD_SAMPLE_RATE as usize * 30), // 30s max
            pre_buffer: Vec::with_capacity(pre_buffer_max),
            pre_buffer_max,
            silence_start: None,
            speech_start: None,
            sensitivity,
            silence_ms,
            min_speech_ms,
            speech_pad_ms,
            chunk_buffer: Vec::with_capacity(VAD_CHUNK_SIZE),
            wake_word_enabled: false,
            wake_word_check_samples: (VAD_SAMPLE_RATE as f32 * 1.2) as usize, // 1.2 seconds
            wake_word_check_emitted: false,
        })
    }

    /// Get current state
    pub fn state(&self) -> VadState {
        self.state
    }

    /// Enable or disable wake word mode
    pub fn set_wake_word_enabled(&mut self, enabled: bool) {
        self.wake_word_enabled = enabled;
    }

    /// Start listening (transition from Idle)
    pub fn start_listening(&mut self) -> Option<VadEvent> {
        if self.state == VadState::Idle {
            self.state = VadState::Listening;
            self.audio_buffer.clear();
            self.chunk_buffer.clear();
            self.pre_buffer.clear();
            self.silence_start = None;
            self.speech_start = None;
            self.wake_word_check_emitted = false;
            Some(VadEvent::StateChanged(VadState::Listening))
        } else {
            None
        }
    }

    /// Stop listening (transition to Idle)
    pub fn stop_listening(&mut self) -> Option<VadEvent> {
        if self.state != VadState::Idle {
            self.state = VadState::Idle;
            self.audio_buffer.clear();
            self.chunk_buffer.clear();
            self.pre_buffer.clear();
            self.silence_start = None;
            self.speech_start = None;
            self.wake_word_check_emitted = false;
            Some(VadEvent::StateChanged(VadState::Idle))
        } else {
            None
        }
    }

    /// Feed audio samples (must be 16kHz f32 mono)
    /// Returns events that occurred
    pub fn feed(&mut self, samples: &[f32]) -> Vec<VadEvent> {
        let mut events = Vec::new();

        // Only process if we're actively listening
        if self.state == VadState::Idle {
            return events;
        }

        // Add samples to chunk buffer
        self.chunk_buffer.extend_from_slice(samples);

        // Process complete chunks
        while self.chunk_buffer.len() >= VAD_CHUNK_SIZE {
            let chunk: Vec<f32> = self.chunk_buffer.drain(..VAD_CHUNK_SIZE).collect();

            // Get speech probability
            let probability = self.detector.predict(chunk.iter().copied());

            // Convert sensitivity to threshold (higher sensitivity = lower threshold)
            // sensitivity 0.0 -> threshold 0.9 (very hard to trigger)
            // sensitivity 0.5 -> threshold 0.5
            // sensitivity 1.0 -> threshold 0.1 (very easy to trigger)
            let threshold = 1.0 - (self.sensitivity * 0.8);
            let is_speech = probability >= threshold;

            // State machine transitions
            match self.state {
                VadState::Idle => {
                    // Shouldn't happen since we check above, but just in case
                }
                VadState::Listening => {
                    // Always maintain rolling pre-buffer while listening
                    self.pre_buffer.extend_from_slice(&chunk);
                    if self.pre_buffer.len() > self.pre_buffer_max {
                        let excess = self.pre_buffer.len() - self.pre_buffer_max;
                        self.pre_buffer.drain(..excess);
                    }

                    if is_speech {
                        self.state = VadState::Speaking;
                        self.speech_start = Some(Instant::now());
                        self.audio_buffer.clear();
                        // Prepend pre-buffer to catch the start of speech
                        self.audio_buffer.extend_from_slice(&self.pre_buffer);
                        self.audio_buffer.extend_from_slice(&chunk);
                        self.pre_buffer.clear();
                        events.push(VadEvent::StateChanged(VadState::Speaking));
                    }
                }
                VadState::Speaking => {
                    // Always accumulate while speaking
                    self.audio_buffer.extend_from_slice(&chunk);

                    // Emit wake word check event when we have enough audio
                    if self.wake_word_enabled
                        && !self.wake_word_check_emitted
                        && self.audio_buffer.len() >= self.wake_word_check_samples
                    {
                        let check_audio = self.audio_buffer[..self.wake_word_check_samples].to_vec();
                        events.push(VadEvent::WakeWordCheckReady(check_audio));
                        self.wake_word_check_emitted = true;
                    }

                    if !is_speech {
                        // Silence detected, start counting
                        self.state = VadState::SilenceDetected;
                        self.silence_start = Some(Instant::now());
                        events.push(VadEvent::StateChanged(VadState::SilenceDetected));
                    }
                }
                VadState::SilenceDetected => {
                    // Still accumulate audio (might be brief pause)
                    self.audio_buffer.extend_from_slice(&chunk);

                    if is_speech {
                        // Speech resumed, back to speaking
                        self.state = VadState::Speaking;
                        self.silence_start = None;
                        events.push(VadEvent::StateChanged(VadState::Speaking));
                    } else if let Some(silence_start) = self.silence_start {
                        // Check if silence has lasted long enough (silence_ms + speech_pad_ms)
                        // The extra padding catches trailing words
                        let total_wait = self.silence_ms + self.speech_pad_ms;
                        if silence_start.elapsed() >= Duration::from_millis(total_wait) {
                            // Check if speech was long enough
                            let speech_duration = self.speech_start
                                .map(|s| s.elapsed())
                                .unwrap_or(Duration::ZERO);

                            if speech_duration >= Duration::from_millis(self.min_speech_ms) {
                                // Valid utterance! Ready to process
                                let audio = std::mem::take(&mut self.audio_buffer);
                                events.push(VadEvent::ReadyToProcess(audio));
                            }

                            // Back to listening for next utterance
                            self.state = VadState::Listening;
                            self.silence_start = None;
                            self.speech_start = None;
                            self.chunk_buffer.clear(); // Clear leftover samples
                            self.wake_word_check_emitted = false;
                            events.push(VadEvent::StateChanged(VadState::Listening));
                        }
                    }
                }
            }
        }

        events
    }

    /// Abort current utterance (e.g., wake word not found)
    /// Returns to Listening state without emitting ReadyToProcess
    pub fn abort_utterance(&mut self) -> Option<VadEvent> {
        if self.state == VadState::Speaking || self.state == VadState::SilenceDetected {
            self.audio_buffer.clear();
            self.chunk_buffer.clear();
            self.silence_start = None;
            self.speech_start = None;
            self.wake_word_check_emitted = false;
            self.state = VadState::Listening;
            Some(VadEvent::StateChanged(VadState::Listening))
        } else {
            None
        }
    }

    /// Reset the detector state (call after processing)
    pub fn reset(&mut self) {
        self.audio_buffer.clear();
        self.chunk_buffer.clear();
        self.pre_buffer.clear();
        self.silence_start = None;
        self.speech_start = None;
        self.wake_word_check_emitted = false;
        if self.state != VadState::Idle {
            self.state = VadState::Listening;
        }
    }
}
