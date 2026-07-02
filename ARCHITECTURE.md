# Spoke Architecture

How Spoke works — the global pipeline shared by every platform, and the parts
that differ per platform.

---

## The global pipeline

Identical on macOS, Linux, and Windows:

```
global hotkey pressed
        ↓
cpal opens the microphone → PCM buffer (Vec<f32>, device native rate)
        ↓  (amplitude streamed to the UI for the bubble animation)
hotkey released
        ↓
downmix to mono → strip/compress silence → [optional: save WAV]
        ↓
STT engine (one of two, chosen by config):
   ├─ Whisper (offline)  — whisper.cpp via whisper-rs, 16 kHz mono
   └─ Google  (online)   — Speech-to-Text v1 REST, base64 LINEAR16
        ↓
transcript: String
        ↓
   ├─ enigo types it into the focused window   (default)
   └─ or copied to the clipboard               (copy_to_clipboard = true)
```

The UI is a single transparent always-on-top Tauri window ("the bubble"),
plain HTML/CSS/JS — no framework, no bundler. Rust and the UI talk over Tauri
commands (UI → Rust) and events (Rust → UI: state, amplitude, transcript,
download progress).

### Module map (`src-tauri/src/`)

| File | Responsibility |
|---|---|
| `lib.rs` | Glue: hotkey → capture → STT → injection; Tauri commands/events; model downloads |
| `platform.rs` | **Build-target description**: OS name, compiled backends, compile-time guards |
| `permissions.rs` | OS permission checks (macOS: mic TCC + Accessibility) feeding the UI warning banner |
| `config.rs` | `spoke.toml` schema, defaults, load/save |
| `audio.rs` | cpal capture thread, mono downmix, resampling, silence stripping, WAV save |
| `hotkey.rs` | `"ctrl+alt+space"` → global shortcut parsing |
| `inject.rs` | enigo keyboard-simulation injection |
| `stt/mod.rs` | `SttEngine` enum — one interface over both backends |
| `stt/whisper.rs` | whisper.cpp engine, model paths/URLs, CoreML bundle toggling |
| `stt/google.rs` | Google STT v1 REST client |

Cross-cutting details worth knowing:

- **Engine caching** — building the Whisper engine loads the whole model into
  RAM, so `SpokeState` caches it keyed on the engine-relevant config fields
  (`EngineKey`) and rebuilds only when those change.
- **Session counter** — every recording bumps an atomic counter; a pipeline
  checks it before injecting, so re-triggering cancels stale in-flight
  transcriptions instead of typing old text late.
- **Memory** — on glibc Linux, `malloc_trim` runs after each transcription to
  return freed heap pages to the OS (glibc arenas otherwise ratchet RSS up).

---

## The platform system

A Spoke binary is built **for exactly one platform** and contains only that
platform's technology. Three layers enforce and surface this:

### 1. Cargo features (build time)

`Cargo.toml` defines capability flags (`whisper`, `metal`, `coreml`, `cuda`,
`vulkan`) and platform presets (`platform-macos`, `platform-linux-cuda`, …)
that bundle them. Heavy dependencies are tied to the flags that need them —
`whisper-rs` and `futures-util` only exist in `whisper` builds, `zip` only in
`coreml` builds. A build without a flag contains none of that flag's code or
dependencies.

### 2. `platform.rs` (single source of truth)

- **Compile-time guards**: `compile_error!` rejects impossible combinations
  (Metal/CoreML off macOS, CUDA/Vulkan on macOS). A mis-targeted build fails
  loudly instead of shipping dead code.
- **Backend catalogue**: `compiled_backends()` returns the backends this
  binary actually contains, best first, always ending with the CPU fallback.
- **`build_info()`**: serializes OS name, whisper availability, the best
  backend, and the full backend list for the UI.

### 3. The UI (runtime)

On startup the UI calls `get_build_info` and renders from it:

- The **badge** shows the effective backend for the current setting.
- The **Accel** dropdown lists *Auto* + every compiled backend + *CPU only* —
  it never shows a backend the binary doesn't have.
- The **ANE bundle** row appears only in CoreML builds.

The user's choice is stored in `config.offline.accel`
(`"auto" | "metal" | "coreml" | "cuda" | "vulkan" | "none"`; the old
`mac_accel` name is still accepted when reading). A value from a different
build (e.g. a config written by a CUDA build opened in a Metal build) falls
back to *Auto*.

### How the choice takes effect

- **CPU vs GPU**: `accel = "none"` builds the Whisper context with
  `use_gpu = false`; anything else enables the compiled GPU backend.
- **CoreML vs Metal** (macOS): whisper.cpp uses a CoreML encoder bundle
  automatically if it sits next to the model file. Spoke toggles this by
  renaming the bundle to `.disabled` when the user selects Metal, and back
  when they select CoreML/Auto — a runtime switch with no rebuild.
- Switching accel (or model, or mode) invalidates the cached engine; the next
  transcription rebuilds it with the new settings.

### Adding a new backend

