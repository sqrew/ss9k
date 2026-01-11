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
- **Hot-reload config** - Change settings without restarting
- **Multiple models** - tiny (75MB) to large (3GB), pick your speed/accuracy tradeoff
- **Cross-platform ready** - Built with portable Rust crates

## Installation

```bash
# Clone and build
git clone https://github.com/sqrew/ss9k
cd ss9k
cargo build --release

# Download a model (pick one)
mkdir -p models
curl -L -o models/ggml-small.bin "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin"
```

## Usage

```bash
# Run it (downloads model on first run)
./target/release/ss9k

# Hold your hotkey (F12 default), speak, release
# Text appears at cursor
```

### Voice Commands

Built-in commands (just say them):
- **Navigation**: "enter", "tab", "escape", "backspace", "space"
- **Editing**: "select all", "copy", "paste", "cut", "undo", "redo", "save"
- **Media**: "play", "pause", "next", "previous", "volume up", "volume down", "mute"

### Configuration

Create `~/.config/ss9k/config.toml`:

```toml
model = "small"           # tiny, base, small, medium, large
language = "en"           # ISO 639-1 code
hotkey = "ScrollLock"     # F1-F12, ScrollLock, Pause, etc.
hotkey_mode = "hold"      # hold or toggle

[commands]
"open terminal" = "kitty"
"open browser" = "firefox"
"screenshot" = "flameshot gui"

[aliases]
"taping" = "typing"       # fix consistent misrecognitions
```

Config hot-reloads when you save - no restart needed.

## Models

| Model | Size | Speed | Accuracy | Use Case |
|-------|------|-------|----------|----------|
| tiny | 75MB | Fastest | Basic | Quick notes, low-end hardware |
| base | 142MB | Fast | Good | General use |
| small | 466MB | Medium | Great | Recommended default |
| medium | 1.5GB | Slow | Excellent | When accuracy matters |
| large-v3 | 3GB | Slowest | Best | Maximum quality |

Download from: `https://huggingface.co/ggerganov/whisper.cpp/tree/main`

Or just run SS9K - it auto-downloads the configured model on first launch.

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

5GB for a fully offline accessibility tool is nothing. Your voice stays on your machine.

## Roadmap

- [x] Config file (model, hotkey, device selection)
- [x] Model auto-download on first run
- [x] Voice commands (navigation, editing, media)
- [x] Custom command mapping
- [x] Hot-reload config
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
