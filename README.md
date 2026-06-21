# Spoke — MVP

Cross-platform voice-to-text dictation for the desktop. Hold a hotkey, speak,
release — the transcript is typed into whatever has focus. See [SPOKE.md](SPOKE.md)
for the full specification.

## Stack

- **Rust + Tauri v2** core (`src-tauri/`)
- **Vanilla HTML/CSS/JS** UI (`ui/`) — no framework, no bundler, no Node build step
- **cpal** capture · **enigo** injection · **global-shortcut** hotkeys
- STT: **Google Speech-to-Text v1** (online, default-buildable) and
  **whisper.cpp** (offline, opt-in via the `whisper` feature)

## Build & Install — Arch Linux

### 1. System dependencies

```sh
sudo pacman -S --needed \
    base-devel curl wget git \
    webkit2gtk-4.1 gtk3 libappindicator-gtk3 librsvg \
    openssl pkgconf \
    alsa-lib pipewire-alsa
```

For offline Whisper mode, also install:

```sh
sudo pacman -S cmake
```

### 2. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 3. Tauri CLI

```sh
cargo install tauri-cli --version "^2"
```

### 4. Build

**Online mode (Google STT):**
```sh
cargo tauri build
```

**Offline mode (local Whisper):**
```sh
cargo tauri build --features whisper
```

With GPU acceleration (pick one):
```sh
cargo tauri build --features whisper,cuda    # NVIDIA
cargo tauri build --features whisper,vulkan  # AMD/Intel
```

Build output: `src-tauri/target/release/bundle/`

### 5. Install

```sh
# Binary
sudo install -Dm755 src-tauri/target/release/bundle/deb/Spoke_0.1.0_amd64/data/usr/bin/spoke \
    /usr/local/bin/spoke

# App launcher (.desktop)
sudo install -Dm644 src-tauri/target/release/bundle/deb/Spoke_0.1.0_amd64/data/usr/share/applications/Spoke.desktop \
    /usr/share/applications/Spoke.desktop

# Icons
sudo install -Dm644 src-tauri/target/release/bundle/deb/Spoke_0.1.0_amd64/data/usr/share/icons/hicolor/256x256@2/apps/spoke.png \
    /usr/share/icons/hicolor/256x256/apps/spoke.png
sudo install -Dm644 src-tauri/target/release/bundle/deb/Spoke_0.1.0_amd64/data/usr/share/icons/hicolor/128x128/apps/spoke.png \
    /usr/share/icons/hicolor/128x128/apps/spoke.png
sudo install -Dm644 src-tauri/target/release/bundle/deb/Spoke_0.1.0_amd64/data/usr/share/icons/hicolor/32x32/apps/spoke.png \
    /usr/share/icons/hicolor/32x32/apps/spoke.png

# Refresh icon cache
sudo gtk-update-icon-cache -f /usr/share/icons/hicolor/
```

Spoke now appears in your application launcher and runs as `spoke` from the terminal.

### Wayland note

Global hotkeys and text injection work best on X11. On Wayland (e.g. Hyprland, sway), hotkeys require a compositor that supports `zwp_virtual_keyboard_v1`. If the hotkey doesn't register, launch with:

```sh
GDK_BACKEND=x11 spoke
```

### Offline model setup (Whisper builds only)

Download a ggml model into `src-tauri/models/` before building, or place it at
`~/.config/spoke/models/ggml-<name>.bin` at runtime. The model name must match
the `model` field in `~/.config/spoke/spoke.toml`.

---

## Prerequisites (other platforms)

- Rust (stable) — https://rustup.rs
- The Tauri CLI:
  ```sh
  cargo install tauri-cli --version "^2"
  ```
- Platform deps for Tauri v2 (WebKit/GTK on Linux, WebView2 on Windows, Xcode CLT on macOS).
- For offline mode only: `cmake` + a C/C++ toolchain (whisper.cpp builds from source).

## Run (dev)

```sh
cargo tauri dev
```

The UI is static, so there is no separate frontend dev server — Tauri serves
`ui/` directly.

## Build (release)

```sh
cargo tauri build
```

## Offline (Whisper) mode

Offline transcription is feature-gated to keep the default build light:

```sh
cargo tauri build --features whisper
```

Download a ggml model into `src-tauri/models/` named `ggml-<model>.bin`
(e.g. `ggml-large-v3-turbo.bin`) — matching the `model` value in the config.

## Online (Google) mode

Set `mode = "online"` and paste a Google Cloud API key (with the Speech-to-Text
API enabled) into the settings panel. The key is stored in `spoke.toml` during
development; production builds should move it to the system keychain.

## Configuration

`spoke.toml` lives in the OS config dir
(`~/Library/Application Support/spoke/` on macOS, `~/.config/spoke/` on Linux,
`%APPDATA%\spoke\` on Windows). It is created on first save; every field has a
default. Schema is documented in [SPOKE.md](SPOKE.md#configuration).

## Platform permissions

- **macOS** — grant **Microphone** and **Accessibility** permissions (the
  latter lets enigo synthesize keystrokes). System Settings → Privacy & Security.
- **Linux (Wayland)** — global hotkeys and injection depend on the compositor's
  support; X11 is the most reliable.

## Tests

```sh
cd src-tauri && cargo test --lib
```

Unit tests cover config (de)serialization, audio mono/resample/PCM conversion,
hotkey parsing, and Google response parsing — none require audio hardware or
network.

## Icons

Regenerate the app icons (pure Python, no deps):

```sh
python3 src-tauri/scripts/gen_icons.py
```
