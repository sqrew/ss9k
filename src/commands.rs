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
use enigo::{Enigo, Key as EnigoKey, Keyboard, Settings};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::lookups::{execute_emoji, execute_punctuation, parse_key_name, word_to_char};

// Wrapper for EnigoKey to implement Hash/Eq (using discriminant)
#[derive(Clone, Debug)]
pub(crate) struct HeldKey(EnigoKey);

impl PartialEq for HeldKey {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(&self.0) == std::mem::discriminant(&other.0)
    }
}

impl Eq for HeldKey {}

impl Hash for HeldKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(&self.0).hash(state);
    }
}

/// Case transformation modes for dictation
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CaseMode {
    #[default]
    Off,         // passthrough
    Snake,       // hello_world
    Camel,       // helloWorld
    Pascal,      // HelloWorld
    Kebab,       // hello-world
    Screaming,   // HELLO_WORLD
    Caps,        // HELLO WORLD
    Lower,       // hello world
    Math,        // one plus one -> 1 + 1
    Code,        // open paren x close paren -> (x)
    Alternating, // aLtErNaTiNg CaPs
    Swearing,    // fuck -> @#$%!
}

// Statics for command state
pub static LAST_COMMAND: std::sync::LazyLock<Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));
pub static HELD_KEYS: std::sync::LazyLock<Mutex<HashSet<HeldKey>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));
pub static HOLD_THREAD_RUNNING: AtomicBool = AtomicBool::new(false);
pub static KEY_REPEAT_MS: AtomicU64 = AtomicU64::new(50);
pub static CURRENT_MODE: std::sync::LazyLock<Mutex<CaseMode>> =
    std::sync::LazyLock::new(|| Mutex::new(CaseMode::Off));
pub static LAST_TYPED_LEN: AtomicUsize = AtomicUsize::new(0);

