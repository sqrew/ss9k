mod audio;
mod commands;
mod lookups;
mod model;

use anyhow::Result;
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use enigo::{Enigo, Settings};
use notify::{recommended_watcher, RecursiveMode, Watcher};
use rdev::{listen, Event, EventType, Key as RdevKey};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use whisper_rs::{WhisperContext, WhisperContextParameters};

use audio::{build_stream, is_microphone, resample_audio, transcribe, AudioBuffer, CALLBACK_COUNT, WHISPER_SAMPLE_RATE};
use commands::{execute_command, print_help, set_key_repeat_ms};
use model::{download_model, get_model_install_path, get_model_path};

// Recording state
static RECORDING: AtomicBool = AtomicBool::new(false);
static RECORDING_SESSION: AtomicU64 = AtomicU64::new(0);
static COMMAND_MODE: AtomicBool = AtomicBool::new(false); // True if recording was started with command_hotkey

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
    pub commands: HashMap<String, String>,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
    #[serde(default)]
    pub inserts: HashMap<String, String>,
    #[serde(default)]
    pub wrappers: HashMap<String, String>,
    #[serde(default)]
    pub quiet: bool,
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
            commands: HashMap::new(),
            aliases: HashMap::new(),
            inserts: HashMap::new(),
            wrappers: HashMap::new(),
            quiet: false,
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

# Language for transcription
# Use ISO 639-1 codes: en, es, fr, de, ja, zh, etc.
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

# Suppress verbose output (processing, resampling, transcription logs)
# Errors still print. Set true once you're comfortable with the tool.
quiet = false

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
    println!("   SuperScreecher9000 v0.13.0");
    println!("   Press {} to screech", config.hotkey);
    println!("=================================");

    print_help();

    println!("[SS9K] Hotkey: {} (mode: {})", config.hotkey, config.hotkey_mode);
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

    let audio_buffer: AudioBuffer = Arc::new(Mutex::new(Vec::new()));
    let buffer_clone = audio_buffer.clone();

    // Create Arc for recording state to pass to build_stream
    let recording_arc = Arc::new(AtomicBool::new(false));
    let recording_for_stream = recording_arc.clone();

    let err_fn = |err| eprintln!("[SS9K] Stream error: {}", err);

    let stream = match audio_config.sample_format() {
        cpal::SampleFormat::I8 => build_stream::<i8>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream, err_fn)?,
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream.clone(), err_fn)?,
        cpal::SampleFormat::I32 => build_stream::<i32>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream.clone(), err_fn)?,
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &audio_config.into(), buffer_clone, channels, recording_for_stream.clone(), err_fn)?,
        format => {
            eprintln!("[SS9K] Unsupported sample format: {:?}", format);
            return Ok(());
        }
    };

    stream.play()?;
    println!("[SS9K] Stream playing. Waiting for F12...");

    let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();

    // Spawn processor thread
    {
        let ctx = ctx.clone();
        let config = config.clone();
        std::thread::spawn(move || {
            println!("[SS9K] 🔧 Processor thread started");
            for audio_data in audio_rx {
                let cfg = config.load();
                let quiet = cfg.quiet;
                let timeout_secs = cfg.processing_timeout_secs;

                let start_time = std::time::Instant::now();
                if !quiet {
                    println!("[SS9K] 🔄 Processing {} samples...", audio_data.len());
                }

                match resample_audio(&audio_data, sample_rate, WHISPER_SAMPLE_RATE) {
                    Ok(resampled) => {
                        if !quiet {
                            println!("[SS9K] 🔄 Resampled to {} samples at 16kHz", resampled.len());
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
                                    eprintln!("[SS9K] ⏱️ TIMEOUT: Processing exceeded {}s limit (ran for {:.1}s)", timeout_secs, elapsed);
                                    eprintln!("[SS9K] 💡 Tip: Try a smaller model (tiny/base) or increase processing_timeout_secs");
                                    COMMAND_MODE.store(false, Ordering::SeqCst); // Reset command mode
                                    continue;
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    eprintln!("[SS9K] ❌ Transcription thread crashed");
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

                                if !quiet {
                                    println!("[SS9K] 📝 Transcription ({:.1}s): {}", elapsed, text);
                                }
                                if !text.is_empty() {
                                    // Update key repeat rate from config
                                    set_key_repeat_ms(cfg.key_repeat_ms);

                                    match Enigo::new(&Settings::default()) {
                                        Ok(mut enigo) => {
                                            if let Err(e) = execute_command(&mut enigo, &text, &cfg.leader, &cfg.commands, &cfg.aliases, &cfg.inserts, &cfg.wrappers) {
                                                eprintln!("[SS9K] ❌ Command/Type error: {}", e);
                                            }
                                        }
                                        Err(e) => eprintln!("[SS9K] ❌ Enigo init error: {}", e),
                                    }
                                }
                            }
                            Err(e) => eprintln!("[SS9K] ❌ Transcription error ({:.1}s): {}", elapsed, e),
                        }
                    }
                    Err(e) => eprintln!("[SS9K] ❌ Resample error: {}", e),
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
                if let Err(e) = tx.send(audio_data) {
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

        // Check if this key is one of our hotkeys
        let is_dictation_key = |key: RdevKey| key == current_hotkey;
        let is_command_key = |key: RdevKey| command_hotkey.map_or(false, |ck| key == ck);
        let is_our_hotkey = |key: RdevKey| is_dictation_key(key) || is_command_key(key);

        match event.event_type {
            EventType::KeyPress(key) if is_our_hotkey(key) => {
                // Set command mode if this is the command hotkey
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
                        if using_command_key {
                            println!("[SS9K] 🎙️ Recording (command mode)...");
                        } else {
                            println!("[SS9K] 🎙️ Recording...");
                        }
                    }
                }
            }
            EventType::KeyRelease(key) if is_our_hotkey(key) => {
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
