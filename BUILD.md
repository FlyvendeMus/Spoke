# Building Spoke

## Prerequisites (all platforms)

- Rust 1.77+ (`rustup update`)
- Node.js 18+ (for Tauri CLI)
- CMake 3.20+ (required by whisper.cpp)
- C/C++ toolchain (gcc/clang)

Install Tauri CLI:
```sh
cargo install tauri-cli
```

---

## Feature flags

| Flag      | Description                                     | Platform        |
|-----------|-------------------------------------------------|-----------------|
| `whisper` | Enable offline transcription via whisper.cpp    | All             |
| `metal`   | GPU acceleration via Apple Metal                | macOS (M1/M2+)  |
| `coreml`  | Neural Engine acceleration via CoreML           | macOS (M1/M2+)  |
| `cuda`    | GPU acceleration via NVIDIA CUDA                | Linux / Windows |
| `vulkan`  | GPU acceleration via Vulkan (AMD/Intel/NVIDIA)  | Linux / Windows |

Flags are additive. Always pair a GPU flag with `whisper`:
```sh
--features whisper,metal
```

---

## macOS (Apple Silicon — M1/M2/M3)

### Option A: Metal (recommended, easiest)

Metal uses the Apple GPU directly. Zero extra setup — Xcode tools are sufficient.

**Prerequisites:**
```sh
xcode-select --install
```

**Dev build:**
```sh
cargo tauri dev --features whisper,metal
```

**Release build:**
```sh
cargo tauri build --features whisper,metal
```

The settings panel will show `Metal` under **Accel** when running.

---

### Option B: CoreML (fastest — uses Apple Neural Engine)

CoreML offloads the Whisper encoder to the Apple Neural Engine. Faster than
Metal for audio >3 s. The encoder bundle is downloaded automatically from
Hugging Face at runtime — no manual model conversion needed.

**Build with CoreML:**

```sh
cargo tauri build --features coreml
```

(`coreml` implies `whisper` — no need to specify both.)

**At runtime:**

1. Open the Spoke settings panel (click the bubble).
2. The **CoreML** row shows `—` and a **Download** button.
3. Click **Download** — Spoke fetches and extracts the encoder bundle
   (~100–800 MB depending on model) into
   `~/Library/Application Support/spoke/models/`.
4. Once complete the row shows `✓`. Subsequent launches use the cached bundle.

> **Note:** CoreML and Metal are mutually exclusive. CoreML takes priority
> in the feature detection order — if both are enabled, CoreML is reported.

---

## macOS (Intel)

Intel Macs have no Metal compute support for whisper.cpp. CPU only:

```sh
cargo tauri build --features whisper
```

---

## Linux (NVIDIA GPU — CUDA)

**Prerequisites:**
- CUDA Toolkit 11.8+ (`nvcc` on PATH)
- `libcuda.so` present

```sh
cargo tauri build --features whisper,cuda
```

Verify CUDA is found during build — if `nvcc` is missing, the build falls
back to CPU silently. Check build output for `GGML_CUDA=1`.

---

## Linux (AMD/Intel GPU — Vulkan)

**Prerequisites:**
```sh
# Debian/Ubuntu
sudo apt install libvulkan-dev vulkan-tools

# Arch
sudo pacman -S vulkan-devel
```

```sh
cargo tauri build --features whisper,vulkan
```

---

## Linux (CPU only)

```sh
cargo tauri build --features whisper
```

---

## Windows (NVIDIA GPU — CUDA)

**Prerequisites:**
- CUDA Toolkit 11.8+ from developer.nvidia.com
- Visual Studio 2022 Build Tools (MSVC)
- CMake 3.20+

```sh
cargo tauri build --features whisper,cuda
```

---

## Windows (CPU only)

Requires Visual Studio 2022 Build Tools + CMake:

```sh
cargo tauri build --features whisper
```

---

## Arch Linux full setup

See the detailed guide in README.md §"Arch Linux build & install".

---

## Checking acceleration at runtime

Open the Spoke settings panel (click the bubble). The **Accel** row shows
which backend was compiled in:

| Value            | Meaning                                          |
|------------------|--------------------------------------------------|
| `Metal`          | Apple GPU via Metal                              |
| `CoreML`         | Apple Neural Engine via CoreML                   |
| `CUDA`           | NVIDIA GPU via CUDA                              |
| `Vulkan`         | GPU via Vulkan (AMD/Intel/NVIDIA)                |
| `CPU`            | Whisper built without GPU support                |
| `CPU (no whisper)` | Whisper feature not compiled in (online only)  |

---

## CoreML implementation notes

CoreML encoder bundles are pre-converted and hosted on HuggingFace by the
whisper.cpp project. Spoke downloads and extracts the `.mlmodelc.zip` at
runtime via the **CoreML → Download** button in the settings panel.

The bundle is extracted to:
```
~/Library/Application Support/spoke/models/ggml-<model>-encoder.mlmodelc/
```

whisper.cpp discovers it automatically by convention (strips `.bin`, appends
`-encoder.mlmodelc`), so no path config is required.

Remaining tasks:
- [ ] Validate `.mlmodelc` existence at startup when built with `--features coreml`
      and emit a clear error to the UI if missing (rather than a cryptic crash)
- [ ] Support bundling a pre-downloaded `.mlmodelc` in `tauri.conf.json` resources
      for fully offline distribution
