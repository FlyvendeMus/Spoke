# Spoke

**Talk instead of type.** Hold a hotkey, say what you want to write, release — and the words appear wherever your cursor is. Any app, any text field: browsers, editors, chat, terminals.

Spoke is a small floating bubble that lives in the corner of your screen. It works the same way on **macOS**, **Windows**, and **Linux**.

## How it works

1. **Hold the hotkey** (default: `Cmd+Shift+S` on Mac, `Ctrl+Alt+Space` elsewhere) — the microphone opens.
2. **Speak.**
3. **Release** — a second later your words are typed into whatever has focus.

Prefer tap-to-start / tap-to-stop? Switch the trigger to *Toggle* in settings. Prefer the text on your clipboard instead of typed out? Turn on *Copy to clipboard*.

## Private by default

Spoke transcribes speech **on your own computer** using [Whisper](https://github.com/ggerganov/whisper.cpp). No account, no subscription, no audio leaving your machine, works offline.

The first time you open settings, pick a model and click **Get** — Spoke downloads it for you:

| Model | Size | Good for |
|---|---|---|
| tiny | 75 MB | Very old or low-power machines |
| base | 145 MB | Fast, decent accuracy |
| small | 465 MB | Balanced |
| large-v3-turbo | 809 MB | Best accuracy — the default |

If you'd rather use a cloud service, switch **Mode** to *Online* and paste a Google Cloud Speech-to-Text API key. Online mode is faster on weak hardware but sends audio to Google.

## Fast on every machine

Spoke uses whatever your hardware is best at. Each build is made for one platform and ships only what that platform needs:

| Platform | Acceleration |
|---|---|
| macOS (Apple Silicon) | Neural Engine (CoreML), GPU (Metal), or CPU |
| Linux | NVIDIA GPU (CUDA), any GPU (Vulkan), or CPU |
| Windows | NVIDIA GPU (CUDA), any GPU (Vulkan), or CPU |

The settings panel shows which one is active (the small badge next to the version number). You can change it anytime — no reinstall, no restart.

## Get Spoke

Spoke is currently built from source. It takes one command per platform once the toolchain is installed — the full walkthrough for **macOS**, **Debian/Ubuntu**, **Arch Linux**, and **Windows** is in **[BUILD.md](BUILD.md)**.

The build produces a normal installer for your system (`.dmg`/`.app` on macOS, `.deb`/`.rpm`/`.AppImage` on Linux, `.msi`/`.exe` on Windows). Everything else — the speech models — Spoke downloads itself when you ask it to.

## First run

- **macOS**: grant **Microphone** and **Accessibility** permissions when prompted (System Settings → Privacy & Security). Accessibility is what lets Spoke type for you. If you rebuild Spoke yourself, macOS treats each rebuild as a new app — re-grant both permissions after replacing the app.
- **Linux on Wayland**: global hotkeys depend on your compositor. If the hotkey doesn't respond, launch with `GDK_BACKEND=x11 spoke`.
- **Everywhere**: click the bubble to open settings, download a model, and you're set.

If a permission is missing on macOS, the bubble shows an amber **!** — click it and use the **Fix** button in the warning banner to jump straight to the right System Settings pane.

## The bubble

- **Dim** — idle, waiting.
- **Warm glow, reacting to your voice** — recording.
- **Teal shimmer** — transcribing.

Drag it anywhere. Click it for settings: mode, hotkey, language, model, acceleration, microphone, transcript history, and optional recording-to-file.

## Where things live

| What | Where |
|---|---|
| Settings (`spoke.toml`) | `~/Library/Application Support/spoke/` (macOS) · `~/.config/spoke/` (Linux) · `%APPDATA%\spoke\` (Windows) |
| Downloaded models | `<config dir>/spoke/models/` |
| Saved recordings (optional) | `~/Documents/Spoke` (configurable) |

## More documentation

- **[BUILD.md](BUILD.md)** — building and packaging for each platform
- **[ARCHITECTURE.md](ARCHITECTURE.md)** — how Spoke works inside, globally and per platform
- **[SPOKE.md](SPOKE.md)** — the original product specification
