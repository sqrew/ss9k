# SuperScreecher9000

**Screech at your computer. It listens.**

A local, GPU-accelerated speech-to-text tool that types wherever your cursor is. Built for accessibility, privacy, and freedom.

## What It Does

Hold F12, speak, release. Your words appear at cursor. No cloud. No API keys. No subscriptions. Just you and your computer.

## Features

- **Local inference** - Whisper runs on YOUR machine
- **GPU accelerated** - Vulkan (Intel/AMD), CUDA (NVIDIA), Metal (macOS)
- **Types at cursor** - Works anywhere: editors, browsers, chat apps
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
# Run it
./target/release/ss9k

# Hold F12, speak, release
# Text appears at cursor
```

That's it. No config files. No setup wizards. Screech and go.

## Models

| Model | Size | Speed | Accuracy | Use Case |
|-------|------|-------|----------|----------|
| tiny | 75MB | Fastest | Basic | Quick notes, low-end hardware |
| base | 142MB | Fast | Good | General use |
| small | 466MB | Medium | Great | Recommended default |
| medium | 1.5GB | Slow | Excellent | When accuracy matters |
| large-v3 | 3GB | Slowest | Best | Maximum quality |

Download from: `https://huggingface.co/ggerganov/whisper.cpp/tree/main`

To change model, edit `MODEL_PATH` in `src/main.rs` (config file coming soon).

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

- [ ] Config file (model, hotkey, device selection)
- [ ] Audio feedback (beep on start/stop)
- [ ] Model auto-download on first run
- [ ] Streaming transcription (type as you speak)
- [ ] Command mode vs dictation mode
- [ ] Cross-platform releases (CI/CD)

## License

MIT. Do whatever you want.

## Credits

Built with:
- [whisper-rs](https://github.com/tazz4843/whisper-rs) - Whisper.cpp Rust bindings
- [cpal](https://github.com/RustAudio/cpal) - Cross-platform audio
- [rdev](https://github.com/Narsil/rdev) - Global hotkey capture
- [enigo](https://github.com/enigo-rs/enigo) - Keyboard simulation
- [rubato](https://github.com/HEnquist/rubato) - Audio resampling

Built by sqrew + Claude in a two-hour MVP session. The screech is real.
