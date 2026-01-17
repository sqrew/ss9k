//! Command execution for SS9K
//!
//! This module handles:
//! - Voice command parsing and execution
//! - Built-in commands (navigation, editing, media)
//! - Shift mode (text selection)
//! - Spell mode (NATO phonetic input)
//! - Hold/Release (key holding for gaming/accessibility)
//! - Custom shell command execution

use anyhow::Result;
use enigo::{Enigo, Key as EnigoKey, Keyboard};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::lookups::{execute_emoji, execute_punctuation, parse_key_name, word_to_char};

// Statics for command state
pub static LAST_COMMAND: std::sync::LazyLock<Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));
pub static HELD_KEYS: std::sync::LazyLock<Mutex<Vec<EnigoKey>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Normalize text by applying aliases (e.g., "e max" -> "emacs")
pub fn normalize_aliases(text: &str, aliases: &HashMap<String, String>) -> String {
    let mut result = text.to_lowercase();
    for (from, to) in aliases {
        result = result.replace(&from.to_lowercase(), to);
    }
    result
}

/// Normalize text for fuzzy command matching
/// Collapses spaces and normalizes number words to digits
pub fn normalize_for_matching(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .map(|word| {
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
        .join("")
}

/// Expand environment variables in a string (e.g., "$TERMINAL" -> "kitty")
pub fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();
    while let Some(start) = result.find('$') {
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
pub fn execute_custom_command(cmd: &str) -> Result<()> {
    let expanded = expand_env_vars(cmd);

    if expanded.trim().is_empty() {
        eprintln!("[SS9K] ⚠️ Command expanded to empty string (check env vars): {}", cmd);
        return Ok(());
    }

    println!("[SS9K] 🚀 Executing: {}", expanded);

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
            let parts: Vec<&str> = cmd_owned.split_whitespace().collect();
            if parts.len() == 1 && !cmd_owned.contains(['|', '&', ';', '>', '<', '$', '`', '(', ')']) {
                std::process::Command::new(&cmd_owned).spawn()
            } else {
                std::process::Command::new("sh")
                    .args(["-c", &cmd_owned])
                    .spawn()
            }
        };

        match result {
            Ok(mut child) => {
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
/// Uses a configurable leader word (default "command") to trigger commands
/// Everything goes through the leader: "command enter", "command emoji smile", "command punctuation comma"
/// Returns true if a command was executed, false if text was typed
pub fn execute_command(
    enigo: &mut Enigo,
    text: &str,
    leader: &str,
    custom_commands: &HashMap<String, String>,
    aliases: &HashMap<String, String>,
) -> Result<bool> {
    let aliased = normalize_aliases(text, aliases);

    let trimmed: String = aliased
        .trim()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase();

    // Build the leader prefix (e.g., "command ")
    let leader_prefix = format!("{} ", leader.to_lowercase());

    // Check if input starts with the leader word
    if let Some(after_leader) = trimmed.strip_prefix(&leader_prefix) {
        let cmd = after_leader.trim();

        // Check for emoji subcommand
        if let Some(emoji_name) = cmd.strip_prefix("emoji ") {
            return execute_emoji(enigo, emoji_name.trim());
        }

        // Check for punctuation subcommand
        if let Some(punct) = cmd.strip_prefix("punctuation ").or_else(|| cmd.strip_prefix("punk ")) {
            return execute_punctuation(enigo, punct.trim());
        }

        // Otherwise it's a builtin command
        return execute_builtin_command(enigo, cmd);
    }

    // Check custom commands (these work without the leader word)
    let normalized_input = normalize_for_matching(&trimmed);
    for (phrase, cmd) in custom_commands {
        if normalized_input == normalize_for_matching(phrase) {
            execute_custom_command(cmd)?;
            return Ok(true);
        }
    }

    // Default: type the text as-is
    enigo.text(&aliased)?;
    println!("[SS9K] ⌨️ Typed!");
    Ok(false)
}

/// Execute a built-in command (navigation, editing, media)
/// Handles "times N" suffix and "repeat" command
pub fn execute_builtin_command(enigo: &mut Enigo, cmd: &str) -> Result<bool> {
    let (base_cmd, count) = parse_times_suffix(cmd);

    if base_cmd == "repeat" || base_cmd.starts_with("repeat ") {
        let repeat_count = if base_cmd == "repeat" {
            count.max(1)
        } else {
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

    if let Some(shift_cmd) = base_cmd.strip_prefix("shift ") {
        return execute_shift(enigo, shift_cmd.trim());
    }

    if let Some(spell_input) = base_cmd.strip_prefix("spell ") {
        return execute_spell_mode(enigo, spell_input.trim());
    }

    if let Some(hold_key) = base_cmd.strip_prefix("hold ") {
        return execute_hold(enigo, hold_key.trim());
    }

    if base_cmd == "release all" || base_cmd == "release" {
        return execute_release_all(enigo);
    }
    if let Some(release_key) = base_cmd.strip_prefix("release ") {
        return execute_release(enigo, release_key.trim());
    }

    for i in 0..count.max(1) {
        if !execute_single_builtin_command(enigo, base_cmd)? {
            return Ok(false);
        }
        if count > 1 && i < count - 1 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    if let Ok(mut last) = LAST_COMMAND.lock() {
        *last = Some(base_cmd.to_string());
    }

    if count > 1 {
        println!("[SS9K] 🔁 Executed {} times", count);
    }

    Ok(true)
}

/// Parse a number from digit or word form
pub fn parse_number_word(s: &str) -> Option<usize> {
    if let Ok(n) = s.parse::<usize>() {
        return Some(n);
    }
    match s {
        "zero" => Some(0),
        "one" => Some(1),
        "two" | "to" | "too" => Some(2),
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
pub fn parse_times_suffix(cmd: &str) -> (&str, usize) {
    if let Some(idx) = cmd.rfind(" times ") {
        let after = &cmd[idx + 7..].trim();
        if let Some(n) = parse_number_word(after) {
            return (&cmd[..idx], n);
        }
    }
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
pub fn execute_single_builtin_command(enigo: &mut Enigo, cmd: &str) -> Result<bool> {
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

/// Execute shift-modified commands (for selections and shift+key combos)
/// Supports "times N" suffix for repetition
pub fn execute_shift(enigo: &mut Enigo, cmd: &str) -> Result<bool> {
    let (base_cmd, count) = parse_times_suffix(cmd);
    let times = count.max(1);

    enigo.key(EnigoKey::Shift, enigo::Direction::Press)?;

    for i in 0..times {
        let result = match base_cmd {
            "left" => enigo.key(EnigoKey::LeftArrow, enigo::Direction::Click),
            "right" => enigo.key(EnigoKey::RightArrow, enigo::Direction::Click),
            "up" => enigo.key(EnigoKey::UpArrow, enigo::Direction::Click),
            "down" => enigo.key(EnigoKey::DownArrow, enigo::Direction::Click),

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

            "home" => enigo.key(EnigoKey::Home, enigo::Direction::Click),
            "end" => enigo.key(EnigoKey::End, enigo::Direction::Click),
            "page up" => enigo.key(EnigoKey::PageUp, enigo::Direction::Click),
            "page down" => enigo.key(EnigoKey::PageDown, enigo::Direction::Click),
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
pub fn execute_spell_mode(enigo: &mut Enigo, input: &str) -> Result<bool> {
    let words: Vec<&str> = input.split_whitespace().collect();
    let mut result = String::new();
    let mut next_capital = false;

    for word in words {
        if word == "capital" || word == "cap" || word == "uppercase" || word == "upper" {
            next_capital = true;
            continue;
        }

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

/// Hold a key down (add to held keys list)
pub fn execute_hold(enigo: &mut Enigo, key_name: &str) -> Result<bool> {
    let key = match parse_key_name(key_name) {
        Some(k) => k,
        None => {
            eprintln!("[SS9K] ⚠️ Unknown key to hold: {}", key_name);
            return Ok(false);
        }
    };

    enigo.key(key.clone(), enigo::Direction::Press)?;

    if let Ok(mut held) = HELD_KEYS.lock() {
        if !held.iter().any(|k| std::mem::discriminant(k) == std::mem::discriminant(&key)) {
            held.push(key.clone());
        }
    }

    println!("[SS9K] 🔒 Holding: {}", key_name);
    Ok(true)
}

/// Release a specific held key
pub fn execute_release(enigo: &mut Enigo, key_name: &str) -> Result<bool> {
    let key = match parse_key_name(key_name) {
        Some(k) => k,
        None => {
            eprintln!("[SS9K] ⚠️ Unknown key to release: {}", key_name);
            return Ok(false);
        }
    };

    enigo.key(key.clone(), enigo::Direction::Release)?;

    if let Ok(mut held) = HELD_KEYS.lock() {
        held.retain(|k| std::mem::discriminant(k) != std::mem::discriminant(&key));
    }

    println!("[SS9K] 🔓 Released: {}", key_name);
    Ok(true)
}

/// Release all held keys
pub fn execute_release_all(enigo: &mut Enigo) -> Result<bool> {
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

/// Print the help/command reference
pub fn print_help() {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                    SS9K Voice Commands                       ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ All commands use a leader word (default: 'command')          ║");
    println!("║ Configure with: leader = \"voice\" in config.toml             ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ NAVIGATION: [leader] enter, tab, escape, backspace, space    ║");
    println!("║             [leader] up, down, left, right, home, end        ║");
    println!("║             [leader] page up, page down                      ║");
    println!("║ EDITING:    [leader] select all, copy, paste, cut, undo, redo║");
    println!("║             [leader] save, find, close tab, new tab          ║");
    println!("║ MEDIA:      [leader] play, pause, next, previous, mute       ║");
    println!("║             [leader] volume up, volume down                  ║");
    println!("║ REPETITION: [leader] [cmd] times [N], repeat, repeat [N]     ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ SUBCOMMANDS:                                                 ║");
    println!("║   [leader] shift [X]   - select (shift+arrow, shift+word)    ║");
    println!("║   [leader] spell [X]   - NATO spelling (alpha bravo = ab)    ║");
    println!("║   [leader] hold [X]    - hold a key (gaming, accessibility)  ║");
    println!("║   [leader] release [X] - release held key(s)                 ║");
    println!("║   [leader] emoji [X]   - insert emoji (smile, fire, etc.)    ║");
    println!("║   [leader] punctuation [X] - insert symbol (comma, arrow)    ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ CONFIG:     ~/.config/ss9k/config.toml                       ║");
    println!("║ DOCS:       https://github.com/sqrew/ss9k                    ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}
