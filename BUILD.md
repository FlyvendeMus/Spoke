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

## Windows — Step-by-step setup

Building on Windows requires several tools. Run each verification command to confirm the tool is on your `PATH` before proceeding.

### 1. Visual Studio 2022 Build Tools (MSVC)

Required for Rust's MSVC target and the C/C++ compiler that whisper.cpp needs.

1. Download from https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022
2. In the installer, select **Desktop development with C++**
3. Under **Individual components**, ensure **Windows 10/11 SDK** is included
4. Install and restart

**Verify:**
```powershell
cl.exe
```
Should print the MSVC compiler version.

---

### 2. CMake

Required by whisper.cpp to generate build files.

1. Download from https://cmake.org/download/ (Windows x64 Installer)
2. During install, **check "Add CMake to the system PATH"**
3. Install and restart your terminal

**Verify:**
```powershell
cmake --version
```

If you missed the PATH option during install, you can add it manually:
- Add `C:\Program Files\CMake\bin` to your system or user `Path` environment variable, or
- Run this before building (session-only):
  ```powershell
  $env:Path += ";C:\Program Files\CMake\bin"
  ```

---

### 3. LLVM / Clang (libclang)

Required by Rust's `bindgen` crate to parse C headers. Without it the build fails with:
```
Unable to find libclang: "couldn't find any valid shared libraries matching: ['clang.dll', 'libclang.dll']"
```

1. Download the latest LLVM release from https://github.com/llvm/llvm-project/releases (grab `LLVM-<version>-win64.exe`)
2. Run the installer — **check "Add LLVM to the system PATH"**
3. Install and restart your terminal

**Verify:**
```powershell
clang --version
```

Then set the environment variable that bindgen reads:

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
```

To make this permanent, add a system/user environment variable named `LIBCLANG_PATH` with value `C:\Program Files\LLVM\bin`.

---

### 4. Rust (with rustfmt)

```powershell
rustup update
rustup component add rustfmt
```

**Verify:**
```powershell
rustc --version
rustfmt --version
```

---

### 5. Tauri CLI

```powershell
cargo install tauri-cli
```

---

### 6. Node.js

Required by Tauri for webview bundling. Download from https://nodejs.org/ (18+).

**Verify:**
```powershell
node --version
```

---

### 7a. CUDA Toolkit (for NVIDIA GPU acceleration)

1. Download CUDA Toolkit 11.8+ from https://developer.nvidia.com/cuda-downloads
2. Run the installer
3. The installer adds CUDA to your `PATH` automatically

**Verify:**
```powershell
nvcc --version
```

> **Note:** End users do **not** need any of these build tools — they only need the NVIDIA GPU driver for CUDA-accelerated builds.

---

### 7b. Vulkan SDK (alternative to CUDA)

If you prefer not to install CUDA, or have a non-NVIDIA GPU, use Vulkan instead:

1. Download the Vulkan SDK from https://vulkan.lunang.com
2. Install and restart

```powershell
cargo tauri build --features whisper,vulkan
```

---

### Build

**CUDA (NVIDIA GPU):**
```powershell
cargo tauri build --features whisper,cuda
```

**Vulkan (any GPU):**
```powershell
cargo tauri build --features whisper,vulkan
```

**CPU only:**
```powershell
cargo tauri build --features whisper
```

**Dev build** (same flags, live-reload):
```powershell
cargo tauri dev --features whisper,cuda
```

### Troubleshooting

| Error | Likely fix |
|-------|------------|
| `is cmake not installed?` | Install CMake and add to PATH |
| `Unable to find libclang` | Install LLVM, set `LIBCLANG_PATH` |
| `called Result::unwrap() on an Err value: NotPresent` (CUDA libs) | Install CUDA Toolkit or switch to `vulkan` |
| `rustfmt.exe is not installed` | Run `rustup component add rustfmt` |

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
