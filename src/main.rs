mod audio;
mod commands;
mod lookups;
mod model;
mod vad;

use anyhow::Result;
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use enigo::{Enigo, Settings};
use notify::{recommended_watcher, RecursiveMode, Watcher};
use rdev::{listen, Event, EventType, Key as RdevKey};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use whisper_rs::{WhisperContext, WhisperContextParameters};

use audio::{build_stream, build_stream_with_vad, is_microphone, resample_audio, transcribe, AudioBuffer, CALLBACK_COUNT, WHISPER_SAMPLE_RATE};
use commands::{execute_command, print_help, set_key_repeat_ms};
use model::{download_model, get_model_install_path, get_model_path};
use vad::{Vad, VadEvent, VadState, VAD_SAMPLE_RATE};

// Recording state
static RECORDING: AtomicBool = AtomicBool::new(false);
static RECORDING_SESSION: AtomicU64 = AtomicU64::new(0);
static COMMAND_MODE: AtomicBool = AtomicBool::new(false); // True if recording was started with command_hotkey

// VAD state
static VAD_LISTENING: AtomicBool = AtomicBool::new(false); // True when VAD is actively listening

/// Audio message for the processor thread
enum AudioMessage {
    /// Audio from hotkey mode - needs resampling from native rate
    NeedsResampling(Vec<f32>),
    /// Audio from VAD mode - already at 16kHz
    AlreadyResampled(Vec<f32>),
    /// Wake word check - quick transcribe first ~1.2s and check for wake word
    WakeWordCheck(Vec<f32>),
}

