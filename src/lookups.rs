//! Lookup tables and data-driven functions for SS9K
//!
//! This module contains the "data" parts of SS9K:
//! - Punctuation symbol lookup
//! - Emoji lookup
//! - NATO phonetic alphabet / word-to-char mapping
//! - Key name parsing for hold/release

use anyhow::Result;
use enigo::{Enigo, Key as EnigoKey, Keyboard};

/// Execute punctuation insertion
/// Includes common Whisper mishearings for robustness
pub fn execute_punctuation(enigo: &mut Enigo, punct: &str) -> Result<bool> {
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
pub fn execute_emoji(enigo: &mut Enigo, name: &str) -> Result<bool> {
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

/// Parse a key name to an EnigoKey (for hold/release functionality)
pub fn parse_key_name(name: &str) -> Option<EnigoKey> {
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

/// Map a word to a single character (NATO, raw letter, number word, or raw digit)
pub fn word_to_char(word: &str) -> Option<char> {
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
