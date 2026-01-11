use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Sample;
use enigo::{Enigo, Key as EnigoKey, Keyboard, Settings};
use indicatif::{ProgressBar, ProgressStyle};
use rdev::{listen, Event, EventType, Key as RdevKey};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

static RECORDING: AtomicBool = AtomicBool::new(false);
static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
// Session ID to prevent stale timeout threads from stopping new recordings
static RECORDING_SESSION: AtomicU64 = AtomicU64::new(0);

const WHISPER_SAMPLE_RATE: u32 = 16000;

type AudioBuffer = Arc<Mutex<Vec<f32>>>;

/// Configuration for SS9K
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// Model to use: tiny, base, small, medium, large
    pub model: String,
    /// Language for transcription
    pub language: String,
    /// Number of threads for whisper
    pub threads: usize,
    /// Specific device name (empty = auto-detect)
    pub device: String,
    /// Hotkey to trigger recording (e.g., "F12", "ScrollLock", "Pause")
    pub hotkey: String,
    /// Hotkey mode: "hold" (press to start, release to stop) or "toggle" (press to start, press again to stop)
    pub hotkey_mode: String,
    /// Timeout in seconds for toggle mode (0 = no timeout)
    pub toggle_timeout_secs: u64,
    /// Custom voice commands mapping phrase -> shell command
    #[serde(default)]
    pub commands: HashMap<String, String>,
    /// Aliases for normalizing common misrecognitions (e.g., "e max" -> "emacs")
    #[serde(default)]
    pub aliases: HashMap<String, String>,
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
            commands: HashMap::new(),
            aliases: HashMap::new(),
        }
    }
}

/// Parse a hotkey string into an rdev::Key
fn parse_hotkey(s: &str) -> Option<RdevKey> {
    match s.to_uppercase().as_str() {
        // Function keys
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
        // Lock keys (good for dedicated hotkeys)
        "SCROLLLOCK" | "SCROLL_LOCK" | "SCROLL" => Some(RdevKey::ScrollLock),
        "PAUSE" | "BREAK" => Some(RdevKey::Pause),
        "PRINTSCREEN" | "PRINT_SCREEN" | "PRTSC" => Some(RdevKey::PrintScreen),
        "INSERT" | "INS" => Some(RdevKey::Insert),
        "HOME" => Some(RdevKey::Home),
        "END" => Some(RdevKey::End),
        "PAGEUP" | "PAGE_UP" | "PGUP" => Some(RdevKey::PageUp),
        "PAGEDOWN" | "PAGE_DOWN" | "PGDN" => Some(RdevKey::PageDown),
        // Numpad
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
    /// Load config from file, or return defaults
    pub fn load() -> Self {
        let config_paths = [
            // 1. XDG config dir
            dirs::config_dir().map(|p| p.join("ss9k").join("config.toml")),
            // 2. Home dir fallback
            dirs::home_dir().map(|p| p.join(".ss9k").join("config.toml")),
            // 3. Current directory (development)
            Some(PathBuf::from("config.toml")),
        ];

        for path in config_paths.into_iter().flatten() {
            if path.exists() {
                if let Ok(contents) = fs::read_to_string(&path) {
                    match toml::from_str(&contents) {
                        Ok(config) => {
                            println!("[SS9K] Loaded config from: {:?}", path);
                            return config;
                        }
                        Err(e) => {
                            eprintln!("[SS9K] Config parse error in {:?}: {}", path, e);
                        }
                    }
                }
            }
        }

        println!("[SS9K] Using default config");
        Self::default()
    }

    /// Get the model filename
    pub fn model_filename(&self) -> String {
        format!("ggml-{}.bin", self.model)
    }

    /// Get the HuggingFace URL for the model
    pub fn model_url(&self) -> String {
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin",
            self.model
        )
    }
}

/// Normalize text by applying aliases (e.g., "e max" -> "emacs")
fn normalize_aliases(text: &str, aliases: &HashMap<String, String>) -> String {
    let mut result = text.to_lowercase();
    for (from, to) in aliases {
        result = result.replace(&from.to_lowercase(), to);
    }
    result
}