/// System beep for audio feedback (single beep)
fn beep() {
    print!("\x07");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Double beep for completion feedback
fn beep_done() {
    beep();
    std::thread::sleep(Duration::from_millis(100));
    beep();
}

/// Get current timestamp string
fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Log a transcription to the dictation log file
fn log_dictation(path: &str, text: &str) {
    if path.is_empty() { return; }
    let expanded = shellexpand::tilde(path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(expanded.as_ref()) {
        let _ = writeln!(file, "[{}] {}", timestamp(), text);
    }
}

/// Log an error to both stderr and the error log file
fn log_error(path: &str, message: &str) {
    eprintln!("[SS9K] ❌ {}", message);
    if path.is_empty() { return; }
    let expanded = shellexpand::tilde(path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(expanded.as_ref()) {
        let _ = writeln!(file, "[{}] [ERROR] {}", timestamp(), message);
    }
}

/// Log a warning to both stderr and the error log file
fn log_warn(path: &str, message: &str) {
    eprintln!("[SS9K] ⚠️ {}", message);
    if path.is_empty() { return; }
    let expanded = shellexpand::tilde(path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(expanded.as_ref()) {
        let _ = writeln!(file, "[{}] [WARN] {}", timestamp(), message);
    }
}

/// Configuration for SS9K
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub language: String,
    pub threads: usize,
    pub device: String,
    pub hotkey: String,
    pub command_hotkey: String, // Alternate hotkey that auto-prefixes with leader word
    pub hotkey_mode: String,
    pub toggle_timeout_secs: u64,
    pub leader: String,
    pub key_repeat_ms: u64,
    pub processing_timeout_secs: u64, // 0 = no timeout
    #[serde(default)]
    pub audio_feedback: bool, // Beep on start/stop listening
    // VAD settings
    pub activation_mode: String,   // "hotkey" (default) or "vad"
    pub vad_sensitivity: f32,      // 0.0-1.0, higher = more sensitive
    pub vad_silence_ms: u64,       // Silence duration before processing
    pub vad_min_speech_ms: u64,    // Minimum speech before valid
    pub vad_speech_pad_ms: u64,    // Padding added to end of speech
    pub wake_word: String,         // Wake word for VAD mode (empty = disabled)
    // Logging
    pub dictation_log: String,     // Path to log transcriptions (empty = disabled)
    pub error_log: String,         // Path to log errors (empty = disabled)
    #[serde(default)]
    pub commands: HashMap<String, String>,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
    #[serde(default)]
    pub inserts: HashMap<String, String>,
    #[serde(default)]
    pub wrappers: HashMap<String, String>,
    #[serde(default)]
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: "small".to_string(),
            language: "en".to_string(),
            threads: 4,
            device: String::new(),
            hotkey: "F12".to_string(),
            command_hotkey: String::new(), // Empty = disabled
            hotkey_mode: "hold".to_string(),
            toggle_timeout_secs: 0,
            leader: "command".to_string(),
            key_repeat_ms: 50,
            processing_timeout_secs: 30, // Default 30s timeout
            audio_feedback: false,       // Disabled by default
            // VAD defaults
            activation_mode: "hotkey".to_string(), // Default to hotkey mode
            vad_sensitivity: 0.9,                  // High sensitivity for reliable detection
            vad_silence_ms: 1000,                  // 1 second - tolerates natural pauses
            vad_min_speech_ms: 200,                // Filter brief noises
            vad_speech_pad_ms: 300,                // Pad end of speech to catch trailing words
            wake_word: String::new(),              // Empty = no wake word required
            // Logging defaults
            dictation_log: String::new(),          // Empty = disabled
            error_log: String::new(),              // Empty = disabled
            commands: HashMap::new(),
            aliases: HashMap::new(),
            inserts: HashMap::new(),
            wrappers: HashMap::new(),
            verbose: true,
        }
    }
}

/// Parse a hotkey string into an rdev::Key
fn parse_hotkey(s: &str) -> Option<RdevKey> {
    match s.to_uppercase().as_str() {
        "F1" => Some(RdevKey::F1),
        "F2" => Some(RdevKey::F2),
        "F3" => Some(RdevKey::F3),
        "F4" => Some(RdevKey::F4),
        "F5" => Some(RdevKey::F5),
        "F6" => Some(RdevKey::F6),
        "F7" => Some(RdevKey::F7),
        "F8" => Some(RdevKey::F8),
        "F9" => Some(RdevKey::F9),
        "F10" => Some(RdevKey::F10),
        "F11" => Some(RdevKey::F11),
        "F12" => Some(RdevKey::F12),
        "SCROLLLOCK" | "SCROLL_LOCK" | "SCROLL" => Some(RdevKey::ScrollLock),
        "PAUSE" | "BREAK" => Some(RdevKey::Pause),
        "PRINTSCREEN" | "PRINT_SCREEN" | "PRTSC" => Some(RdevKey::PrintScreen),
        "INSERT" | "INS" => Some(RdevKey::Insert),
        "HOME" => Some(RdevKey::Home),
        "END" => Some(RdevKey::End),
        "PAGEUP" | "PAGE_UP" | "PGUP" => Some(RdevKey::PageUp),
        "PAGEDOWN" | "PAGE_DOWN" | "PGDN" => Some(RdevKey::PageDown),
        "NUM0" | "NUMPAD0" => Some(RdevKey::Kp0),
        "NUM1" | "NUMPAD1" => Some(RdevKey::Kp1),
        "NUM2" | "NUMPAD2" => Some(RdevKey::Kp2),
        "NUM3" | "NUMPAD3" => Some(RdevKey::Kp3),
        "NUM4" | "NUMPAD4" => Some(RdevKey::Kp4),
        "NUM5" | "NUMPAD5" => Some(RdevKey::Kp5),
        "NUM6" | "NUMPAD6" => Some(RdevKey::Kp6),
        "NUM7" | "NUMPAD7" => Some(RdevKey::Kp7),
        "NUM8" | "NUMPAD8" => Some(RdevKey::Kp8),
        "NUM9" | "NUMPAD9" => Some(RdevKey::Kp9),
        _ => None,
    }
}

impl Config {
    pub fn load() -> (Self, Option<PathBuf>) {
        let config_paths = [
            dirs::config_dir().map(|p| p.join("ss9k").join("config.toml")),
            dirs::home_dir().map(|p| p.join(".ss9k").join("config.toml")),
            Some(PathBuf::from("config.toml")),
        ];

        for path in config_paths.into_iter().flatten() {
            if path.exists() {
                if let Ok(contents) = fs::read_to_string(&path) {
                    match toml::from_str(&contents) {
                        Ok(config) => {
                            println!("[SS9K] Loaded config from: {:?}", path);
                            return (config, Some(path));
                        }
                        Err(e) => {
                            eprintln!("[SS9K] Config parse error in {:?}: {}", path, e);
                        }
                    }
                }
            }
        }

        // No config found - create one at the default location
        if let Some(config_dir) = dirs::config_dir() {
            let ss9k_dir = config_dir.join("ss9k");
            let config_path = ss9k_dir.join("config.toml");

            // Create directory if needed
            if let Err(e) = fs::create_dir_all(&ss9k_dir) {
                eprintln!("[SS9K] Failed to create config directory: {}", e);
            } else {
                // Write default config
                if let Err(e) = fs::write(&config_path, Self::default_config_content()) {
                    eprintln!("[SS9K] Failed to write default config: {}", e);
                } else {
                    println!("[SS9K] Created default config at: {:?}", config_path);
                    println!("[SS9K] Edit this file to customize your settings!");
                    return (Self::default(), Some(config_path));
                }
            }
        }

        println!("[SS9K] Using default config");
        (Self::default(), None)
    }

    fn default_config_content() -> &'static str {
        r##"# SuperScreecher9000 Configuration
# Edit this file to customize your settings.
# Changes are hot-reloaded - no restart needed!

# Model to use: tiny, base, small, medium, large
# Larger = more accurate but slower
# Tip: Use "tiny" or "base" on older/weaker CPUs
model = "small"

# Language for transcription (ISO 639-1 codes)
# Say "command languages" or "command language list" for full list
# Or see: https://github.com/openai/whisper#available-models-and-languages
language = "en"

# Number of threads for whisper inference
# More threads = faster on multi-core CPUs
threads = 4

# Specific audio device name (partial match)
# Leave empty for auto-detection
# Example: "Microphone" or "Blue Yeti"
device = ""

# Hotkey to trigger recording (dictation mode)
# Options: F1-F12, ScrollLock, Pause, PrintScreen, Insert, Home, End, PageUp, PageDown, Num0-Num9
hotkey = "F12"

# Command hotkey - alternate key that auto-prefixes with leader word
# Use this to speak commands without saying "command" first
# Example: with command_hotkey = "F11", pressing F11 and saying "enter"
# is the same as pressing F12 and saying "command enter"
# Leave empty to disable
command_hotkey = ""

# Hotkey mode: "hold" (release to stop) or "toggle" (press again to stop)
# Applies to both hotkey and command_hotkey
hotkey_mode = "hold"

# Auto-stop timeout for toggle mode (0 = no timeout)
toggle_timeout_secs = 0

# Leader word for voice commands
# All commands require this prefix: "command enter", "command emoji smile", etc.
# Change to whatever feels natural: "voice", "computer", "hey", etc.
leader = "command"

# Key repeat rate for hold mode (milliseconds between key presses)
# Lower = faster repeat, higher = slower
# Used when you say "command hold w" to spam a key
key_repeat_ms = 50

# Processing timeout in seconds (0 = no timeout)
# If transcription takes longer than this, it will be aborted
# Useful for weak CPUs that might hang on larger models
# Tip: If you hit timeouts often, try model = "tiny" or "base"
processing_timeout_secs = 30

# Verbose logging (processing, resampling, transcription details)
# Errors always print regardless. Set false once you're comfortable with the tool.
verbose = true

# Audio feedback (system beep)
# Single beep when recording starts, double beep when transcription completes
audio_feedback = false

# Activation mode: "hotkey" (default) or "vad" (voice activity detection)
# - hotkey: Press a key to start/stop recording (traditional mode)
# - vad: Automatically detect when you're speaking (hands-free mode)
#        In VAD mode, the hotkey toggles listening on/off
activation_mode = "hotkey"

# VAD settings (only used when activation_mode = "vad")
# Sensitivity: 0.0-1.0, higher = more sensitive to speech
vad_sensitivity = 0.9
# Silence duration (ms) before processing - wait for pause after speech
vad_silence_ms = 1000
# Minimum speech duration (ms) before it counts - ignore brief noises
vad_min_speech_ms = 200
# Speech padding (ms) - extra time at end to catch trailing words
vad_speech_pad_ms = 300

# Custom voice commands
# Maps spoken phrase -> shell command
# Supports $ENV_VAR expansion (e.g., $TERMINAL, $BROWSER, $EDITOR)
[commands]
# "open terminal" = "$TERMINAL"
# "open browser" = "$BROWSER"
# "open firefox" = "firefox"
# "screenshot" = "flameshot gui"

# Aliases for common misrecognitions
# Maps what whisper hears -> what you meant
[aliases]
# "e max" = "emacs"
# "fire fox" = "firefox"

# Text snippets for quick insertion
# Say "command insert <name>" to type the snippet
# Supports placeholders: {date}, {time}, {datetime}, {shell:cmd}
[inserts]
# email = "you@example.com"
# sig = "Best regards,\nYour Name"

# Text wrappers for quick wrapping
# Say "command wrap <name> <text>" to wrap text
# Use | to separate left/right: "parens" = "(|)"
[wrappers]
# quotes = '"'
# parens = "(|)"
# brackets = "[|]"
"##
    }

    pub fn load_from(path: &PathBuf) -> Option<Self> {
        if let Ok(contents) = fs::read_to_string(path) {
            match toml::from_str(&contents) {
                Ok(config) => Some(config),
                Err(e) => {
                    eprintln!("[SS9K] Config reload error: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }

    pub fn model_filename(&self) -> String {
        format!("ggml-{}.bin", self.model)
    }

    pub fn model_url(&self) -> String {
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin",
            self.model
        )
    }
}

fn main() -> Result<()> {
    let (config, config_path) = Config::load();
    println!("[SS9K] Model: {}, Language: {}, Threads: {}",
             config.model, config.language, config.threads);

    if parse_hotkey(&config.hotkey).is_none() {
        eprintln!("[SS9K] Unknown hotkey '{}', will default to F12", config.hotkey);
    }

    println!("=================================");
    println!("   SuperScreecher9000 v0.14.0");
    println!("   Press {} to screech", config.hotkey);
    println!("=================================");

    print_help();

    if config.activation_mode == "vad" {
        println!("[SS9K] Activation: VAD (voice activity detection)");
        println!("[SS9K] Hotkey: {} (toggles VAD listening)", config.hotkey);
        println!("[SS9K] VAD: sensitivity={}, silence={}ms, min_speech={}ms",
                 config.vad_sensitivity, config.vad_silence_ms, config.vad_min_speech_ms);
    } else {
        println!("[SS9K] Activation: hotkey ({})", config.hotkey_mode);
        println!("[SS9K] Hotkey: {} (mode: {})", config.hotkey, config.hotkey_mode);
    }
    if !config.command_hotkey.is_empty() {
        println!("[SS9K] Command hotkey: {} (auto-prefixes '{}')", config.command_hotkey, config.leader);
    }
    if !config.commands.is_empty() {
        println!("[SS9K] Custom commands: {} loaded", config.commands.len());
    }
    if !config.aliases.is_empty() {
        println!("[SS9K] Aliases: {} loaded", config.aliases.len());
    }
    if !config.inserts.is_empty() {
        println!("[SS9K] Inserts: {} loaded", config.inserts.len());
    }
    if !config.wrappers.is_empty() {
        println!("[SS9K] Wrappers: {} loaded", config.wrappers.len());
    }

    // Check if model exists, download if not
    let model_filename = config.model_filename();
    let mut model_path = get_model_path(&model_filename);

    if !model_path.exists() {
        println!("[SS9K] Model '{}' not found locally", config.model);
        let install_path = get_model_install_path(&model_filename);
        println!("[SS9K] Will download to: {:?}", install_path);

        download_model(&config.model_url(), &install_path)?;
        model_path = install_path;
    }

    // Load whisper model
    println!("[SS9K] Loading whisper model from: {:?}", model_path);
    let ctx = WhisperContext::new_with_params(
        model_path.to_str().expect("Invalid model path"),
        WhisperContextParameters::default()
    ).expect("Failed to load whisper model");
    let ctx = Arc::new(ctx);
    let config = Arc::new(ArcSwap::from_pointee(config));
    println!("[SS9K] Model loaded!");

    // Set up config hot-reload
    if let Some(ref path) = config_path {
        let config_for_watcher = config.clone();
        let watch_path = path.clone();
        std::thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = match recommended_watcher(tx) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("[SS9K] Failed to create config watcher: {}", e);
                    return;
                }
            };
            if let Err(e) = watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
                eprintln!("[SS9K] Failed to watch config file: {}", e);
                return;
            }
            println!("[SS9K] 👀 Watching config for changes: {:?}", watch_path);

            for event in rx {
                if let Ok(event) = event {
                    if event.kind.is_modify() {
                        std::thread::sleep(Duration::from_millis(100));
                        if let Some(new_config) = Config::load_from(&watch_path) {
                            config_for_watcher.store(Arc::new(new_config));
                            println!("[SS9K] 🔄 Config reloaded!");
                        }
                    }
                }
            }
        });
    }

    let host = cpal::default_host();
    println!("[SS9K] Host: {:?}", host.id());

    // Find microphone device
    let cfg = config.load();
    let device = if !cfg.device.is_empty() {
        let device_name = cfg.device.clone();
        host.input_devices()?
            .find(|d| d.name().map(|n| n.contains(&device_name)).unwrap_or(false))
            .or_else(|| {
                eprintln!("[SS9K] Configured device '{}' not found, using default", device_name);
                host.default_input_device()
            })
    } else {
        host.input_devices()?
            .find(|d| d.name().map(|n| is_microphone(&n)).unwrap_or(false))
            .or_else(|| host.default_input_device())
    }.expect("No input device available");
    println!("[SS9K] Device: {}", device.name()?);

    let audio_config = device.default_input_config()?;
    println!("[SS9K] Audio config: {:?}", audio_config);

    let sample_rate = audio_config.sample_rate().0;
    let channels = audio_config.channels() as usize;

    let is_vad_mode = cfg.activation_mode == "vad";

    // Shared state
    let audio_buffer: AudioBuffer = Arc::new(Mutex::new(Vec::new()));
    let recording_arc = Arc::new(AtomicBool::new(false));

    // Create audio channel for processor
    let (audio_tx, audio_rx) = mpsc::channel::<AudioMessage>();

    // Create wake word result channel (processor -> VAD thread)
    let (wake_word_tx, wake_word_rx) = mpsc::channel::<bool>();

    // Build stream based on activation mode
    let stream = if is_vad_mode {
        println!("[SS9K] 🎤 VAD mode enabled");

        // Create VAD audio channel
        let (vad_audio_tx, vad_audio_rx) = mpsc::channel::<Vec<f32>>();

        // Build VAD stream
        let err_fn = |err| eprintln!("[SS9K] Stream error: {}", err);
        let stream = match audio_config.sample_format() {
            cpal::SampleFormat::I8 => build_stream_with_vad::<i8>(&device, &audio_config.clone().into(), vad_audio_tx.clone(), channels, err_fn)?,
            cpal::SampleFormat::I16 => build_stream_with_vad::<i16>(&device, &audio_config.clone().into(), vad_audio_tx.clone(), channels, err_fn)?,
            cpal::SampleFormat::I32 => build_stream_with_vad::<i32>(&device, &audio_config.clone().into(), vad_audio_tx.clone(), channels, err_fn)?,
            cpal::SampleFormat::F32 => build_stream_with_vad::<f32>(&device, &audio_config.clone().into(), vad_audio_tx, channels, err_fn)?,
            format => {
                eprintln!("[SS9K] Unsupported sample format: {:?}", format);
                return Ok(());
            }
        };

        // Spawn VAD processor thread
        {
            let audio_tx = audio_tx.clone();
            let config = config.clone();
            let wake_word_rx = wake_word_rx; // Move receiver to VAD thread
            std::thread::spawn(move || {
                let cfg = config.load();
                println!("[SS9K] 🎤 VAD thread starting (sensitivity: {}, silence: {}ms, min_speech: {}ms, pad: {}ms)",
                         cfg.vad_sensitivity, cfg.vad_silence_ms, cfg.vad_min_speech_ms, cfg.vad_speech_pad_ms);

                // Initialize VAD
                let mut vad = match Vad::new(cfg.vad_sensitivity, cfg.vad_silence_ms, cfg.vad_min_speech_ms, cfg.vad_speech_pad_ms) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[SS9K] ❌ Failed to initialize VAD: {}", e);
                        return;
                    }
                };

                // Enable wake word mode if configured
                if !cfg.wake_word.is_empty() {
                    vad.set_wake_word_enabled(true);
                    println!("[SS9K] 🗣️ Wake word mode enabled: '{}'", cfg.wake_word);
                }

                // Buffer for accumulating audio to resample
                let mut native_buffer: Vec<f32> = Vec::new();

                // Process audio chunks
                for chunk in vad_audio_rx {
                    // Check for wake word results (non-blocking)
                    while let Ok(wake_word_found) = wake_word_rx.try_recv() {
                        if !wake_word_found {
                            // Wake word not found - abort current utterance
                            let cfg = config.load();
                            if cfg.verbose {
                                println!("[SS9K] ❌ Wake word not detected, aborting utterance");
                            }
                            vad.abort_utterance();
                            native_buffer.clear();
                        } else {
                            let cfg = config.load();
                            if cfg.verbose {
                                println!("[SS9K] ✅ Wake word confirmed, continuing...");
                            }
                        }
                    }

                    // Reload config for hot-reload support
                    let cfg = config.load();

                    // Check if we should be listening
                    if !VAD_LISTENING.load(Ordering::SeqCst) {
                        // Not listening - reset VAD state if needed
                        if vad.state() != VadState::Idle {
                            vad.stop_listening();
                            native_buffer.clear();
                        }
                        continue;
                    }

                    // Start listening if not already
                    if vad.state() == VadState::Idle {
                        vad.start_listening();
                        native_buffer.clear();
                        if cfg.audio_feedback { beep(); }
                        println!("[SS9K] 🎤 VAD listening...");
                    }

                    // Accumulate audio
                    native_buffer.extend_from_slice(&chunk);

                    // Resample when we have enough samples
                    // Resample in chunks to avoid latency
                    let min_chunk = (sample_rate as usize) / 10; // 100ms chunks
                    while native_buffer.len() >= min_chunk {
                        let to_resample: Vec<f32> = native_buffer.drain(..min_chunk).collect();

                        // Resample to 16kHz for VAD
                        match resample_audio(&to_resample, sample_rate, VAD_SAMPLE_RATE) {
                            Ok(resampled) => {
                                // Feed to VAD
                                let events = vad.feed(&resampled);

                                for event in events {
                                    match event {
                                        VadEvent::StateChanged(state) => {
                                            let cfg = config.load();
                                            match state {
                                                VadState::Speaking => {
                                                    if cfg.verbose {
                                                        println!("[SS9K] 🗣️ Speech detected!");
                                                    }
                                                }
                                                VadState::SilenceDetected => {
                                                    if cfg.verbose {
                                                        println!("[SS9K] 🤫 Silence detected, waiting...");
                                                    }
                                                }
                                                VadState::Listening => {
                                                    if cfg.verbose {
                                                        println!("[SS9K] 👂 Listening for speech...");
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                        VadEvent::WakeWordCheckReady(audio) => {
                                            let cfg = config.load();
                                            if cfg.verbose {
                                                let duration = audio.len() as f32 / VAD_SAMPLE_RATE as f32;
                                                println!("[SS9K] 🔍 Sending {:.2}s for wake word check...", duration);
                                            }
                                            // Send for async wake word check
                                            if let Err(e) = audio_tx.send(AudioMessage::WakeWordCheck(audio)) {
                                                eprintln!("[SS9K] ❌ Failed to send wake word check: {}", e);
                                            }
                                        }
                                        VadEvent::ReadyToProcess(audio) => {
                                            let cfg = config.load();
                                            let duration = audio.len() as f32 / VAD_SAMPLE_RATE as f32;
                                            println!("[SS9K] 📤 VAD: Sending {:.2}s of speech for transcription", duration);

                                            // Clear native buffer to start fresh for next utterance
                                            native_buffer.clear();

                                            // Send already-resampled audio to processor
                                            if let Err(e) = audio_tx.send(AudioMessage::AlreadyResampled(audio)) {
                                                eprintln!("[SS9K] ❌ Failed to send VAD audio: {}", e);
                                            } else if cfg.audio_feedback {
                                                beep_done();
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if cfg.verbose {
                                    eprintln!("[SS9K] ⚠️ VAD resample error: {}", e);
                                }
                            }
                        }
                    }
                }
                println!("[SS9K] 🎤 VAD thread exiting");
            });
        }

        stream
    } else {
        // Hotkey mode - use existing stream
        let buffer_clone = audio_buffer.clone();
        let recording_for_stream = recording_arc.clone();
        let err_fn = |err| eprintln!("[SS9K] Stream error: {}", err);

        match audio_config.sample_format() {
            cpal::SampleFormat::I8 => build_stream::<i8>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream, err_fn)?,
            cpal::SampleFormat::I16 => build_stream::<i16>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream.clone(), err_fn)?,
            cpal::SampleFormat::I32 => build_stream::<i32>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream.clone(), err_fn)?,
            cpal::SampleFormat::F32 => build_stream::<f32>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream.clone(), err_fn)?,
            format => {
                eprintln!("[SS9K] Unsupported sample format: {:?}", format);
                return Ok(());
            }
        }
    };

    stream.play()?;
    if is_vad_mode {
        println!("[SS9K] Stream playing. Press {} to toggle VAD listening...", cfg.hotkey);
    } else {
        println!("[SS9K] Stream playing. Press {} to record...", cfg.hotkey);
    }

    // Spawn processor thread
    {
        let ctx = ctx.clone();
        let config = config.clone();
        let wake_word_tx = wake_word_tx; // Move sender to processor thread
        std::thread::spawn(move || {
            println!("[SS9K] 🔧 Processor thread started");
            for audio_msg in audio_rx {
                let cfg = config.load();
                let verbose = cfg.verbose;
                let timeout_secs = cfg.processing_timeout_secs;

                let start_time = std::time::Instant::now();

                // Handle wake word check separately (early return)
                if let AudioMessage::WakeWordCheck(audio_data) = audio_msg {
                    if verbose {
                        println!("[SS9K] 🔍 Checking for wake word '{}'...", cfg.wake_word);
                    }

                    // Quick transcription of the audio
                    match transcribe(&ctx, &audio_data, &cfg) {
                        Ok(check_text) => {
                            let check_lower = check_text.to_lowercase();
                            let wake_lower = cfg.wake_word.to_lowercase();
                            let found = check_lower.contains(&wake_lower);

                            if verbose {
                                if found {
                                    println!("[SS9K] ✅ Wake word '{}' found in: \"{}\"", cfg.wake_word, check_text.trim());
                                } else {
                                    println!("[SS9K] ❌ Wake word '{}' not found in: \"{}\"", cfg.wake_word, check_text.trim());
                                }
                            }

                            // Send result back to VAD thread
                            let _ = wake_word_tx.send(found);
                        }
                        Err(e) => {
                            log_warn(&cfg.error_log, &format!("Wake word check failed: {}", e));
                            // On error, assume wake word found (don't reject)
                            let _ = wake_word_tx.send(true);
                        }
                    }
                    continue; // Don't process further
                }

                // Track if this is VAD audio (for wake word stripping)
                let is_vad_audio = matches!(&audio_msg, AudioMessage::AlreadyResampled(_));

                // Get resampled audio based on message type
                let resampled = match audio_msg {
                    AudioMessage::NeedsResampling(audio_data) => {
                        if verbose {
                            println!("[SS9K] 🔄 Processing {} samples...", audio_data.len());
                        }
                        match resample_audio(&audio_data, sample_rate, WHISPER_SAMPLE_RATE) {
                            Ok(r) => {
                                if verbose {
                                    println!("[SS9K] 🔄 Resampled to {} samples at 16kHz", r.len());
                                }
                                r
                            }
                            Err(e) => {
                                log_error(&cfg.error_log, &format!("Resample error: {}", e));
                                continue;
                            }
                        }
                    }
                    AudioMessage::AlreadyResampled(audio_data) => {
                        if verbose {
                            println!("[SS9K] 🔄 Processing {} pre-resampled samples...", audio_data.len());
                        }
                        audio_data
                    }
                    AudioMessage::WakeWordCheck(_) => {
                        // Already handled above with early continue
                        unreachable!()
                    }
                };

                // Wake word check for VAD mode
                if is_vad_audio && !cfg.wake_word.is_empty() {
                    // Check first ~1.2s for wake word
                    let check_samples = (WHISPER_SAMPLE_RATE as f32 * 1.2) as usize;
                    let check_audio = if resampled.len() > check_samples {
                        &resampled[..check_samples]
                    } else {
                        &resampled[..]
                    };

                    if verbose {
                        println!("[SS9K] 🔍 Checking for wake word '{}'...", cfg.wake_word);
                    }

                    // Quick transcription of first segment
                    match transcribe(&ctx, check_audio, &cfg) {
                        Ok(check_text) => {
                            let check_lower = check_text.to_lowercase();
                            let wake_lower = cfg.wake_word.to_lowercase();
                            if !check_lower.contains(&wake_lower) {
                                if verbose {
                                    println!("[SS9K] ❌ Wake word '{}' not found in: \"{}\"", cfg.wake_word, check_text.trim());
                                }
                                continue; // Skip this utterance
                            }
                            if verbose {
                                println!("[SS9K] ✅ Wake word detected!");
                            }
                        }
                        Err(e) => {
                            log_warn(&cfg.error_log, &format!("Wake word check failed: {}", e));
                            // Continue anyway - don't reject on error
                        }
                    }
                }

                // Run transcription with optional timeout
                let transcribe_result = if timeout_secs > 0 {
                    // Spawn transcription in a thread and wait with timeout
                    let (tx, rx) = mpsc::channel();
                    let ctx_clone = ctx.clone();
                    let cfg_clone = cfg.clone();
                    let resampled_clone = resampled.clone();

                    std::thread::spawn(move || {
                        let result = transcribe(&ctx_clone, &resampled_clone, &cfg_clone);
                        let _ = tx.send(result); // Ignore send error if receiver dropped
                    });

                    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
                        Ok(result) => result,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let elapsed = start_time.elapsed().as_secs_f32();
                            log_warn(&cfg.error_log, &format!("TIMEOUT: Processing exceeded {}s limit (ran for {:.1}s). Tip: Try a smaller model (tiny/base) or increase processing_timeout_secs", timeout_secs, elapsed));
                            COMMAND_MODE.store(false, Ordering::SeqCst); // Reset command mode
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            log_error(&cfg.error_log, "Transcription thread crashed");
                            COMMAND_MODE.store(false, Ordering::SeqCst);
                            continue;
                        }
                    }
                } else {
                    // No timeout - blocking call
                    transcribe(&ctx, &resampled, &cfg)
                };

                let elapsed = start_time.elapsed().as_secs_f32();

                match transcribe_result {
                    Ok(text) => {
                        // If command_hotkey was used, prepend the leader word
                        let text = if COMMAND_MODE.load(Ordering::SeqCst) {
                            COMMAND_MODE.store(false, Ordering::SeqCst); // Reset for next recording
                            format!("{} {}", cfg.leader, text)
                        } else {
                            text
                        };

                        // Strip wake word from beginning if present (VAD mode only)
                        let text = if is_vad_audio && !cfg.wake_word.is_empty() {
                            let text_lower = text.to_lowercase();
                            let wake_lower = cfg.wake_word.to_lowercase();
                            if text_lower.starts_with(&wake_lower) {
                                // Strip wake word and any following whitespace
                                text[cfg.wake_word.len()..].trim_start().to_string()
                            } else {
                                text
                            }
                        } else {
                            text
                        };

                        if verbose {
                            println!("[SS9K] 📝 Transcription ({:.1}s): {}", elapsed, text);
                        }

                        // Log to dictation log if configured
                        log_dictation(&cfg.dictation_log, &text);

                        if !text.is_empty() {
                            // Update key repeat rate from config
                            set_key_repeat_ms(cfg.key_repeat_ms);

                            match Enigo::new(&Settings::default()) {
                                Ok(mut enigo) => {
                                    if let Err(e) = execute_command(&mut enigo, &text, &cfg.leader, &cfg.commands, &cfg.aliases, &cfg.inserts, &cfg.wrappers) {
                                        log_error(&cfg.error_log, &format!("Command/Type error: {}", e));
                                    } else if cfg.audio_feedback {
                                        beep_done();
                                    }
                                }
                                Err(e) => log_error(&cfg.error_log, &format!("Enigo init error: {}", e)),
                            }
                        }
                    }
                    Err(e) => log_error(&cfg.error_log, &format!("Transcription error ({:.1}s): {}", elapsed, e)),
                }
            }
            println!("[SS9K] 🔧 Processor thread exiting");
        });
    }

    let buffer_for_kb = audio_buffer.clone();
    let config_for_kb = config.clone();
    let recording_for_kb = recording_arc.clone();

    let send_audio = {
        let buffer = buffer_for_kb.clone();
        let tx = audio_tx.clone();
        Arc::new(move || {
            let audio_data = if let Ok(buf) = buffer.lock() {
                let duration = buf.len() as f32 / sample_rate as f32;
                let callbacks = CALLBACK_COUNT.load(Ordering::SeqCst);
                println!(
                    "[SS9K] 🛑 Stopped. {} samples ({:.2}s), {} callbacks",
                    buf.len(), duration, callbacks
                );
                buf.clone()
            } else {
                Vec::new()
            };

            if !audio_data.is_empty() {
                if let Err(e) = tx.send(AudioMessage::NeedsResampling(audio_data)) {
                    eprintln!("[SS9K] ❌ Failed to queue audio: {}", e);
                } else {
                    println!("[SS9K] 📤 Audio queued for processing");
                }
            }
        })
    };

    let send_audio_for_timeout = send_audio.clone();
    let config_for_timeout = config_for_kb.clone();
    let recording_for_timeout = recording_for_kb.clone();

    let callback = move |event: Event| {
        let cfg = config_for_kb.load();
        let current_hotkey = parse_hotkey(&cfg.hotkey).unwrap_or(RdevKey::F12);
        let command_hotkey = parse_hotkey(&cfg.command_hotkey); // None if empty/invalid
        let is_toggle_mode = cfg.hotkey_mode == "toggle";
        let toggle_timeout = cfg.toggle_timeout_secs;
        let is_vad_mode = cfg.activation_mode == "vad";

        // Check if this key is one of our hotkeys
        let is_dictation_key = |key: RdevKey| key == current_hotkey;
        let is_command_key = |key: RdevKey| command_hotkey.map_or(false, |ck| key == ck);
        let is_our_hotkey = |key: RdevKey| is_dictation_key(key) || is_command_key(key);

        match event.event_type {
            EventType::KeyPress(key) if is_our_hotkey(key) => {
                // VAD mode: hotkey toggles listening
                if is_vad_mode {
                    let was_listening = VAD_LISTENING.load(Ordering::SeqCst);
                    VAD_LISTENING.store(!was_listening, Ordering::SeqCst);

                    if was_listening {
                        println!("[SS9K] 🔇 VAD listening stopped");
                    } else {
                        println!("[SS9K] 🎤 VAD listening started (press {} to stop)", cfg.hotkey);
                    }
                    return;
                }

                // Hotkey mode: original behavior
                let using_command_key = is_command_key(key);
                if is_toggle_mode {
                    if recording_for_kb.load(Ordering::SeqCst) {
                        recording_for_kb.store(false, Ordering::SeqCst);
                        RECORDING.store(false, Ordering::SeqCst);
                        send_audio();
                    } else {
                        if let Ok(mut buf) = buffer_for_kb.lock() {
                            buf.clear();
                        }
                        CALLBACK_COUNT.store(0, Ordering::SeqCst);

                        let session_id = RECORDING_SESSION.fetch_add(1, Ordering::SeqCst) + 1;
                        recording_for_kb.store(true, Ordering::SeqCst);
                        RECORDING.store(true, Ordering::SeqCst);
                        COMMAND_MODE.store(using_command_key, Ordering::SeqCst);

                        let hotkey_name = if using_command_key { cfg.command_hotkey.clone() } else { cfg.hotkey.clone() };
                        if cfg.audio_feedback { beep(); }
                        if toggle_timeout > 0 {
                            println!("[SS9K] 🎙️ Recording... ({} to stop, or {}s timeout)", hotkey_name, toggle_timeout);

                            let send_audio_timeout = send_audio_for_timeout.clone();
                            let config_timeout = config_for_timeout.clone();
                            let recording_timeout = recording_for_timeout.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_secs(toggle_timeout));

                                if RECORDING_SESSION.load(Ordering::SeqCst) == session_id
                                   && recording_timeout.load(Ordering::SeqCst) {
                                    let cfg = config_timeout.load();
                                    println!("[SS9K] ⏱️ Timeout reached! (was recording with {})", cfg.hotkey);
                                    recording_timeout.store(false, Ordering::SeqCst);
                                    RECORDING.store(false, Ordering::SeqCst);
                                    send_audio_timeout();
                                }
                            });
                        } else {
                            println!("[SS9K] 🎙️ Recording... (press {} again to stop)", hotkey_name);
                        }
                    }
                } else {
                    if !recording_for_kb.load(Ordering::SeqCst) {
                        if let Ok(mut buf) = buffer_for_kb.lock() {
                            buf.clear();
                        }
                        CALLBACK_COUNT.store(0, Ordering::SeqCst);
                        recording_for_kb.store(true, Ordering::SeqCst);
                        RECORDING.store(true, Ordering::SeqCst);
                        COMMAND_MODE.store(using_command_key, Ordering::SeqCst);
                        if cfg.audio_feedback { beep(); }
                        if using_command_key {
                            println!("[SS9K] 🎙️ Recording (command mode)...");
                        } else {
                            println!("[SS9K] 🎙️ Recording...");
                        }
                    }
                }
            }
            EventType::KeyRelease(key) if is_our_hotkey(key) => {
                // VAD mode doesn't use key release
                if is_vad_mode {
                    return;
                }

                if !is_toggle_mode {
                    if recording_for_kb.load(Ordering::SeqCst) {
                        recording_for_kb.store(false, Ordering::SeqCst);
                        RECORDING.store(false, Ordering::SeqCst);
                        send_audio();
                    }
                }
            }
            _ => {}
        }
    };

    listen(callback).map_err(|e| anyhow::anyhow!("Listen error: {:?}", e))?;
    Ok(())
}