/// Normalize text by applying aliases (e.g., "e max" -> "emacs")
/// Preserves original case for non-aliased text (important for languages with meaningful capitals)
pub fn normalize_aliases(text: &str, aliases: &HashMap<String, String>) -> String {
    let mut result = text.to_string();

    for (from, to) in aliases {
        // Case-insensitive search, but preserve case of non-matched parts
        let from_lower = from.to_lowercase();
        let mut new_result = String::with_capacity(result.len());
        let mut search_start = 0;

        while let Some(pos) = result[search_start..].to_lowercase().find(&from_lower) {
            let abs_pos = search_start + pos;
            // Append everything before the match (preserving case)
            new_result.push_str(&result[search_start..abs_pos]);
            // Append the replacement
            new_result.push_str(to);
            // Move past the matched portion
            search_start = abs_pos + from.len();
        }
        // Append any remaining text after the last match
        new_result.push_str(&result[search_start..]);
        result = new_result;
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

/// Capitalize the first letter of a word
fn capitalize_word(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Apply case transformation based on current mode
pub fn apply_case_mode(text: &str) -> String {
    let mode = CURRENT_MODE.lock().map(|m| *m).unwrap_or(CaseMode::Off);

    if mode == CaseMode::Off {
        return text.to_string();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    match mode {
        CaseMode::Off => text.to_string(),
        CaseMode::Snake => words.iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join("_"),
        CaseMode::Camel => {
            let mut result = words[0].to_lowercase();
            for word in &words[1..] {
                result.push_str(&capitalize_word(&word.to_lowercase()));
            }
            result
        }
        CaseMode::Pascal => words.iter().map(|w| capitalize_word(&w.to_lowercase())).collect(),
        CaseMode::Kebab => words.iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join("-"),
        CaseMode::Screaming => words.iter().map(|w| w.to_uppercase()).collect::<Vec<_>>().join("_"),
        CaseMode::Caps => words.iter().map(|w| w.to_uppercase()).collect::<Vec<_>>().join(" "),
        CaseMode::Lower => words.iter().map(|w| w.to_lowercase()).collect::<Vec<_>>().join(" "),
        CaseMode::Math => apply_math_mode(text),
        CaseMode::Code => apply_code_mode(text),
        CaseMode::Alternating => apply_alternating_mode(text),
        CaseMode::Swearing => apply_swearing_mode(text),
    }
}

/// Strip punctuation from a word for matching (keeps the word itself clean)
fn strip_punct(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// Apply math mode transformation: convert spoken math to symbols
/// "one plus one" → "1 + 1"
/// "five times three" → "5 * 3"
/// "x greater than y" → "x > y"
pub fn apply_math_mode(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    // Pre-strip punctuation from all words for matching
    let clean: Vec<String> = words.iter().map(|w| strip_punct(w)).collect();

    let mut result = Vec::new();
    let mut i = 0;

    while i < words.len() {
        // Check for multi-word phrases first (longest match)

        // Five-word phrases
        if i + 4 < clean.len() {
            let five = format!("{} {} {} {} {}",
                clean[i], clean[i+1], clean[i+2], clean[i+3], clean[i+4]
            );
            if let Some(sym) = match five.as_str() {
                "greater than or equal to" => Some(">="),
                "less than or equal to" => Some("<="),
                _ => None,
            } {
                result.push(sym.to_string());
                i += 5;
                continue;
            }
        }

        // Three-word phrases
        if i + 2 < clean.len() {
            let three = format!("{} {} {}", clean[i], clean[i+1], clean[i+2]);
            if let Some(sym) = match three.as_str() {
                "divided by" | "over" => None, // handled in two-word
                "is equal to" => Some("="),
                "not equal to" | "not equals to" => Some("!="),
                "to the power" => Some("^"), // "to the power of" would be 4 words
                _ => None,
            } {
                result.push(sym.to_string());
                i += 3;
                continue;
            }
        }

        // Two-word phrases
        if i + 1 < clean.len() {
            let two = format!("{} {}", clean[i], clean[i+1]);
            if let Some(sym) = match two.as_str() {
                "divided by" | "divided over" => Some("/"),
                "multiplied by" => Some("*"),
                "greater than" => Some(">"),
                "less than" => Some("<"),
                "equal to" | "equals to" => Some("="),
                "not equal" | "not equals" => Some("!="),
                "double equals" | "double equal" | "triple equals" | "strict equals" => Some("=="),
                "open paren" | "open parenthesis" | "left paren" | "left parenthesis" => Some("("),
                "close paren" | "close parenthesis" | "right paren" | "right parenthesis" => Some(")"),
                "open bracket" | "left bracket" => Some("["),
                "close bracket" | "right bracket" => Some("]"),
                "open brace" | "left brace" => Some("{"),
                "close brace" | "right brace" => Some("}"),
                "at least" => Some(">="),
                "at most" => Some("<="),
                _ => None,
            } {
                result.push(sym.to_string());
                i += 2;
                continue;
            }
        }

        // Single words (use pre-cleaned version)
        let word = &clean[i];
        let converted = match word.as_str() {
            // Numbers 0-20
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
            "eleven" => "11",
            "twelve" => "12",
            "thirteen" => "13",
            "fourteen" => "14",
            "fifteen" => "15",
            "sixteen" => "16",
            "seventeen" => "17",
            "eighteen" => "18",
            "nineteen" => "19",
            "twenty" => "20",

            // Operators
            "plus" | "add" => "+",
            "minus" | "subtract" => "-",
            "times" | "multiplied" | "multiply" => "*",
            "divided" | "divide" | "over" => "/",
            "equals" | "equal" => "=",
            "modulo" | "mod" => "%",
            "caret" | "power" | "exponent" => "^",

            // Comparisons (single word forms)
            "greater" => ">",  // "greater" alone = >
            "less" => "<",     // "less" alone = <

            // Punctuation/grouping
            "point" | "dot" | "decimal" => ".",
            "comma" => ",",
            "percent" => "%",
            "paren" | "parenthesis" => "(", // ambiguous, default open
            "bracket" => "[",
            "brace" => "{",

            // Math constants (just type them)
            "pi" => "π",
            "infinity" => "∞",

            // Pass through anything else (variables like x, y, z)
            _ => &word,
        };

        result.push(converted.to_string());
        i += 1;
    }

    result.join(" ")
}

/// Apply code mode transformation: convert symbol names to tight symbols for coding
/// "function open paren x close paren" → "function(x)"
/// "if x double equals y open brace" → "if x == y {"
pub fn apply_code_mode(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let clean: Vec<String> = words.iter().map(|w| strip_punct(w)).collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < words.len() {
        // Check for two-word symbol phrases first
        if i + 1 < clean.len() {
            let two = format!("{} {}", clean[i], clean[i+1]);
            if let Some(sym) = match two.as_str() {
                // Parentheses
                "open paren" | "open parenthesis" | "left paren" | "left parenthesis" => Some("("),
                "close paren" | "close parenthesis" | "right paren" | "right parenthesis" => Some(")"),
                // Brackets
                "open bracket" | "left bracket" => Some("["),
                "close bracket" | "right bracket" => Some("]"),
                // Braces
                "open brace" | "left brace" => Some("{"),
                "close brace" | "right brace" => Some("}"),
                // Angle brackets
                "open angle" | "left angle" | "less than" => Some("<"),
                "close angle" | "right angle" | "greater than" => Some(">"),
                // Double symbols
                "double equals" | "equals equals" | "double equal" => Some("=="),
                "not equals" | "not equal" | "bang equals" => Some("!="),
                "double ampersand" | "and and" | "ampersand ampersand" => Some("&&"),
                "double pipe" | "or or" | "pipe pipe" => Some("||"),
                "double colon" | "colon colon" => Some("::"),
                "double slash" | "slash slash" => Some("//"),
                "fat arrow" | "arrow fat" | "equals arrow" => Some("=>"),
                "thin arrow" | "arrow thin" | "dash arrow" | "minus arrow" => Some("->"),
                "plus equals" | "plus equal" => Some("+="),
                "minus equals" | "minus equal" => Some("-="),
                "times equals" | "star equals" => Some("*="),
                "divide equals" | "slash equals" => Some("/="),
                "less equal" | "less equals" => Some("<="),
                "greater equal" | "greater equals" => Some(">="),
                "left shift" | "shift left" => Some("<<"),
                "right shift" | "shift right" => Some(">>"),
                // Common words that become symbols in code
                "and sign" => Some("&"),
                "or sign" => Some("|"),
                _ => None,
            } {
                result.push(sym.to_string());
                i += 2;
                continue;
            }
        }

        // Single word symbols
        let word = &clean[i];
        let converted = match word.as_str() {
            // Single character symbols
            "dot" | "period" => ".",
            "comma" => ",",
            "colon" => ":",
            "semicolon" | "semi" => ";",
            "equals" | "equal" => "=",
            "plus" => "+",
            "minus" | "dash" | "hyphen" => "-",
            "asterisk" | "star" | "times" => "*",
            "slash" | "divide" => "/",
            "backslash" => "\\",
            "pipe" => "|",
            "ampersand" | "amp" => "&",
            "caret" | "hat" => "^",
            "tilde" | "squiggle" => "~",
            "at" => "@",
            "hash" | "pound" | "hashtag" => "#",
            "dollar" | "dollars" => "$",
            "percent" => "%",
            "bang" | "exclamation" => "!",
            "question" => "?",
            "underscore" => "_",
            "backtick" | "grave" => "`",
            "quote" | "double quote" => "\"",
            "single quote" | "apostrophe" => "'",
            // Standalone parens/brackets (ambiguous, default open)
            "paren" | "parenthesis" => "(",
            "bracket" => "[",
            "brace" => "{",
            "angle" => "<",
            // Pass through anything else
            _ => "",
        };

        if converted.is_empty() {
            // Not a symbol, keep original word with space
            result.push(words[i].to_string());
        } else {
            result.push(converted.to_string());
        }
        i += 1;
    }

    // Join without spaces between symbols, with spaces between words
    let mut output = String::new();
    for (idx, token) in result.iter().enumerate() {
        if idx > 0 {
            // Add space only if both current and previous are words (not symbols)
            let prev = &result[idx - 1];
            let prev_is_word = prev.chars().all(|c| c.is_alphanumeric() || c == '_');
            let curr_is_word = token.chars().all(|c| c.is_alphanumeric() || c == '_');
            if prev_is_word && curr_is_word {
                output.push(' ');
            }
        }
        output.push_str(token);
    }

    output
}

/// Apply alternating case transformation: aLtErNaTiNg CaPs
pub fn apply_alternating_mode(text: &str) -> String {
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_lowercase().to_string()
            } else {
                c.to_uppercase().to_string()
            }
        })
        .collect()
}

/// Apply swearing mode: replaces swear words with grawlix (@#$%!)
pub fn apply_swearing_mode(text: &str) -> String {
    let grawlix = ["@", "#", "$", "%", "!", "&", "*"];
    let swears = [
        "fuck", "fucking", "fucked", "fucker", "fucks",
        "shit", "shitting", "shitty", "shits",
        "damn", "damned", "dammit",
        "ass", "asses", "asshole", "assholes",
        "bitch", "bitches", "bitchy",
        "crap", "crappy",
        "hell", "heck",
        "bastard", "bastards",
        "dick", "dicks",
        "piss", "pissed", "pissing",
        "cock", "cocks",
        "cunt", "cunts",
    ];

    let words: Vec<&str> = text.split_whitespace().collect();
    let result: Vec<String> = words.iter().map(|word| {
        let lower = word.to_lowercase();
        let clean = lower.trim_matches(|c: char| !c.is_alphanumeric());

        if swears.contains(&clean) {
            // Generate grawlix of similar length
            let len = word.len().max(3).min(7);
            (0..len).map(|i| grawlix[i % grawlix.len()]).collect()
        } else {
            word.to_string()
        }
    }).collect();

    result.join(" ")
}

/// Set the current case mode
pub fn set_case_mode(mode: CaseMode) {
    if let Ok(mut current) = CURRENT_MODE.lock() {
        *current = mode;
    }
}

/// Get the current case mode
pub fn get_case_mode() -> CaseMode {
    CURRENT_MODE.lock().map(|m| *m).unwrap_or(CaseMode::Off)
}

/// Parse a mode name into CaseMode
pub fn parse_mode_name(name: &str) -> Option<CaseMode> {
    match name.to_lowercase().as_str() {
        "off" | "normal" | "default" => Some(CaseMode::Off),
        "snake" | "snek" => Some(CaseMode::Snake),
        "camel" => Some(CaseMode::Camel),
        "pascal" => Some(CaseMode::Pascal),
        "kebab" | "kebob" => Some(CaseMode::Kebab),
        "screaming" | "scream" | "yelling" | "yell" => Some(CaseMode::Screaming),
        "caps" | "upper" | "uppercase" | "capital" | "capitals" => Some(CaseMode::Caps),
        "lower" | "lowercase" => Some(CaseMode::Lower),
        "math" | "maths" | "numeral" | "numerals" | "numbers" => Some(CaseMode::Math),
        "code" | "coding" | "programming" | "symbols" => Some(CaseMode::Code),
        "alternating" | "alternate" | "spongebob" | "mocking" => Some(CaseMode::Alternating),
        "swearing" | "swear" | "grawlix" | "censored" | "censor" => Some(CaseMode::Swearing),
        _ => None,
    }
}

/// Execute mode command
pub fn execute_mode(mode_name: &str) -> Result<bool> {
    match parse_mode_name(mode_name) {
        Some(mode) => {
            set_case_mode(mode);
            let mode_str = match mode {
                CaseMode::Off => "off (normal)",
                CaseMode::Snake => "snake_case",
                CaseMode::Camel => "camelCase",
                CaseMode::Pascal => "PascalCase",
                CaseMode::Kebab => "kebab-case",
                CaseMode::Screaming => "SCREAMING_SNAKE_CASE",
                CaseMode::Caps => "CAPS LOCK",
                CaseMode::Lower => "lowercase",
                CaseMode::Math => "math (one plus one → 1 + 1)",
                CaseMode::Code => "code (open paren → ()",
                CaseMode::Alternating => "aLtErNaTiNg CaPs",
                CaseMode::Swearing => "swearing (fuck → @#$%!)",
            };
            println!("[SS9K] 🔤 Mode: {}", mode_str);
            Ok(true)
        }
        None => {
            eprintln!("[SS9K] ⚠️ Unknown mode: {}", mode_name);
            eprintln!("[SS9K] Available: off, snake, camel, pascal, kebab, screaming, caps, lower, math, code, alternating, swearing");
            Ok(false)
        }
    }
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

/// Expand placeholders in insert text
/// {date} → 2026-01-17
/// {time} → 13:52
/// {datetime} → 2026-01-17 13:52
/// {timestamp} → Unix timestamp
/// {iso} → ISO 8601 format
/// {shell:command} → output of shell command
fn expand_placeholders(text: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = chrono::Local::now();
    let mut result = text.to_string();

    result = result.replace("{date}", &now.format("%Y-%m-%d").to_string());
    result = result.replace("{time}", &now.format("%H:%M").to_string());
    result = result.replace("{datetime}", &now.format("%Y-%m-%d %H:%M").to_string());
    result = result.replace("{iso}", &now.format("%Y-%m-%dT%H:%M:%S%:z").to_string());

    if result.contains("{timestamp}") {
        if let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) {
            result = result.replace("{timestamp}", &duration.as_secs().to_string());
        }
    }

    // Expand {shell:command} placeholders
    while let Some(start) = result.find("{shell:") {
        if let Some(end) = result[start..].find('}') {
            let end = start + end;
            let cmd = &result[start + 7..end]; // skip "{shell:"
            let output = match std::process::Command::new("sh")
                .args(["-c", cmd])
                .output()
            {
                Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
                Err(e) => {
                    eprintln!("[SS9K] ⚠️ Shell command failed: {}", e);
                    String::new()
                }
            };
            result = format!("{}{}{}", &result[..start], output, &result[end + 1..]);
        } else {
            break; // malformed placeholder, stop
        }
    }

    // Handle escaped newlines
    result = result.replace("\\n", "\n");
    result = result.replace("\\t", "\t");

    result
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
    inserts: &HashMap<String, String>,
    wrappers: &HashMap<String, String>,
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

        // Check for insert subcommand
        if let Some(insert_name) = cmd.strip_prefix("insert ") {
            let name = insert_name.trim();
            if let Some(template) = inserts.get(name) {
                let expanded = expand_placeholders(template);
                enigo.text(&expanded)?;
                LAST_TYPED_LEN.store(expanded.chars().count(), Ordering::SeqCst);
                println!("[SS9K] 📋 Inserted '{}': {}", name, expanded.chars().take(50).collect::<String>());
                return Ok(true);
            } else {
                eprintln!("[SS9K] ⚠️ Unknown insert: '{}'", name);
                eprintln!("[SS9K] Available: {:?}", inserts.keys().collect::<Vec<_>>());
                return Ok(false);
            }
        }

        // Check for wrap subcommand: "wrap <name> <text>"
        if let Some(wrap_rest) = cmd.strip_prefix("wrap ") {
            let parts: Vec<&str> = wrap_rest.splitn(2, ' ').collect();
            if parts.is_empty() {
                eprintln!("[SS9K] ⚠️ Wrap requires a name and text: 'command wrap quotes hello'");
                return Ok(false);
            }
            let wrapper_name = parts[0].trim();
            let wrap_text = if parts.len() > 1 { parts[1].trim() } else { "" };

            if let Some(wrapper) = wrappers.get(wrapper_name) {
                let (left, right) = if let Some(idx) = wrapper.find('|') {
                    (&wrapper[..idx], &wrapper[idx + 1..])
                } else {
                    (wrapper.as_str(), wrapper.as_str())
                };
                let wrapped = format!("{}{}{}", left, wrap_text, right);
                enigo.text(&wrapped)?;
                LAST_TYPED_LEN.store(wrapped.chars().count(), Ordering::SeqCst);
                println!("[SS9K] 🎁 Wrapped '{}': {}", wrapper_name, wrapped);
                return Ok(true);
            } else {
                eprintln!("[SS9K] ⚠️ Unknown wrapper: '{}'", wrapper_name);
                eprintln!("[SS9K] Available: {:?}", wrappers.keys().collect::<Vec<_>>());
                return Ok(false);
            }
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

    // Default: type the text with case mode applied
    let output = apply_case_mode(&aliased);
    enigo.text(&output)?;

    // Track length for "scratch that" undo
    LAST_TYPED_LEN.store(output.chars().count(), Ordering::SeqCst);

    let mode = get_case_mode();
    if mode != CaseMode::Off {
        println!("[SS9K] ⌨️ Typed ({:?}): {}", mode, output);
    } else {
        println!("[SS9K] ⌨️ Typed!");
    }
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

    // Scratch that - undo last typed text
    if base_cmd == "scratch that" || base_cmd == "undo" || base_cmd == "scratch" {
        let len = LAST_TYPED_LEN.swap(0, Ordering::SeqCst);
        if len > 0 {
            for _ in 0..len {
                enigo.key(EnigoKey::Backspace, enigo::Direction::Click)?;
            }
            println!("[SS9K] ⏪ Scratched {} character(s)", len);
            return Ok(true);
        } else {
            eprintln!("[SS9K] ⚠️ Nothing to scratch");
            return Ok(false);
        }
    }

    if let Some(mode_name) = base_cmd.strip_prefix("mode ") {
        return execute_mode(mode_name.trim());
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

        // Help, Config & Info
        "help" => {
            print_help();
        }
        "languages" | "language list" | "list languages" => {
            print_languages();
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

/// Set the key repeat rate (called from main before executing commands)
pub fn set_key_repeat_ms(ms: u64) {
    KEY_REPEAT_MS.store(ms, Ordering::SeqCst);
}

/// Spawn the hold thread if not already running
fn spawn_hold_thread() {
    if HOLD_THREAD_RUNNING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        std::thread::spawn(|| {
            println!("[SS9K] 🔄 Hold thread started");

            // Create our own Enigo instance for this thread
            let mut enigo = match Enigo::new(&Settings::default()) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[SS9K] ❌ Hold thread failed to create Enigo: {}", e);
                    HOLD_THREAD_RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            };

            loop {
                let repeat_ms = KEY_REPEAT_MS.load(Ordering::SeqCst);

                // Get snapshot of held keys
                let keys: Vec<EnigoKey> = if let Ok(held) = HELD_KEYS.lock() {
                    if held.is_empty() {
                        break; // No more keys, exit thread
                    }
                    held.iter().map(|hk| hk.0.clone()).collect()
                } else {
                    break;
                };

                // Click all held keys together
                for key in &keys {
                    if let Err(e) = enigo.key(key.clone(), enigo::Direction::Click) {
                        eprintln!("[SS9K] ⚠️ Hold thread key error: {}", e);
                    }
                }

                std::thread::sleep(Duration::from_millis(repeat_ms));
            }

            HOLD_THREAD_RUNNING.store(false, Ordering::SeqCst);
            println!("[SS9K] 🔄 Hold thread stopped");
        });
    }
}

/// Hold a key down (add to held keys, spawn spam thread)
pub fn execute_hold(_enigo: &mut Enigo, key_name: &str) -> Result<bool> {
    let key = match parse_key_name(key_name) {
        Some(k) => k,
        None => {
            eprintln!("[SS9K] ⚠️ Unknown key to hold: {}", key_name);
            return Ok(false);
        }
    };

    // Add to held keys set
    if let Ok(mut held) = HELD_KEYS.lock() {
        held.insert(HeldKey(key));
    }

    // Spawn hold thread if not running
    spawn_hold_thread();

    println!("[SS9K] 🔒 Holding: {}", key_name);
    Ok(true)
}

/// Release a specific held key (remove from set, thread will stop clicking it)
pub fn execute_release(_enigo: &mut Enigo, key_name: &str) -> Result<bool> {
    let key = match parse_key_name(key_name) {
        Some(k) => k,
        None => {
            eprintln!("[SS9K] ⚠️ Unknown key to release: {}", key_name);
            return Ok(false);
        }
    };

    if let Ok(mut held) = HELD_KEYS.lock() {
        held.remove(&HeldKey(key));
    }

    println!("[SS9K] 🔓 Released: {}", key_name);
    Ok(true)
}

/// Release all held keys (clear set, thread will exit)
pub fn execute_release_all(_enigo: &mut Enigo) -> Result<bool> {
    let count = if let Ok(mut held) = HELD_KEYS.lock() {
        let c = held.len();
        held.clear();
        c
    } else {
        0
    };

    if count == 0 {
        println!("[SS9K] 🔓 No keys held");
        return Ok(true);
    }

    println!("[SS9K] 🔓 Released {} key(s)", count);
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
    println!("║   [leader] insert [X]  - insert snippet from config          ║");
    println!("║   [leader] wrap [X] [text] - wrap text (quotes, parens, etc) ║");
    println!("║   [leader] mode [X]    - modes: snake, camel, pascal, kebab, ║");
    println!("║                          screaming, caps, lower, math, code, ║");
    println!("║                          alternating, swearing, off          ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ INFO:       [leader] languages - list supported languages      ║");
    println!("║ CONFIG:     ~/.config/ss9k/config.toml                       ║");
    println!("║ DOCS:       https://github.com/sqrew/ss9k                    ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}

/// Print supported languages for Whisper
pub fn print_languages() {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║              SS9K Supported Languages (Whisper)              ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Set in config.toml: language = \"en\"                         ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ af - Afrikaans      el - Greek          nl - Dutch          ║");
    println!("║ am - Amharic        en - English        no - Norwegian      ║");
    println!("║ ar - Arabic         es - Spanish        pa - Punjabi        ║");
    println!("║ as - Assamese       et - Estonian       pl - Polish         ║");
    println!("║ az - Azerbaijani    eu - Basque         pt - Portuguese     ║");
    println!("║ ba - Bashkir        fa - Persian        ro - Romanian       ║");
    println!("║ be - Belarusian     fi - Finnish        ru - Russian        ║");
    println!("║ bg - Bulgarian      fo - Faroese        sa - Sanskrit       ║");
    println!("║ bn - Bengali        fr - French         sd - Sindhi         ║");
    println!("║ bo - Tibetan        gl - Galician       si - Sinhala        ║");
    println!("║ br - Breton         gu - Gujarati       sk - Slovak         ║");
    println!("║ bs - Bosnian        ha - Hausa          sl - Slovenian      ║");
    println!("║ ca - Catalan        haw - Hawaiian      sn - Shona          ║");
    println!("║ cs - Czech          he - Hebrew         so - Somali         ║");
    println!("║ cy - Welsh          hi - Hindi          sq - Albanian       ║");
    println!("║ da - Danish         hr - Croatian       sr - Serbian        ║");
    println!("║ de - German         ht - Haitian        su - Sundanese      ║");
    println!("║ el - Greek          hu - Hungarian      sv - Swedish        ║");
    println!("║ hy - Armenian       id - Indonesian     sw - Swahili        ║");
    println!("║ is - Icelandic      it - Italian        ta - Tamil          ║");
    println!("║ ja - Japanese       jw - Javanese       te - Telugu         ║");
    println!("║ ka - Georgian       kk - Kazakh         tg - Tajik          ║");
    println!("║ km - Khmer          kn - Kannada        th - Thai           ║");
    println!("║ ko - Korean         la - Latin          tl - Tagalog        ║");
    println!("║ lb - Luxembourgish  ln - Lingala        tr - Turkish        ║");
    println!("║ lo - Lao            lt - Lithuanian     tt - Tatar          ║");
    println!("║ lv - Latvian        mg - Malagasy       uk - Ukrainian      ║");
    println!("║ mi - Maori          mk - Macedonian     ur - Urdu           ║");
    println!("║ ml - Malayalam      mn - Mongolian      uz - Uzbek          ║");
    println!("║ mr - Marathi        ms - Malay          vi - Vietnamese     ║");
    println!("║ mt - Maltese        my - Myanmar        yi - Yiddish        ║");
    println!("║ ne - Nepali         yo - Yoruba         zh - Chinese        ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Full list: https://github.com/openai/whisper#available-models║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}