1. Add a cargo feature in `Cargo.toml` mapping to the `whisper-rs` feature
   (and a platform preset if it defines a new shipping target).
2. Add a `Backend` entry (+ OS guard if needed) in `platform.rs`.
3. Done — the UI dropdown, badge, and config handling pick it up from
   `build_info()`; no UI changes required.

---

## Per-platform technology

| | macOS | Linux | Windows |
|---|---|---|---|
| Audio capture (cpal) | CoreAudio | ALSA (works with PipeWire/PulseAudio) | WASAPI |
| Text injection (enigo) | CGEvent (needs Accessibility permission) | X11 / Wayland virtual keyboard | SendInput |
| Global hotkey | Carbon event tap (via tauri plugin) | X11 grab / compositor protocol | RegisterHotKey |
| Webview | WKWebView (built-in) | WebKitGTK | WebView2 |
| Whisper acceleration | CoreML (Neural Engine), Metal (GPU) | CUDA, Vulkan | CUDA, Vulkan |
| Config dir | `~/Library/Application Support/spoke/` | `~/.config/spoke/` | `%APPDATA%\spoke\` |
| Default hotkey | `cmd+shift+s` | `ctrl+alt+space` | `ctrl+alt+space` |
| Bundle formats | `.app`, `.dmg` | `.deb`, `.rpm`, `.AppImage` | `.msi`, `.exe` (NSIS) |

### Platform quirks handled in code

- **Linux/WebKitGTK**: `WEBKIT_DISABLE_COMPOSITING_MODE=1` is set to avoid
  blank transparent windows; on Wayland a 1 px "repaint nudge" resize forces
  the compositor to present panel updates.
- **Linux/ALSA**: device enumeration is filtered (no `hw:`/`dmix:` pseudo
  devices) and runs on a timeout thread, since misconfigured backends can
  block indefinitely. Capture prefers `pulse`/`pipewire`/`default`.
- **Linux/glibc**: `malloc_trim` after each transcription (see above).
- **macOS**: the bubble is marked visible on all Spaces; the private-API flag
  gives the transparent window proper behavior.
- **macOS permissions**: the UI polls `check_permissions` (AVCaptureDevice
  TCC status + `AXIsProcessTrusted`) and shows an amber `!` on the bubble plus
  a banner in the panel when Microphone or Accessibility is missing, with a
  *Fix* button that opens the right System Settings pane. Other platforms
  report `unknown` and never warn — extend `permissions.rs` if one grows a
  queryable API. The Accessibility warning is suppressed in clipboard mode,
  which doesn't inject keystrokes.

### Why the binary is self-contained

whisper.cpp and all GGML backends are **statically linked** into the Spoke
executable — there are no whisper/GGML shared libraries to bundle or install:

- **macOS**: Metal compute kernels are embedded in the binary
  (`GGML_METAL_EMBED_LIBRARY`); Metal/CoreML/Accelerate are OS frameworks.
  What a bundled app *does* need is permission metadata:
  `src-tauri/Info.plist` (microphone usage description — without it macOS
  silently denies mic access to the .app) and `src-tauri/entitlements.plist`
  (audio-input, for hardened-runtime signing).
- **Windows**: the MSVC C runtime is statically linked
  (`src-tauri/.cargo/config.toml` sets `+crt-static`), so no VC++
  Redistributable is required. CUDA builds link the CUDA runtime statically —
  users only need the normal NVIDIA driver.
- **Linux**: CUDA runtime static, same driver-only story; Vulkan uses the
  system loader (`libvulkan.so.1`, part of every desktop's GPU stack). GTK/
  WebKitGTK are declared as package dependencies in the `.deb`/`.rpm` and
  bundled into the `.AppImage`.

The only runtime artifacts are the models, which the app downloads itself.

---

## Model management

Models are **not** bundled into installers. Both downloads stream from
Hugging Face (`ggerganov/whisper.cpp`) with progress events to the UI:

- **GGML model** (`ggml-<name>.bin`) — required for offline mode. Downloaded
  to a `.tmp` file and renamed on completion, so an interrupted download can
  never masquerade as an installed model.
- **CoreML encoder bundle** (`ggml-<name>-encoder.mlmodelc`, macOS CoreML
  builds only) — downloaded as a zip and extracted next to the model
  (extraction validates entry paths against zip path traversal).

Lookup order at runtime: `src-tauri/models/` (dev convenience) first, then
`<config dir>/spoke/models/`. whisper.cpp finds the CoreML bundle by naming
convention — no path configuration.

Online mode needs no models: audio is sent as one batch REST request to
Google Speech-to-Text v1 with the API key from config.

---

## Configuration

One file, `spoke.toml`, in the OS config dir. Every field has a default, so a
missing or partial file always works. Engine-relevant fields (mode, model,
accel, use_gpu, provider, api_key) invalidate the cached engine on change;
the hotkey re-registers immediately on save. Schema lives in
`src-tauri/src/config.rs` with the documented sample in
[SPOKE.md](SPOKE.md#configuration).