/// Expand environment variables in a string (e.g., "$TERMINAL" -> "kitty")
fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    // Find all $VAR patterns and expand them
    while let Some(start) = result.find('$') {
        // Find the end of the variable name (non-alphanumeric/underscore)
        let rest = &result[start + 1..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        let var_name = &rest[..end];

        if var_name.is_empty() {
            break;
        }

        let value = std::env::var(var_name).unwrap_or_default();
        result = format!("{}{}{}", &result[..start], value, &rest[end..]);
    }
    result
}

/// Execute a custom shell command
fn execute_custom_command(cmd: &str) -> Result<()> {
    let expanded = expand_env_vars(cmd);

    if expanded.trim().is_empty() {
        eprintln!("[SS9K] ⚠️ Command expanded to empty string (check env vars): {}", cmd);
        return Ok(());
    }

    println!("[SS9K] 🚀 Executing: {}", expanded);

    // Spawn in a separate thread to avoid blocking and properly detach
    let cmd_owned = expanded.to_string();
    std::thread::spawn(move || {
        #[cfg(target_os = "windows")]
        let result = std::process::Command::new("cmd")
            .args(["/C", &cmd_owned])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        #[cfg(not(target_os = "windows"))]
        let result = {
            // For simple single-word commands, run directly
            // For complex commands (with spaces, pipes, etc), use shell
            let parts: Vec<&str> = cmd_owned.split_whitespace().collect();
            if parts.len() == 1 && !cmd_owned.contains(['|', '&', ';', '>', '<', '$', '`', '(', ')']) {
                // Simple command - run directly
                std::process::Command::new(&cmd_owned)
                    .spawn()
            } else {
                // Complex command - use shell
                std::process::Command::new("sh")
                    .args(["-c", &cmd_owned])
                    .spawn()
            }
        };

        match result {
            Ok(mut child) => {
                // Wait briefly to see if it fails immediately
                std::thread::sleep(std::time::Duration::from_millis(100));
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            eprintln!("[SS9K] ⚠️ Command exited with: {}", status);
                        }
                    }
                    Ok(None) => println!("[SS9K] ✅ Command running"),
                    Err(e) => eprintln!("[SS9K] ❌ Error checking command: {}", e),
                }
            }
            Err(e) => eprintln!("[SS9K] ❌ Failed to spawn: {}", e),
        }
    });

    Ok(())
}

