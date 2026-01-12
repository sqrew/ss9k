# SuperScreecher9000

**Screech at your computer. It listens.**

A local, GPU-accelerated speech-to-text tool that types wherever your cursor is. Built for accessibility, privacy, and freedom.

## What It Does

Press a key, speak, release. Your words appear at cursor. Or say a command. No cloud. No API keys. No subscriptions. Just you and your computer.

## Features

- **Local inference** - Whisper runs on YOUR machine
- **GPU accelerated** - Vulkan (Intel/AMD), CUDA (NVIDIA), Metal (macOS)
- **Types at cursor** - Works anywhere: editors, browsers, chat apps
- **Voice commands** - Navigation, editing, media controls, custom shell commands
- **Leader words** - No reserved words; say "command enter" vs just "enter"
- **Punctuation** - 50+ symbols via voice: "punctuation arrow" → `=>`
- **Spell mode** - NATO phonetic or raw letters: "spell alpha bravo charlie" → `abc`
- **Hot-reload config** - Change settings without restarting
- **Multiple models** - tiny (75MB) to large (3GB), pick your speed/accuracy tradeoff
- **Cross-platform ready** - Built with portable Rust crates

## Installation

```bash
# Clone and build
git clone https://github.com/sqrew/ss9k
cd ss9k
cargo build --release

# Run it - model downloads automatically on first launch
./target/release/ss9k
```

## Usage

```bash
# Hold your hotkey (F12 default), speak, release
# Text appears at cursor
```

### Voice Commands

SS9K uses **leader words** to distinguish commands from dictation:

- `"command enter"` → presses Enter key
- `"enter"` → types the word "enter"
- `"punctuation period"` → types `.`
- `"period"` → types the word "period"

**Commands** (say "command" + any of these):

| Category       | Commands                                                                             |
|----------------|--------------------------------------------------------------------------------------|
| **Navigation** | enter, tab, escape, backspace, space, up, down, left, right, home, end, page up/down |
| **Editing**    | select all, copy, paste, cut, undo, redo, save, find, close tab, new tab             |
| **Media**      | play, pause, next, skip, previous, volume up, volume down, mute                      |

**Punctuation** (say "punctuation" + any of these):

| Category        | Options                                                                              |
|-----------------|--------------------------------------------------------------------------------------|
| **Basic**       | period, comma, question, exclamation, colon, semicolon                               |
| **Quotes**      | quote, single quote, backtick                                                        |
| **Brackets**    | open/close paren, bracket, brace, angle                                              |
| **Symbols**     | plus, minus, equals, asterisk, slash, pipe, at, hash, etc.                           |
| **Programming** | arrow (=>), thin arrow (->), double colon, equals equals, not equals, and and, or or |

**Spell Mode** (say "spell" + letters/numbers):

| Input                              | Output |
|------------------------------------|--------|
| `spell alpha bravo charlie`        | abc    |
| `spell capital alpha bravo`        | Ab     |
| `spell one two three`              | 123    |
| `spell a b c`                      | abc    |
| `spell cap mike cap sierra`        | MS     |

Supports: NATO phonetic (alpha-zulu), number words (zero-nine), raw letters (a-z), raw digits (0-9).
Capital modifiers: `capital`, `cap`, `uppercase`, `upper`.

**Custom commands** (from config) work without a leader word.

**Tip:** Use aliases to shorten leaders: `"cmd" = "command"` → say "cmd enter"

### Configuration

Create `~/.config/ss9k/config.toml`:

```toml
model = "small"              # tiny, base, small, medium, large
language = "en"              # ISO 639-1 code
threads = 4                  # whisper inference threads
device = ""                  # audio device (empty = auto-detect)
hotkey = "F12"               # see supported hotkeys below
hotkey_mode = "hold"         # hold (release to stop) or toggle (press again to stop)
toggle_timeout_secs = 0      # auto-stop after N seconds in toggle mode (0 = no timeout)

[commands]
"open terminal" = "kitty"
"open browser" = "$BROWSER"  # supports $ENV_VAR expansion
"screenshot" = "flameshot gui"

[aliases]
"taping" = "typing"          # fix consistent misrecognitions
```

**Supported hotkeys:** F1-F12, ScrollLock, Pause, PrintScreen, Insert, Home, End, PageUp, PageDown, Num0-Num9

Config hot-reloads when you save - no restart needed.

## Models

| Model    | Size  | Speed   | Accuracy  | Use Case                      |
|----------|-------|---------|-----------|-------------------------------|
| tiny     | 75MB  | Fastest | Basic     | Quick notes, low-end hardware |
| base     | 142MB | Fast    | Good      | General use                   |
| small    | 466MB | Medium  | Great     | **Recommended default**       |
| medium   | 1.5GB | Slow    | Excellent | When accuracy matters         |
| large-v3 | 3GB   | Slowest | Best      | Maximum quality               |

Models auto-download on first launch. Change `model` in config to switch.

## Hardware

**Minimum:**
- Any x86_64 CPU
- 2GB RAM (+ model size)
- Microphone

**Recommended:**
- Modern CPU (last 5 years)
- GPU with Vulkan support
- 8GB RAM
- Decent microphone

**Tested on:**
- Intel i5-6500 + HD Graphics 530 (works, ~15s inference on medium)
- Your machine (probably faster)

## GPU Backends

Build with the appropriate feature:
```bash
cargo build --release --features vulkan  # Intel/AMD (Linux/Windows)
cargo build --release --features cuda    # NVIDIA (requires CUDA toolkit)
cargo build --release --features metal   # macOS
```

## Known Issues

- **Wayland**: Global hotkeys don't work (Wayland security model). Use X11.

## Why?

For people who:
- Have RSI or carpal tunnel
- Have motor disabilities
- Think better out loud
- Want local/private speech-to-text
- Are tired of cloud subscriptions
- Rage-smashed your keyboard into pieces

5GB for a fully offline accessibility tool is nothing. Your voice stays on your machine.
Just remove unused models to save space if it's important.

## Roadmap

- [x] Config file (model, hotkey, device selection)
- [x] Model auto-download on first run
- [x] Voice commands (navigation, editing, media)
- [x] Custom command mapping (voice → shell)
- [x] Alias system (fix misrecognitions)
- [x] Toggle mode + timeout
- [x] Hot-reload config
- [x] Async processing (non-blocking inference)
- [x] Leader words (no reserved words - full vocabulary available)
- [x] Punctuation commands (50+ symbols)
- [x] Spell mode (NATO phonetic, raw letters, numbers)
- [ ] Streaming transcription (type as you speak)
- [ ] Cross-platform testing (Windows, macOS)

## License

MIT. Do whatever you want.

## Credits

Built with:
- [whisper-rs](https://github.com/tazz4843/whisper-rs) - Whisper.cpp Rust bindings
- [cpal](https://github.com/RustAudio/cpal) - Cross-platform audio
- [rdev](https://github.com/Narsil/rdev) - Global hotkey capture
- [enigo](https://github.com/enigo-rs/enigo) - Keyboard simulation
- [rubato](https://github.com/HEnquist/rubato) - Audio resampling
- [arc-swap](https://github.com/vorner/arc-swap) - Lock-free config hot-reload
- [notify](https://github.com/notify-rs/notify) - File watching

Built by sqrew + Claude. The screech is real. 🦀
