use anyhow::Result;
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Sample;
use enigo::{Enigo, Key as EnigoKey, Keyboard, Settings};
use indicatif::{ProgressBar, ProgressStyle};
use notify::{recommended_watcher, RecursiveMode, Watcher};
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
// Last executed command for "repeat" functionality
static LAST_COMMAND: std::sync::LazyLock<Mutex<Option<String>>> = std::sync::LazyLock::new(|| Mutex::new(None));
// Currently held keys for "hold/release" functionality
static HELD_KEYS: std::sync::LazyLock<Mutex<Vec<EnigoKey>>> = std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

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
    /// Suppress verbose output (transcriptions, command logs)
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
            commands: HashMap::new(),
            aliases: HashMap::new(),
            quiet: false,
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
    /// Load config from file, or return defaults. Also returns the path loaded from (if any).
    pub fn load() -> (Self, Option<PathBuf>) {
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

    /// Reload config from a specific path
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

/// Normalize text for fuzzy command matching
/// Collapses spaces and normalizes number words to digits
fn normalize_for_matching(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .map(|word| {
            // Convert number words to digits for consistent matching
            match word {
                "zero" => "0",
                "one" => "1",
                "two" | "to" | "too" => "2",
                "three" => "3",
                "four" | "for" => "4",
                "five" => "5",
                "six" => "6",
                "seven" => "7",
                "eight" => "8",
                "nine" => "9",
                "ten" => "10",
                _ => word,
            }
        })
        .collect::<Vec<_>>()
        .join("") // collapse all spaces
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
/// Uses leader words: "command X" for commands, "punctuation X" for symbols
/// Returns true if a command was executed, false if text was typed
fn execute_command(enigo: &mut Enigo, text: &str, custom_commands: &HashMap<String, String>, aliases: &HashMap<String, String>) -> Result<bool> {
    // First normalize using aliases
    let aliased = normalize_aliases(text, aliases);

    // Strip punctuation for parsing
    let trimmed: String = aliased
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase();

    // Check for "command X" leader
    if let Some(cmd) = trimmed.strip_prefix("command ") {
        return execute_builtin_command(enigo, cmd.trim());
    }

    // Check for "punctuation X" or "punk X" leader (safe - distinctive words)
    if let Some(punct) = trimmed.strip_prefix("punctuation ").or_else(|| trimmed.strip_prefix("punk ")) {
        return execute_punctuation(enigo, punct.trim());
    }

    // Check for "emoji X" leader (safe - distinctive word)
    if let Some(emoji_name) = trimmed.strip_prefix("emoji ") {
        return execute_emoji(enigo, emoji_name.trim());
    }

    // Check custom commands (user-defined phrases don't need leader)
    // Use fuzzy matching: normalize spaces and number words
    let normalized_input = normalize_for_matching(&trimmed);
    for (phrase, cmd) in custom_commands {
        if normalized_input == normalize_for_matching(phrase) {
            execute_custom_command(cmd)?;
            return Ok(true);
        }
    }

    // Not a command, type it (use aliased version)
    enigo.text(&aliased)?;
    println!("[SS9K] ⌨️ Typed!");
    Ok(false)
}

/// Execute a built-in command (navigation, editing, media)
/// Handles "times N" suffix and "repeat" command
fn execute_builtin_command(enigo: &mut Enigo, cmd: &str) -> Result<bool> {
    // Parse "times N" suffix (e.g., "backspace times 5")
    let (base_cmd, count) = parse_times_suffix(cmd);

    // Handle "repeat" command
    if base_cmd == "repeat" || base_cmd.starts_with("repeat ") {
        let repeat_count = if base_cmd == "repeat" {
            count.max(1) // "repeat" alone = 1, "repeat times 3" = 3
        } else {
            // "repeat 5" or "repeat five" - parse number word
            base_cmd.strip_prefix("repeat ")
                .and_then(|s| s.split_whitespace().next())
                .and_then(parse_number_word)
                .unwrap_or(1)
                .max(1) * count.max(1)
        };

        let last_cmd = LAST_COMMAND.lock().ok().and_then(|g| g.clone());
        if let Some(ref cmd_to_repeat) = last_cmd {
            println!("[SS9K] 🔁 Repeating '{}' {} time(s)", cmd_to_repeat, repeat_count);
            for _ in 0..repeat_count {
                execute_single_builtin_command(enigo, cmd_to_repeat)?;
            }
            return Ok(true);
        } else {
            eprintln!("[SS9K] ⚠️ Nothing to repeat");
            return Ok(false);
        }
    }

    // Handle "shift X" subcommand (selection/shift-modified keys)
    if let Some(shift_cmd) = base_cmd.strip_prefix("shift ") {
        return execute_shift(enigo, shift_cmd.trim());
    }

    // Handle "spell X Y Z" subcommand
    if let Some(spell_input) = base_cmd.strip_prefix("spell ") {
        return execute_spell_mode(enigo, spell_input.trim());
    }

    // Handle "hold X" subcommand
    if let Some(hold_key) = base_cmd.strip_prefix("hold ") {
        return execute_hold(enigo, hold_key.trim());
    }

    // Handle "release X" or "release all" subcommand
    if base_cmd == "release all" || base_cmd == "release" {
        return execute_release_all(enigo);
    }
    if let Some(release_key) = base_cmd.strip_prefix("release ") {
        return execute_release(enigo, release_key.trim());
    }

    // Execute the command count times
    for i in 0..count.max(1) {
        if !execute_single_builtin_command(enigo, base_cmd)? {
            return Ok(false);
        }
        if count > 1 && i < count - 1 {
            // Small delay between repeated commands for reliability
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    // Store as last command (for repeat)
    if let Ok(mut last) = LAST_COMMAND.lock() {
        *last = Some(base_cmd.to_string());
    }

    if count > 1 {
        println!("[SS9K] 🔁 Executed {} times", count);
    }

    Ok(true)
}

/// Parse a number from digit or word form
fn parse_number_word(s: &str) -> Option<usize> {
    // Try digit first
    if let Ok(n) = s.parse::<usize>() {
        return Some(n);
    }
    // Try word form
    match s {
        "zero" => Some(0),
        "one" => Some(1),
        "two" | "to" | "too" => Some(2), // common mishearings
        "three" => Some(3),
        "four" | "for" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "thirteen" => Some(13),
        "fourteen" => Some(14),
        "fifteen" => Some(15),
        "sixteen" => Some(16),
        "seventeen" => Some(17),
        "eighteen" => Some(18),
        "nineteen" => Some(19),
        "twenty" => Some(20),
        _ => None,
    }
}

/// Parse "times N" suffix from a command
/// Returns (base_command, count) where count is 0 if no suffix found
fn parse_times_suffix(cmd: &str) -> (&str, usize) {
    // Check for "times N" at the end (e.g., "backspace times 5" or "backspace times five")
    if let Some(idx) = cmd.rfind(" times ") {
        let after = &cmd[idx + 7..].trim();
        if let Some(n) = parse_number_word(after) {
            return (&cmd[..idx], n);
        }
    }
    // Check for "X times" pattern (e.g., "backspace 5 times" or "backspace five times")
    let words: Vec<&str> = cmd.split_whitespace().collect();
    if words.len() >= 2 && words[words.len() - 1] == "times" {
        if let Some(n) = parse_number_word(words[words.len() - 2]) {
            let end_idx = cmd.rfind(words[words.len() - 2]).unwrap_or(cmd.len());
            return (cmd[..end_idx].trim(), n);
        }
    }
    (cmd, 0)
}

/// Execute a single built-in command once (internal helper)
fn execute_single_builtin_command(enigo: &mut Enigo, cmd: &str) -> Result<bool> {
    match cmd {
        // Navigation
        "enter" | "new line" | "newline" | "return" => {
            enigo.key(EnigoKey::Return, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Enter");
        }
        "tab" => {
            enigo.key(EnigoKey::Tab, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Tab");
        }
        "escape" | "cancel" => {
            enigo.key(EnigoKey::Escape, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Escape");
        }
        "backspace" | "delete" | "delete that" | "oops" => {
            enigo.key(EnigoKey::Backspace, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Backspace");
        }
        "space" => {
            enigo.key(EnigoKey::Space, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Space");
        }
        "up" | "arrow up" => {
            enigo.key(EnigoKey::UpArrow, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Up");
        }
        "down" | "arrow down" => {
            enigo.key(EnigoKey::DownArrow, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Down");
        }
        "left" | "arrow left" => {
            enigo.key(EnigoKey::LeftArrow, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Left");
        }
        "right" | "arrow right" => {
            enigo.key(EnigoKey::RightArrow, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Right");
        }
        "home" => {
            enigo.key(EnigoKey::Home, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Home");
        }
        "end" => {
            enigo.key(EnigoKey::End, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: End");
        }
        "page up" => {
            enigo.key(EnigoKey::PageUp, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Page Up");
        }
        "page down" => {
            enigo.key(EnigoKey::PageDown, enigo::Direction::Click)?;
            println!("[SS9K] ⌨️ Command: Page Down");
        }

        // Editing shortcuts
        "select all" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('a'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Select All");
        }
        "copy" | "copy that" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('c'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Copy");
        }
        "paste" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('v'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Paste");
        }
        "cut" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('x'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Cut");
        }
        "undo" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('z'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Undo");
        }
        "redo" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Shift, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('z'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Shift, enigo::Direction::Release)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Redo");
        }
        "save" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('s'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Save");
        }
        "find" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('f'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Find");
        }
        "close" | "close tab" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('w'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: Close");
        }
        "new tab" => {
            enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
            enigo.key(EnigoKey::Unicode('t'), enigo::Direction::Click)?;
            enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
            println!("[SS9K] ⌨️ Command: New Tab");
        }

        // Media controls
        "play" | "pause" | "play pause" | "playpause" => {
            enigo.key(EnigoKey::MediaPlayPause, enigo::Direction::Click)?;
            println!("[SS9K] 🎵 Command: Play/Pause");
        }
        "next" | "next track" | "skip" => {
            enigo.key(EnigoKey::MediaNextTrack, enigo::Direction::Click)?;
            println!("[SS9K] 🎵 Command: Next Track");
        }
        "previous" | "previous track" | "prev" | "back" => {
            enigo.key(EnigoKey::MediaPrevTrack, enigo::Direction::Click)?;
            println!("[SS9K] 🎵 Command: Previous Track");
        }
        "volume up" | "louder" => {
            enigo.key(EnigoKey::VolumeUp, enigo::Direction::Click)?;
            println!("[SS9K] 🔊 Command: Volume Up");
        }
        "volume down" | "quieter" | "softer" => {
            enigo.key(EnigoKey::VolumeDown, enigo::Direction::Click)?;
            println!("[SS9K] 🔉 Command: Volume Down");
        }
        "mute" | "unmute" | "mute toggle" => {
            enigo.key(EnigoKey::VolumeMute, enigo::Direction::Click)?;
            println!("[SS9K] 🔇 Command: Mute Toggle");
        }

        // Help & Config
        "help" => {
            print_help();
        }
        "config" | "settings" | "edit config" => {
            let config_path = dirs::config_dir()
                .map(|p| p.join("ss9k").join("config.toml"))
                .unwrap_or_else(|| PathBuf::from("~/.config/ss9k/config.toml"));

            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "xdg-open".to_string());
            println!("[SS9K] 📝 Opening config: {:?}", config_path);

            if let Err(e) = std::process::Command::new(&editor)
                .arg(&config_path)
                .spawn()
            {
                eprintln!("[SS9K] ⚠️ Failed to open config: {}", e);
                println!("[SS9K] Config path: {:?}", config_path);
            }
        }

        _ => {
            eprintln!("[SS9K] ⚠️ Unknown command: {}", cmd);
            return Ok(false);
        }
    }
    Ok(true)
}

/// Execute punctuation insertion
/// Includes common Whisper mishearings for robustness
fn execute_punctuation(enigo: &mut Enigo, punct: &str) -> Result<bool> {
    let symbol = match punct {
        // Basic punctuation
        "period" | "dot" | "full stop" | "point" => ".",
        "comma" | "coma" => ",",
        "question" | "question mark" => "?",
        "exclamation" | "exclamation mark" | "bang" | "exclamation point" => "!",
        "colon" | "colin" | "cologne" => ":",
        "semicolon" | "semi colon" | "semi colin" | "semicolin" => ";",
        "ellipsis" | "ellipses" | "dot dot dot" => "...",

        // Quotes
        "quote" | "double quote" | "quotes" | "quotation" => "\"",
        "single quote" | "apostrophe" | "apostrophy" => "'",
        "backtick" | "grave" | "back tick" | "back tic" | "backtic" => "`",

        // Brackets
        "open paren" | "left paren" | "open parenthesis" | "open parentheses" => "(",
        "close paren" | "right paren" | "close parenthesis" | "close parentheses" => ")",
        "open bracket" | "left bracket" | "open square" => "[",
        "close bracket" | "right bracket" | "close square" => "]",
        "open brace" | "left brace" | "open curly" | "open curley" => "{",
        "close brace" | "right brace" | "close curly" | "close curley" => "}",
        "less than" | "open angle" | "left angle" | "left chevron" => "<",
        "greater than" | "close angle" | "right angle" | "right chevron" => ">",

        // Math/symbols
        "plus" | "positive" => "+",
        "minus" | "dash" | "hyphen" | "negative" => "-",
        "equals" | "equal" | "equal sign" | "equals sign" => "=",
        "underscore" | "under score" | "underline" => "_",
        "asterisk" | "star" | "asterix" | "astrix" | "asterisks" => "*",
        "slash" | "forward slash" | "forwardslash" => "/",
        "backslash" | "back slash" | "backward slash" => "\\",
        "pipe" | "bar" | "vertical bar" | "vertical line" => "|",
        "caret" | "carrot" | "karet" | "carret" | "hat" => "^",
        "tilde" | "tilda" | "tildy" | "squiggle" => "~",
        "percent" | "percentage" | "per cent" => "%",
        "ampersand" | "and sign" | "and symbol" => "&",
        "at" | "at sign" | "at symbol" => "@",
        "hash" | "hashtag" | "pound" | "number sign" | "hash tag" | "octothorpe" => "#",
        "dollar" | "dollar sign" | "dollars" => "$",

        // Programming
        "arrow" | "fat arrow" | "thick arrow" | "rocket" => "=>",
        "thin arrow" | "skinny arrow" | "dash arrow" | "hyphen arrow" => "->",
        "double colon" | "scope" | "colon colon" | "colin colin" => "::",
        "double equals" | "equals equals" | "equal equal" => "==",
        "not equals" | "not equal" | "bang equals" | "exclamation equals" => "!=",
        "less than or equal" | "less equal" | "less or equal" => "<=",
        "greater than or equal" | "greater equal" | "greater or equal" => ">=",
        "plus equals" | "plus equal" => "+=",
        "minus equals" | "minus equal" | "dash equals" => "-=",
        "and and" | "double and" | "ampersand ampersand" => "&&",
        "or or" | "double or" | "pipe pipe" | "double pipe" => "||",

        _ => {
            eprintln!("[SS9K] ⚠️ Unknown punctuation: {}", punct);
            return Ok(false);
        }
    };

    enigo.text(symbol)?;
    println!("[SS9K] ✏️ Punctuation: {}", symbol);
    Ok(true)
}

/// Execute emoji insertion
fn execute_emoji(enigo: &mut Enigo, name: &str) -> Result<bool> {
    let emoji = match name {
        // Faces
        "smile" | "happy" => "😊",
        "laugh" | "lol" | "laughing" => "😂",
        "joy" => "🤣",
        "wink" => "😉",
        "love" | "heart eyes" => "😍",
        "cool" | "sunglasses" => "😎",
        "think" | "thinking" | "hmm" => "🤔",
        "cry" | "sad" | "crying" => "😭",
        "angry" | "mad" => "😠",
        "skull" | "dead" => "💀",
        "eye roll" | "roll eyes" => "🙄",
        "shush" | "quiet" => "🤫",
        "mind blown" | "exploding head" => "🤯",
        "clown" => "🤡",
        "nerd" => "🤓",
        "sick" | "ill" => "🤢",
        "scream" => "😱",

        // Gestures
        "thumbs up" | "thumb up" | "yes" => "👍",
        "thumbs down" | "thumb down" | "no" => "👎",
        "clap" | "clapping" => "👏",
        "wave" | "hi" | "bye" => "👋",
        "shrug" => "🤷",
        "facepalm" | "face palm" => "🤦",
        "pray" | "please" | "thanks" => "🙏",
        "muscle" | "strong" | "flex" => "💪",
        "point up" => "☝️",
        "point right" => "👉",
        "point left" => "👈",
        "point down" => "👇",
        "ok" | "okay" => "👌",
        "peace" | "victory" => "✌️",
        "rock" | "metal" => "🤘",
        "middle finger" | "fuck you" => "🖕",

        // Hearts & love
        "heart" | "red heart" => "❤️",
        "blue heart" => "💙",
        "green heart" => "💚",
        "yellow heart" => "💛",
        "purple heart" => "💜",
        "black heart" => "🖤",
        "white heart" => "🤍",
        "orange heart" => "🧡",
        "broken heart" => "💔",
        "sparkling heart" => "💖",
        "kiss" => "😘",

        // Animals
        "dog" | "wag" => "🐕",
        "cat" => "🐈",
        "crab" | "rust" => "🦀",
        "snake" => "🐍",
        "bug" | "beetle" => "🐛",
        "butterfly" => "🦋",
        "unicorn" => "🦄",
        "dragon" => "🐉",
        "shark" => "🦈",
        "whale" => "🐋",
        "octopus" => "🐙",

        // Objects & symbols
        "fire" | "lit" => "🔥",
        "star" | "gold star" => "⭐",
        "sparkles" | "sparkle" => "✨",
        "lightning" | "zap" => "⚡",
        "poop" | "shit" => "💩",
        "100" | "hundred" => "💯",
        "check" | "checkmark" => "✅",
        "x" | "cross" => "❌",
        "warning" => "⚠️",
        "question" => "❓",
        "exclamation" => "❗",
        "pin" | "pushpin" => "📌",
        "bulb" | "idea" | "lightbulb" => "💡",
        "gear" | "settings" => "⚙️",
        "rocket" => "🚀",
        "trophy" => "🏆",
        "medal" => "🏅",
        "crown" => "👑",
        "money" | "cash" => "💰",
        "gem" | "diamond" => "💎",
        "gift" | "present" => "🎁",
        "party" | "celebrate" => "🎉",
        "balloon" => "🎈",
        "beer" | "cheers" => "🍺",
        "coffee" => "☕",
        "pizza" => "🍕",
        "taco" => "🌮",

        _ => {
            eprintln!("[SS9K] ⚠️ Unknown emoji: {}", name);
            return Ok(false);
        }
    };

    enigo.text(emoji)?;
    println!("[SS9K] 😀 Emoji: {}", emoji);
    Ok(true)
}

/// Execute shift-modified commands (for selections and shift+key combos)
/// Supports "times N" suffix for repetition
fn execute_shift(enigo: &mut Enigo, cmd: &str) -> Result<bool> {
    // Parse "times N" suffix
    let (base_cmd, count) = parse_times_suffix(cmd);
    let times = count.max(1);

    // Hold shift for the duration
    enigo.key(EnigoKey::Shift, enigo::Direction::Press)?;

    for i in 0..times {
        let result = match base_cmd {
            // Arrow keys (selection)
            "left" => enigo.key(EnigoKey::LeftArrow, enigo::Direction::Click),
            "right" => enigo.key(EnigoKey::RightArrow, enigo::Direction::Click),
            "up" => enigo.key(EnigoKey::UpArrow, enigo::Direction::Click),
            "down" => enigo.key(EnigoKey::DownArrow, enigo::Direction::Click),

            // Word selection (Ctrl+Shift+Arrow)
            "word left" => {
                enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
                let r = enigo.key(EnigoKey::LeftArrow, enigo::Direction::Click);
                enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
                r
            }
            "word right" => {
                enigo.key(EnigoKey::Control, enigo::Direction::Press)?;
                let r = enigo.key(EnigoKey::RightArrow, enigo::Direction::Click);
                enigo.key(EnigoKey::Control, enigo::Direction::Release)?;
                r
            }

            // Line selection
            "home" => enigo.key(EnigoKey::Home, enigo::Direction::Click),
            "end" => enigo.key(EnigoKey::End, enigo::Direction::Click),

            // Page selection
            "page up" => enigo.key(EnigoKey::PageUp, enigo::Direction::Click),
            "page down" => enigo.key(EnigoKey::PageDown, enigo::Direction::Click),

            // Common shift combos
            "tab" => enigo.key(EnigoKey::Tab, enigo::Direction::Click),
            "enter" | "return" => enigo.key(EnigoKey::Return, enigo::Direction::Click),

            _ => {
                enigo.key(EnigoKey::Shift, enigo::Direction::Release)?;
                eprintln!("[SS9K] ⚠️ Unknown shift command: {}", base_cmd);
                return Ok(false);
            }
        };

        if result.is_err() {
            enigo.key(EnigoKey::Shift, enigo::Direction::Release)?;
            return Err(result.unwrap_err().into());
        }

        if times > 1 && i < times - 1 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    enigo.key(EnigoKey::Shift, enigo::Direction::Release)?;

    if times > 1 {
        println!("[SS9K] ⇧ Shift+{} × {}", base_cmd, times);
    } else {
        println!("[SS9K] ⇧ Shift+{}", base_cmd);
    }

    Ok(true)
}

/// Execute spell mode - spell out letters using NATO phonetic, raw letters, or numbers
/// Examples: "spell alpha bravo charlie" → "abc"
///           "spell capital alpha bravo" → "Ab"
///           "spell one two three" → "123"
fn execute_spell_mode(enigo: &mut Enigo, input: &str) -> Result<bool> {
    let words: Vec<&str> = input.split_whitespace().collect();
    let mut result = String::new();
    let mut next_capital = false;

    for word in words {
        // Check for capital modifier
        if word == "capital" || word == "cap" || word == "uppercase" || word == "upper" {
            next_capital = true;
            continue;
        }

        // Try to map word to character
        if let Some(ch) = word_to_char(word) {
            if next_capital {
                result.push(ch.to_ascii_uppercase());
                next_capital = false;
            } else {
                result.push(ch);
            }
        } else {
            eprintln!("[SS9K] ⚠️ Unknown spell word: {}", word);
        }
    }

    if result.is_empty() {
        eprintln!("[SS9K] ⚠️ Spell mode produced no characters");
        return Ok(false);
    }

    enigo.text(&result)?;
    println!("[SS9K] 🔤 Spelled: {}", result);
    Ok(true)
}

/// Parse a key name to an EnigoKey (for hold/release functionality)
fn parse_key_name(name: &str) -> Option<EnigoKey> {
    match name.to_lowercase().as_str() {
        // Letters (common for gaming: WASD)
        "a" => Some(EnigoKey::Unicode('a')),
        "b" => Some(EnigoKey::Unicode('b')),
        "c" => Some(EnigoKey::Unicode('c')),
        "d" => Some(EnigoKey::Unicode('d')),
        "e" => Some(EnigoKey::Unicode('e')),
        "f" => Some(EnigoKey::Unicode('f')),
        "g" => Some(EnigoKey::Unicode('g')),
        "h" => Some(EnigoKey::Unicode('h')),
        "i" => Some(EnigoKey::Unicode('i')),
        "j" => Some(EnigoKey::Unicode('j')),
        "k" => Some(EnigoKey::Unicode('k')),
        "l" => Some(EnigoKey::Unicode('l')),
        "m" => Some(EnigoKey::Unicode('m')),
        "n" => Some(EnigoKey::Unicode('n')),
        "o" => Some(EnigoKey::Unicode('o')),
        "p" => Some(EnigoKey::Unicode('p')),
        "q" => Some(EnigoKey::Unicode('q')),
        "r" => Some(EnigoKey::Unicode('r')),
        "s" => Some(EnigoKey::Unicode('s')),
        "t" => Some(EnigoKey::Unicode('t')),
        "u" => Some(EnigoKey::Unicode('u')),
        "v" => Some(EnigoKey::Unicode('v')),
        "w" => Some(EnigoKey::Unicode('w')),
        "x" => Some(EnigoKey::Unicode('x')),
        "y" => Some(EnigoKey::Unicode('y')),
        "z" => Some(EnigoKey::Unicode('z')),

        // Modifiers
        "shift" => Some(EnigoKey::Shift),
        "control" | "ctrl" => Some(EnigoKey::Control),
        "alt" => Some(EnigoKey::Alt),
        "meta" | "super" | "windows" | "win" => Some(EnigoKey::Meta),

        // Navigation
        "up" | "arrow up" => Some(EnigoKey::UpArrow),
        "down" | "arrow down" => Some(EnigoKey::DownArrow),
        "left" | "arrow left" => Some(EnigoKey::LeftArrow),
        "right" | "arrow right" => Some(EnigoKey::RightArrow),

        // Common keys
        "space" => Some(EnigoKey::Space),
        "enter" | "return" => Some(EnigoKey::Return),
        "tab" => Some(EnigoKey::Tab),
        "escape" | "esc" => Some(EnigoKey::Escape),
        "backspace" => Some(EnigoKey::Backspace),

        _ => None,
    }
}

/// Hold a key down (add to held keys list)
fn execute_hold(enigo: &mut Enigo, key_name: &str) -> Result<bool> {
    let key = match parse_key_name(key_name) {
        Some(k) => k,
        None => {
            eprintln!("[SS9K] ⚠️ Unknown key to hold: {}", key_name);
            return Ok(false);
        }
    };

    // Press the key (hold it down)
    enigo.key(key.clone(), enigo::Direction::Press)?;

    // Add to held keys list
    if let Ok(mut held) = HELD_KEYS.lock() {
        // Avoid duplicates
        if !held.iter().any(|k| std::mem::discriminant(k) == std::mem::discriminant(&key)) {
            held.push(key.clone());
        }
    }

    println!("[SS9K] 🔒 Holding: {}", key_name);
    Ok(true)
}

/// Release a specific held key
fn execute_release(enigo: &mut Enigo, key_name: &str) -> Result<bool> {
    let key = match parse_key_name(key_name) {
        Some(k) => k,
        None => {
            eprintln!("[SS9K] ⚠️ Unknown key to release: {}", key_name);
            return Ok(false);
        }
    };

    // Release the key
    enigo.key(key.clone(), enigo::Direction::Release)?;

    // Remove from held keys list
    if let Ok(mut held) = HELD_KEYS.lock() {
        held.retain(|k| std::mem::discriminant(k) != std::mem::discriminant(&key));
    }

    println!("[SS9K] 🔓 Released: {}", key_name);
    Ok(true)
}

/// Release all held keys
fn execute_release_all(enigo: &mut Enigo) -> Result<bool> {
    let keys_to_release = if let Ok(mut held) = HELD_KEYS.lock() {
        let keys = held.clone();
        held.clear();
        keys
    } else {
        Vec::new()
    };

    if keys_to_release.is_empty() {
        println!("[SS9K] 🔓 No keys held");
        return Ok(true);
    }

    for key in &keys_to_release {
        enigo.key(key.clone(), enigo::Direction::Release)?;
    }

    println!("[SS9K] 🔓 Released {} key(s)", keys_to_release.len());
    Ok(true)
}

/// Map a word to a single character (NATO, raw letter, number word, or raw digit)
fn word_to_char(word: &str) -> Option<char> {
    // NATO phonetic alphabet
    let nato = match word {
        "alpha" | "alfa" => Some('a'),
        "bravo" => Some('b'),
        "charlie" => Some('c'),
        "delta" => Some('d'),
        "echo" => Some('e'),
        "foxtrot" => Some('f'),
        "golf" => Some('g'),
        "hotel" => Some('h'),
        "india" => Some('i'),
        "juliet" | "juliett" => Some('j'),
        "kilo" => Some('k'),
        "lima" => Some('l'),
        "mike" => Some('m'),
        "november" => Some('n'),
        "oscar" => Some('o'),
        "papa" => Some('p'),
        "quebec" => Some('q'),
        "romeo" => Some('r'),
        "sierra" => Some('s'),
        "tango" => Some('t'),
        "uniform" => Some('u'),
        "victor" => Some('v'),
        "whiskey" => Some('w'),
        "xray" | "x-ray" => Some('x'),
        "yankee" => Some('y'),
        "zulu" => Some('z'),
        _ => None,
    };
    if nato.is_some() {
        return nato;
    }

    // Number words
    let number = match word {
        "zero" => Some('0'),
        "one" => Some('1'),
        "two" => Some('2'),
        "three" => Some('3'),
        "four" => Some('4'),
        "five" => Some('5'),
        "six" => Some('6'),
        "seven" => Some('7'),
        "eight" => Some('8'),
        "nine" => Some('9'),
        _ => None,
    };
    if number.is_some() {
        return number;
    }

    // Space and punctuation (for spelling emails, URLs, etc.)
    // Includes common Whisper mishearings
    let punct = match word {
        "space" => Some(' '),
        "period" | "dot" | "point" => Some('.'),
        "comma" | "coma" => Some(','),
        "at" | "at sign" => Some('@'),
        "dash" | "hyphen" | "minus" => Some('-'),
        "underscore" | "under score" | "underline" => Some('_'),
        "slash" | "forward slash" => Some('/'),
        "colon" | "colin" | "cologne" => Some(':'),
        "semicolon" | "semi colon" | "semi colin" => Some(';'),
        "hash" | "pound" | "hashtag" | "hash tag" | "octothorpe" => Some('#'),
        "dollar" | "dollars" => Some('$'),
        "percent" | "percentage" => Some('%'),
        "ampersand" | "and" => Some('&'),
        "asterisk" | "star" | "asterix" | "astrix" => Some('*'),
        "plus" | "positive" => Some('+'),
        "equals" | "equal" => Some('='),
        "question" => Some('?'),
        "exclamation" | "bang" => Some('!'),
        "tilde" | "tilda" | "tildy" | "squiggle" => Some('~'),
        "caret" | "carrot" | "karet" | "carret" | "hat" => Some('^'),
        "pipe" | "bar" | "vertical" => Some('|'),
        "backslash" | "back slash" => Some('\\'),
        _ => None,
    };
    if punct.is_some() {
        return punct;
    }

    // Raw single letter (a-z)
    if word.len() == 1 {
        let ch = word.chars().next()?;
        if ch.is_ascii_alphabetic() {
            return Some(ch.to_ascii_lowercase());
        }
        if ch.is_ascii_digit() {
            return Some(ch);
        }
    }

    None
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

/// Print the help/command reference
fn print_help() {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                    SS9K Voice Commands                       ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ NAVIGATION: enter, tab, escape, backspace, space             ║");
    println!("║             up, down, left, right, home, end, page up/down   ║");
    println!("║ EDITING:    select all, copy, paste, cut, undo, redo, save   ║");
    println!("║             find, close tab, new tab                         ║");
    println!("║ MEDIA:      play, pause, next, previous, volume up/down, mute║");
    println!("║ REPETITION: [cmd] times [N], repeat, repeat [N]              ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ SUBCOMMANDS (under 'command'):                               ║");
    println!("║   shift [X]  - select text (shift+arrow, shift+word, etc.)   ║");
    println!("║   spell [X]  - NATO spelling (alpha bravo = ab)              ║");
    println!("║   hold [X]   - hold a key (for gaming, accessibility)        ║");
    println!("║   release [X]/release all - release held keys                ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ LEADERS:    'command [X]'     - execute command              ║");
    println!("║             'punctuation [X]' - insert symbol (or 'punk')    ║");
    println!("║             'emoji [X]'       - insert emoji                 ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ CONFIG:     ~/.config/ss9k/config.toml                       ║");
    println!("║ DOCS:       https://github.com/sqrew/ss9k                    ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}

fn main() -> Result<()> {
    // Load configuration first so we can show the right hotkey
    let (config, config_path) = Config::load();
    println!("[SS9K] Model: {}, Language: {}, Threads: {}",
             config.model, config.language, config.threads);

    // Validate hotkey at startup (actual hotkey is loaded fresh from config on each event)
    if parse_hotkey(&config.hotkey).is_none() {
        eprintln!("[SS9K] Unknown hotkey '{}', will default to F12", config.hotkey);
    }

    println!("=================================");
    println!("   SuperScreecher9000 v0.13.0");
    println!("   Press {} to screech", config.hotkey);
    println!("=================================");

    // Show help on startup for discoverability
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

    // Set up config hot-reload if we have a config file
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
                        // Small delay to let the file finish writing
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

    // Find microphone device (config override or platform-specific detection)
    let cfg = config.load();
    let device = if !cfg.device.is_empty() {
        // User specified a device in config
        let device_name = cfg.device.clone();
        host.input_devices()?
            .find(|d| d.name().map(|n| n.contains(&device_name)).unwrap_or(false))
            .or_else(|| {
                eprintln!("[SS9K] Configured device '{}' not found, using default", device_name);
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
                // Load current config (hot-reloadable)
                let cfg = config.load();
                let quiet = cfg.quiet;

                if !quiet {
                    println!("[SS9K] 🔄 Processing {} samples...", audio_data.len());
                }

                // Resample
                match resample_audio(&audio_data, sample_rate, WHISPER_SAMPLE_RATE) {
                    Ok(resampled) => {
                        if !quiet {
                            println!("[SS9K] 🔄 Resampled to {} samples at 16kHz", resampled.len());
                        }

                        // Transcribe
                        match transcribe(&ctx, &resampled, &cfg) {
                            Ok(text) => {
                                if !quiet {
                                    println!("[SS9K] 📝 Transcription: {}", text);
                                }
                                if !text.is_empty() {
                                    // Execute command or type at cursor
                                    match Enigo::new(&Settings::default()) {
                                        Ok(mut enigo) => {
                                            if let Err(e) = execute_command(&mut enigo, &text, &cfg.commands, &cfg.aliases) {
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
    let config_for_kb = config.clone();

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
    let config_for_timeout = config_for_kb.clone();

    let callback = move |event: Event| {
        // Load config fresh on each event (hot-reloadable!)
        let cfg = config_for_kb.load();
        let current_hotkey = parse_hotkey(&cfg.hotkey).unwrap_or(RdevKey::F12);
        let is_toggle_mode = cfg.hotkey_mode == "toggle";
        let toggle_timeout = cfg.toggle_timeout_secs;

        match event.event_type {
            EventType::KeyPress(key) if key == current_hotkey => {
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

                        let hotkey_name = cfg.hotkey.clone();
                        if toggle_timeout > 0 {
                            println!("[SS9K] 🎙️ Recording... ({} to stop, or {}s timeout)", hotkey_name, toggle_timeout);

                            // Spawn timeout thread
                            let send_audio_timeout = send_audio_for_timeout.clone();
                            let config_timeout = config_for_timeout.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_secs(toggle_timeout));

                                // Only stop if this is still the same recording session
                                if RECORDING_SESSION.load(Ordering::SeqCst) == session_id
                                   && RECORDING.load(Ordering::SeqCst) {
                                    let cfg = config_timeout.load();
                                    println!("[SS9K] ⏱️ Timeout reached! (was recording with {})", cfg.hotkey);
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
            EventType::KeyRelease(key) if key == current_hotkey => {
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
