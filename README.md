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
- **Spell mode** - NATO phonetic, letters, numbers, punctuation: "command spell alpha at bravo dot com" → `a@b.com`
- **Shift mode** - Text selection: "command shift right times five" selects 5 characters
- **Hold/Release** - Hold keys for gaming/accessibility: "command hold w" runs forward
- **Emoji** - 80+ emoji via voice: "emoji thumbs up" → 👍
- **Repetition** - "command backspace times five" or "command repeat three"
- **Mishearing tolerance** - Built-in handling for common Whisper errors (caret/carrot, colon/colin, etc.)
- **Fuzzy matching** - Custom commands match despite spacing/number variations
- **Self-documenting** - "command help" shows all commands, "command config" opens config
- **Hot-reload config** - Change settings without restarting
- **Quiet mode** - Suppress verbose output once you're comfortable
- **Multiple models** - tiny (75MB) to large (3GB), pick your speed/accuracy tradeoff
- **Cross-platform ready** - Built with portable Rust crates

## Installation

```bash
# Clone and build (Linux)
git clone https://github.com/sqrew/ss9k
cd ss9k
cargo build --release

# Run it - model downloads automatically on first launch
./target/release/ss9k
```

### Windows Installation

Windows requires additional dependencies:

1. **Visual Studio Build Tools** with "Desktop development with C++" workload
   - Download from [Visual Studio Downloads](https://visualstudio.microsoft.com/downloads/)
   - Select "Build Tools for Visual Studio"
   - In the installer, check "Desktop development with C++"

2. **LLVM/Clang** (needed for whisper-rs bindings)
   - `winget install LLVM.LLVM` or download from [LLVM Releases](https://github.com/llvm/llvm-project/releases)
   - Make sure to add to PATH during installation

3. **Rust** via [rustup](https://rustup.rs)

4. **Git** from [git-scm.com](https://git-scm.com)

Then build from **"x64 Native Tools Command Prompt for VS"**:
```cmd
git clone https://github.com/sqrew/ss9k
cd ss9k
cargo build --release
```

Yeah, it's a lot. Linux is easier. But it works!

## Usage

```bash
# Hold your hotkey (F12 default), speak, release
# Text appears at cursor
```

### Voice Commands

SS9K uses a **leader word** (default: `command`) to distinguish commands from dictation:

- `"command enter"` → presses Enter key
- `"enter"` → types the word "enter"
- `"command punctuation period"` → types `.` (or `"command punk period"`)
- `"command spell alpha at bravo"` → types `a@b`
- `"command emoji fire"` → types 🔥

**Everything goes through the leader word.** Configure it in your config:
```toml
leader = "voice"  # or "computer", "hey", whatever feels natural
```

**Commands** (say "command" + any of these):

| Category       | Commands                                                                             |
|----------------|--------------------------------------------------------------------------------------|
| **Navigation** | enter, tab, escape, backspace, space, up, down, left, right, home, end, page up/down |
| **Editing**    | select all, copy, paste, cut, undo, redo, save, find, close tab, new tab             |
| **Media**      | play, pause, next, skip, previous, volume up, volume down, mute                      |
| **Utility**    | help (show commands), config (open config), repeat, repeat [N]                       |

**Punctuation** (say "command punctuation" + any of these, or "command punk"):

| Category        | Options                                                                              |
|-----------------|--------------------------------------------------------------------------------------|
| **Basic**       | period, comma, question, exclamation, colon, semicolon                               |
| **Quotes**      | quote, single quote, backtick                                                        |
| **Brackets**    | open/close paren, bracket, brace, angle                                              |
| **Symbols**     | plus, minus, equals, asterisk, slash, pipe, at, hash, etc.                           |
| **Programming** | arrow (=>), thin arrow (->), double colon, equals equals, not equals, and and, or or |

**Spell Mode** (say "command spell" + letters/numbers/punctuation):

| Input                                    | Output   |
|------------------------------------------|----------|
| `command spell alpha bravo charlie`      | abc      |
| `command spell capital alpha bravo`      | Ab       |
| `command spell one two three`            | 123      |
| `command spell alpha at bravo dot com`   | a@b.com  |
| `command spell alpha space bravo`        | a b      |
| `command spell alpha underscore bravo`   | a_b      |

Supports: NATO phonetic (alpha-zulu), number words (zero-nine), raw letters, raw digits, space, and punctuation (dot, at, dash, underscore, slash, colon, hash, etc.).
Capital modifiers: `capital`, `cap`, `uppercase`, `upper`.

**Shift Mode** (say "command shift" + direction for text selection):

| Input                              | Effect                        |
|------------------------------------|-------------------------------|
| `command shift right`              | Select 1 character right      |
| `command shift right times five`   | Select 5 characters right     |
| `command shift word left`          | Select word left              |
| `command shift home`               | Select to start of line       |
| `command shift end`                | Select to end of line         |
| `command shift page down`          | Select a page down            |

Supports: left, right, up, down, word left, word right, home, end, page up, page down, tab, enter.

**Hold/Release** (for gaming, accessibility, or modifier keys):

| Input                              | Effect                        |
|------------------------------------|-------------------------------|
| `command hold w`                   | Hold W key (run forward)      |
| `command hold shift`               | Hold Shift modifier           |
| `command release w`                | Release W key                 |
| `command release all`              | Release all held keys         |

Supports: all letters (a-z), modifiers (shift, control/ctrl, alt, meta/super/win), arrows (up, down, left, right), and common keys (space, enter, tab, escape, backspace).

**How it works:** Hold mode rapidly presses the key (configurable via `key_repeat_ms`). All held keys press together, so "hold shift" + "hold w" works for sprint+move.

**Tip:** Use hold for games ("command hold w" to run), accessibility, or any situation where you need a key pressed continuously.

**Emoji** (say "command emoji" + name):

| Input                        | Output |
|------------------------------|--------|
| `command emoji smile`        | 😊     |
| `command emoji thumbs up`    | 👍     |
| `command emoji fire`         | 🔥     |
| `command emoji blue heart`   | 💙     |
| `command emoji crab`         | 🦀     |
| `command emoji poop`         | 💩     |

80+ emoji available: faces, gestures, hearts (all colors), animals, objects, symbols. Say "command emoji rust" for 🦀.

**Repetition** (add "times N" to any command, or use "repeat"):

| Input                              | Effect                        |
|------------------------------------|-------------------------------|
| `command backspace times five`     | Delete 5 characters           |
| `command down times ten`           | Move down 10 lines            |
| `command repeat`                   | Repeat last command once      |
| `command repeat three`             | Repeat last command 3 times   |

Works with number words (one-twenty) or digits. Handles common mishearings like "to"→2, "for"→4.

**Mishearing tolerance**: SS9K handles common Whisper transcription errors automatically:
- `caret` → also matches "carrot", "karet"
- `colon` → also matches "colin", "cologne"
- `asterisk` → also matches "asterix", "astrix"
- `tilde` → also matches "tilda", "squiggle"
- And many more built-in.

**Custom commands** (from config) work without a leader word.

**Tip:** Use aliases to shorten the leader: `"cmd" = "command"` → say "cmd enter"

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
leader = "command"           # leader word for commands (or "voice", "computer", etc.)
key_repeat_ms = 50           # key repeat rate for hold mode (ms between presses)
quiet = false                # suppress verbose output (set true once comfortable)

[commands]
"open terminal" = "kitty"
"open browser" = "$BROWSER"  # supports $ENV_VAR expansion
"screenshot" = "flameshot gui"
"workspace one" = "i3-msg 'workspace 1'"  # fuzzy matches "work space 1", "Workspace One", etc.

[aliases]
"taping" = "typing"          # fix consistent misrecognitions
"come and" = "command"       # common Whisper mishearing
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