/// Execute a voice command or type the text
/// Returns true if a command was executed, false if text was typed
fn execute_command(enigo: &mut Enigo, text: &str, custom_commands: &HashMap<String, String>, aliases: &HashMap<String, String>) -> Result<bool> {
    // First normalize using aliases
    let aliased = normalize_aliases(text, aliases);

    // Strip punctuation for command matching
    let trimmed: String = aliased
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();

    // Check for commands
    match trimmed.as_str() {
        // Navigation
        "enter" | "new line" | "newline" | "return" => {
            enigo.key(EnigoKey::Return, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Enter");
            Ok(true)
        }
        "tab" => {
            enigo.key(EnigoKey::Tab, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Tab");
            Ok(true)
        }
        "escape" | "cancel" => {
            enigo.key(EnigoKey::Escape, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Escape");
            Ok(true)
        }
        "backspace" | "delete that" | "oops" => {
            enigo.key(EnigoKey::Backspace, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Backspace");
            Ok(true)
        }
        "space" => {
            enigo.key(EnigoKey::Space, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Space");
            Ok(true)
        }

        // Editing shortcuts
        "select all" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('a'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Select All");
            Ok(true)
        }
        "copy" | "copy that" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('c'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Copy");
            Ok(true)
        }
        "paste" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('v'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Paste");
            Ok(true)
        }
        "cut" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('x'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Cut");
            Ok(true)
        }
        "undo" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('z'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Undo");
            Ok(true)
        }
        "redo" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Shift, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('z'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Shift, enigo::Direction::Release)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Redo");
            Ok(true)
        }
        "save" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('s'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Save");
            Ok(true)
        }

        // Media controls
        "play" | "pause" | "play pause" | "playpause" => {
            enigo.key(EnigoKey::MediaPlayPause, enigo::Direction::Click)?;
            println!("[SS9K] 🎵 Command: Play/Pause");
            Ok(true)
        }
        "next" | "next track" | "skip" => {
            enigo.key(EnigoKey::MediaNextTrack, enigo::Direction::Click)?;
            println!("[SS9K] 🎵 Command: Next Track");
            Ok(true)
        }
        "previous" | "previous track" | "prev" | "back" => {
            enigo.key(EnigoKey::MediaPrevTrack, enigo::Direction::Click)?;
            println!("[SS9K] 🎵 Command: Previous Track");
            Ok(true)
        }
        "volume up" | "louder" => {
            enigo.key(EnigoKey::VolumeUp, enigo::Direction::Click)?;
            println!("[SS9K] 🔊 Command: Volume Up");
            Ok(true)
        }
        "volume down" | "quieter" | "softer" => {
            enigo.key(EnigoKey::VolumeDown, enigo::Direction::Click)?;
            println!("[SS9K] 🔉 Command: Volume Down");
            Ok(true)
        }
        "mute" | "unmute" | "mute toggle" => {
            enigo.key(EnigoKey::VolumeMute, enigo::Direction::Click)?;
            println!("[SS9K] 🔇 Command: Mute Toggle");
            Ok(true)
        }

        // Check custom commands from config
        _ => {
            // Try to match against custom commands (case-insensitive)
            for (phrase, cmd) in custom_commands {
                if trimmed == phrase.to_lowercase() {
                    execute_custom_command(cmd)?;
                    return Ok(true);
                }
            }

            // Not a command, type it (use aliased version)
            enigo.text(&aliased)?;
            println!("[SS9K] ⌨️ Typed!");
            Ok(false)
        }
    }
}

/// Download a model from HuggingFace with progress bar
fn download_model(url: &str, dest: &PathBuf) -> Result<()> {
    println!("[SS9K] Downloading model from: {}", url);

    // Create parent directories
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    // Start download
    let response = reqwest::blocking::get(url)?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", response.status());
    }

    let total_size = response.content_length().unwrap_or(0);

    // Setup progress bar
    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[SS9K] {bar:40.cyan/blue} {bytes}/{total_bytes} ({eta})")?
            .progress_chars("##-"),
    );

    // Write to file
    let mut file = File::create(dest)?;
    let mut downloaded: u64 = 0;
    let content = response.bytes()?;

    for chunk in content.chunks(8192) {
        file.write_all(chunk)?;
        downloaded += chunk.len() as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("Download complete!");
    println!("[SS9K] Model saved to: {:?}", dest);

    Ok(())
}

/// Get the preferred model install location
fn get_model_install_path(model_name: &str) -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ss9k")
        .join("models")
        .join(model_name)
}

/// Get the model path, checking multiple locations
fn get_model_path(model_name: &str) -> PathBuf {
    let candidates = [
        // 1. Current directory (for development)
        PathBuf::from("models").join(model_name),
        // 2. XDG data dir (Linux: ~/.local/share/ss9k)
        dirs::data_dir()
            .map(|p| p.join("ss9k").join("models").join(model_name))
            .unwrap_or_default(),
        // 3. Home dir fallback
        dirs::home_dir()
            .map(|p| p.join(".ss9k").join("models").join(model_name))
            .unwrap_or_default(),
    ];

    for path in candidates {
        if path.exists() {
            return path;
        }
    }

    // Return the preferred install location if none exist (for error message)
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ss9k")
        .join("models")
        .join(model_name)
}

/// Check if a device name looks like a microphone (platform-specific)
#[cfg(target_os = "linux")]
fn is_microphone(name: &str) -> bool {
    // ALSA device naming: look for "Microphone" and "CARD"
    name.contains("Microphone") && name.contains("CARD")
}

#[cfg(target_os = "windows")]
fn is_microphone(name: &str) -> bool {
    // Windows: case-insensitive "microphone"
    name.to_lowercase().contains("microphone")
}

#[cfg(target_os = "macos")]
fn is_microphone(name: &str) -> bool {
    // macOS: CoreAudio usually has sensible defaults, but check for common names
    let lower = name.to_lowercase();
    lower.contains("microphone") || lower.contains("input") || lower.contains("mic")
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn is_microphone(_name: &str) -> bool {
    // Unknown platform: accept anything, fall back to default
    true
}

fn main() -> Result<()> {
    // Load configuration first so we can show the right hotkey
    let config = Config::load();
    println!("[SS9K] Model: {}, Language: {}, Threads: {}",
             config.model, config.language, config.threads);

    // Parse hotkey
    let hotkey = parse_hotkey(&config.hotkey).unwrap_or_else(|| {
        eprintln!("[SS9K] Unknown hotkey '{}', defaulting to F12", config.hotkey);
        RdevKey::F12
    });

    println!("=================================");
    println!("   SuperScreecher9000 v0.5.0");
    println!("   Press {} to screech", config.hotkey);
    println!("=================================");
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
    let config = Arc::new(config);
    println!("[SS9K] Model loaded!");

    let host = cpal::default_host();
    println!("[SS9K] Host: {:?}", host.id());

    // Find microphone device (config override or platform-specific detection)
    let device = if !config.device.is_empty() {
        // User specified a device in config
        host.input_devices()?
            .find(|d| d.name().map(|n| n.contains(&config.device)).unwrap_or(false))
            .or_else(|| {
                eprintln!("[SS9K] Configured device '{}' not found, using default", config.device);
                host.default_input_device()
            })
    } else {
        // Auto-detect using platform-specific logic
        host.input_devices()?
            .find(|d| d.name().map(|n| is_microphone(&n)).unwrap_or(false))
            .or_else(|| host.default_input_device())
    }.expect("No input device available");
    println!("[SS9K] Device: {}", device.name()?);

    // Get audio device config
    let audio_config = device.default_input_config()?;
    println!("[SS9K] Audio config: {:?}", audio_config);

    let sample_rate = audio_config.sample_rate().0;
    let channels = audio_config.channels() as usize;

    // Buffer for audio
    let audio_buffer: AudioBuffer = Arc::new(Mutex::new(Vec::new()));
    let buffer_clone = audio_buffer.clone();

    // Error callback
    let err_fn = |err| eprintln!("[SS9K] Stream error: {}", err);

    // Build stream with explicit sample format handling
    let stream = match audio_config.sample_format() {
        cpal::SampleFormat::I8 => build_stream::<i8>(&device, &audio_config.into(), buffer_clone, channels, err_fn)?,
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &audio_config.into(), buffer_clone, channels, err_fn)?,
        cpal::SampleFormat::I32 => build_stream::<i32>(&device, &audio_config.into(), buffer_clone, channels, err_fn)?,
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &audio_config.into(), buffer_clone, channels, err_fn)?,
        format => {
            eprintln!("[SS9K] Unsupported sample format: {:?}", format);
            return Ok(());
        }
    };

    stream.play()?;
    println!("[SS9K] Stream playing. Waiting for F12...");

    // Create async processing queue
    let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();

    // Spawn dedicated processor thread (processes jobs in order, never blocks main)
    {
        let ctx = ctx.clone();
        let config = config.clone();
        std::thread::spawn(move || {
            println!("[SS9K] 🔧 Processor thread started");
            for audio_data in audio_rx {
                println!("[SS9K] 🔄 Processing {} samples...", audio_data.len());

                // Resample
                match resample_audio(&audio_data, sample_rate, WHISPER_SAMPLE_RATE) {
                    Ok(resampled) => {
                        println!("[SS9K] 🔄 Resampled to {} samples at 16kHz", resampled.len());

                        // Transcribe
                        match transcribe(&ctx, &resampled, &config) {
                            Ok(text) => {
                                println!("[SS9K] 📝 Transcription: {}", text);
                                if !text.is_empty() {
                                    // Execute command or type at cursor
                                    match Enigo::new(&Settings::default()) {
                                        Ok(mut enigo) => {
                                            if let Err(e) = execute_command(&mut enigo, &text, &config.commands, &config.aliases) {
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

    // Keyboard callback
    let buffer_for_kb = audio_buffer.clone();
    let is_toggle_mode = config.hotkey_mode == "toggle";
    let toggle_timeout = config.toggle_timeout_secs;

    // Helper closure to extract audio and send to processor (non-blocking)
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

    // Clone for timeout thread
    let send_audio_for_timeout = send_audio.clone();

    let hotkey_name = config.hotkey.clone();
    let callback = move |event: Event| {
        match event.event_type {
            EventType::KeyPress(key) if key == hotkey => {
                if is_toggle_mode {
                    // Toggle mode: press toggles recording on/off
                    if RECORDING.load(Ordering::SeqCst) {
                        // Was recording, stop and queue for processing
                        RECORDING.store(false, Ordering::SeqCst);
                        send_audio();
                    } else {
                        // Not recording, start
                        if let Ok(mut buf) = buffer_for_kb.lock() {
                            buf.clear();
                        }
                        CALLBACK_COUNT.store(0, Ordering::SeqCst);

                        // Increment session ID to invalidate any pending timeout threads
                        let session_id = RECORDING_SESSION.fetch_add(1, Ordering::SeqCst) + 1;
                        RECORDING.store(true, Ordering::SeqCst);

                        if toggle_timeout > 0 {
                            println!("[SS9K] 🎙️ Recording... ({} to stop, or {}s timeout)", hotkey_name, toggle_timeout);

                            // Spawn timeout thread
                            let send_audio_timeout = send_audio_for_timeout.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_secs(toggle_timeout));

                                // Only stop if this is still the same recording session
                                if RECORDING_SESSION.load(Ordering::SeqCst) == session_id
                                   && RECORDING.load(Ordering::SeqCst) {
                                    println!("[SS9K] ⏱️ Timeout reached!");
                                    RECORDING.store(false, Ordering::SeqCst);
                                    send_audio_timeout();
                                }
                            });
                        } else {
                            println!("[SS9K] 🎙️ Recording... (press {} again to stop)", hotkey_name);
                        }
                    }
                } else {
                    // Hold mode: press starts recording
                    if !RECORDING.load(Ordering::SeqCst) {
                        if let Ok(mut buf) = buffer_for_kb.lock() {
                            buf.clear();
                        }
                        CALLBACK_COUNT.store(0, Ordering::SeqCst);
                        RECORDING.store(true, Ordering::SeqCst);
                        println!("[SS9K] 🎙️ Recording...");
                    }
                }
            }
            EventType::KeyRelease(key) if key == hotkey => {
                if !is_toggle_mode {
                    // Hold mode: release stops recording and queues for processing
                    if RECORDING.load(Ordering::SeqCst) {
                        RECORDING.store(false, Ordering::SeqCst);
                        send_audio();
                    }
                }
                // Toggle mode: release does nothing
            }
            _ => {}
        }
    };

    listen(callback).map_err(|e| anyhow::anyhow!("Listen error: {:?}", e))?;
    Ok(())
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buffer: AudioBuffer,
    channels: usize,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);

            if RECORDING.load(Ordering::SeqCst) {
                if let Ok(mut buf) = buffer.lock() {
                    for chunk in data.chunks(channels) {
                        // Convert to mono f32
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

fn resample_audio(input: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
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
        2.0, // max relative ratio change
        params,
        input.len(),
        1, // mono
    )?;

    let waves_in = vec![input.to_vec()];
    let waves_out = resampler.process(&waves_in, None)?;

    Ok(waves_out.into_iter().next().unwrap_or_default())
}

fn transcribe(ctx: &WhisperContext, audio: &[f32], config: &Config) -> Result<String> {
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
