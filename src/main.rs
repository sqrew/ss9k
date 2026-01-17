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
use commands::{execute_command, print_help};
use model::{download_model, get_model_install_path, get_model_path};

// Recording state
static RECORDING: AtomicBool = AtomicBool::new(false);
static RECORDING_SESSION: AtomicU64 = AtomicU64::new(0);

/// Configuration for SS9K
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub model: String,
    pub language: String,
    pub threads: usize,
    pub device: String,
    pub hotkey: String,
    pub hotkey_mode: String,
    pub toggle_timeout_secs: u64,
    pub leader: String,
    #[serde(default)]
    pub commands: HashMap<String, String>,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
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
            hotkey_mode: "hold".to_string(),
            toggle_timeout_secs: 0,
            leader: "command".to_string(),
            commands: HashMap::new(),
            aliases: HashMap::new(),
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

        println!("[SS9K] Using default config");
        (Self::default(), None)
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
    if !config.commands.is_empty() {
        println!("[SS9K] Custom commands: {} loaded", config.commands.len());
    }
    if !config.aliases.is_empty() {
        println!("[SS9K] Aliases: {} loaded", config.aliases.len());
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

                if !quiet {
                    println!("[SS9K] 🔄 Processing {} samples...", audio_data.len());
                }

                match resample_audio(&audio_data, sample_rate, WHISPER_SAMPLE_RATE) {
                    Ok(resampled) => {
                        if !quiet {
                            println!("[SS9K] 🔄 Resampled to {} samples at 16kHz", resampled.len());
                        }

                        match transcribe(&ctx, &resampled, &cfg) {
                            Ok(text) => {
                                if !quiet {
                                    println!("[SS9K] 📝 Transcription: {}", text);
                                }
                                if !text.is_empty() {
                                    match Enigo::new(&Settings::default()) {
                                        Ok(mut enigo) => {
                                            if let Err(e) = execute_command(&mut enigo, &text, &cfg.leader, &cfg.commands, &cfg.aliases) {
                                                eprintln!("[SS9K] ❌ Command/Type error: {}", e);
                                            }
                                        }
                                        Err(e) => eprintln!("[SS9K] ❌ Enigo init error: {}", e),
                                    }
                                }
                            }
                            Err(e) => eprintln!("[SS9K] ❌ Transcription error: {}", e),
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
        let is_toggle_mode = cfg.hotkey_mode == "toggle";
        let toggle_timeout = cfg.toggle_timeout_secs;

        match event.event_type {
            EventType::KeyPress(key) if key == current_hotkey => {
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

                        let hotkey_name = cfg.hotkey.clone();
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
                        println!("[SS9K] 🎙️ Recording...");
                    }
                }
            }
            EventType::KeyRelease(key) if key == current_hotkey => {
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
