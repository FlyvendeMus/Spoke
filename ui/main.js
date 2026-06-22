const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const $ = (id) => document.getElementById(id);

const bubble = $("bubble");
const panel = $("panel");
const ring = $("ring");
const status = $("status");
const hotkeyDisplay = $("hotkey");
const recordBtn = $("recordHotkey");
const closeBtn = $("closePanel");

const accelBadge = $("accelBadge");

const fields = {
  mode: $("mode"),
  trigger: $("trigger"),
  language: $("language"),
  model: $("model"),
  macAccel: $("macAccel"),
  apikey: $("apikey"),
  saveAudio: $("saveAudio"),
  savePath: $("savePath"),
  saveMode: $("saveMode"),
  micDevice: $("micDevice"),
};

let config = null;
let amplitude = 0;
let recording = false;
let modelDownloading = false;
let coremlDownloading = false;
let buildInfo = null;

// ── Mosaic canvas setup ───────────────────────────────────────────────────
const S   = 48;
const dpr = Math.min(window.devicePixelRatio || 1, 2);
ring.width  = S * dpr;
ring.height = S * dpr;
const mCtx  = ring.getContext('2d');
mCtx.scale(dpr, dpr);

const COLS = 13;
const ROWS = 13;
const cellW = S / COLS;
const cellH = S / ROWS;
const GAP   = 1.2;

const MOSAIC_PALETTES = {
  idle:       { bg: [16, 10, 44],  fg: [130, 100, 255] },
  listening:  { bg: [44, 10, 22],  fg: [255, 108,  75] },
  processing: { bg: [ 8, 32, 44],  fg: [ 38, 212, 196] },
};

let mosaicT    = 0;
let mosaicPrev = performance.now();
let smoothedAmp = 0;

let fromState  = 'idle';
let toState    = 'idle';
let transP     = 1;
const TRANS_DUR = 0.55;

// Reused per-frame color buffers (see drawMosaic) — module-level to avoid
// allocating on every animation frame.
const bbg = [0, 0, 0];
const bfg = [0, 0, 0];

// Idle ambient animation is throttled to this rate; listening/processing run at
// full refresh for audio-reactive smoothness. Continuous 60fps full-canvas
// redraws when idle are the main driver of WebKit web-process memory/CPU.
const IDLE_INTERVAL = 1000 / 20;
let lastFrame = 0;

// ---- Panel close ---------------------------------------------------------

// On Wayland, WebKitGTK does not re-commit a transparent, unfocused window's
// surface on content change, so the panel's DOM state flips but the pixels do not
// update until an unrelated ~20s refresh. `nudge_repaint` resizes the window by
// 1px, forcing a surface reconfigure + commit that presents the new frame.
function closePanel() {
  panel.classList.add("hidden");
  invoke("nudge_repaint");
}

function openPanel() {
  panel.classList.remove("hidden");
  invoke("nudge_repaint");
}

// Use pointerdown (fires before click) for reliability on Linux/GTK.
closeBtn.addEventListener("pointerdown", (e) => {
  e.stopPropagation();
  e.preventDefault();
  closePanel();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !panel.classList.contains("hidden")) {
    closePanel();
  }
});

// Close panel when clicking outside both the panel and the bubble.
document.addEventListener("pointerdown", (e) => {
  if (panel.classList.contains("hidden")) return;
  if (!panel.contains(e.target) && !bubble.contains(e.target)) {
    closePanel();
  }
});

// ---- Config <-> form ----------------------------------------------------

function formFromConfig(c) {
  fields.mode.value = c.general.mode;
  fields.trigger.value = c.general.trigger;
  fields.language.value = c.general.language;
  fields.model.value = c.offline.model;
  if (c.offline.mac_accel) fields.macAccel.value = c.offline.mac_accel;
  fields.apikey.value = c.online.api_key;
  fields.saveAudio.checked = c.recording.save_audio;
  fields.savePath.value = c.recording.save_path;
  fields.saveMode.value = c.recording.save_processed ? "processed" : "original";
  if (c.recording.input_device && fields.micDevice.querySelector(`option[value="${c.recording.input_device}"]`)) {
    fields.micDevice.value = c.recording.input_device;
  }
  hotkeyDisplay.textContent = c.general.hotkey || "—";
  applyModeVisibility(c.general.mode);
}

function configFromForm() {
  const c = structuredClone(config);
  c.general.mode = fields.mode.value;
  c.general.trigger = fields.trigger.value;
  c.general.language = fields.language.value;
  c.offline.model = fields.model.value;
  c.offline.mac_accel = fields.macAccel.value;
  c.online.api_key = fields.apikey.value;
  c.recording.save_audio = fields.saveAudio.checked;
  c.recording.save_path = fields.savePath.value.trim();
  c.recording.save_processed = fields.saveMode.value === "processed";
  c.recording.input_device = fields.micDevice.value;
  c.general.hotkey = config.general.hotkey;
  return c;
}

function applyModeVisibility(mode) {
  const offline = mode === "offline";
  document
    .querySelectorAll(".offline-only")
    .forEach((el) => el.classList.toggle("hide", !offline));
  document
    .querySelectorAll(".online-only")
    .forEach((el) => el.classList.toggle("hide", offline));
}

async function pushConfig() {
  config = configFromForm();
  applyModeVisibility(config.general.mode);
  try {
    await invoke("set_config", { newConfig: config });
    flash("Saved");
  } catch (e) {
    flash(String(e));
  }
}

let flashTimer = null;
function flash(msg) {
  status.textContent = msg;
  clearTimeout(flashTimer);
  flashTimer = setTimeout(() => (status.textContent = ""), 2500);
}

// ---- Microphone device list ---------------------------------------------

async function populateMicDevices() {
  try {
    const devices = await invoke("list_audio_devices");
    const sel = fields.micDevice;
    sel.innerHTML = "";
    const def = document.createElement("option");
    def.value = "";
    def.textContent = "Default";
    sel.appendChild(def);
    for (const d of devices) {
      const opt = document.createElement("option");
      opt.value = d;
      opt.textContent = d;
      sel.appendChild(opt);
    }
  } catch (e) {
    console.error("Failed to list audio devices:", e);
  }
}

// ---- Hotkey recorder ----------------------------------------------------

function keyName(code) {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-2])$/.test(code)) return code.toLowerCase();
  const map = {
    Space: "space",
    Enter: "enter",
    Return: "enter",
    Tab: "tab",
    Escape: "esc",
  };
  return map[code] || null;
}

function comboFromEvent(e) {
  const mods = [];
  if (e.metaKey) mods.push("cmd");
  if (e.ctrlKey) mods.push("ctrl");
  if (e.altKey) mods.push("alt");
  if (e.shiftKey) mods.push("shift");
  const key = keyName(e.code);
  if (!key) return null;
  return [...mods, key].join("+");
}

let capturing = false;

function startCapture() {
  if (capturing) return;
  capturing = true;
  recordBtn.classList.add("recording");
  recordBtn.textContent = "Press keys…";
  hotkeyDisplay.classList.add("listening");
  window.addEventListener("keydown", onCaptureKey, true);
}

function endCapture() {
  capturing = false;
  recordBtn.classList.remove("recording");
  recordBtn.textContent = "Record";
  hotkeyDisplay.classList.remove("listening");
  window.removeEventListener("keydown", onCaptureKey, true);
}

async function onCaptureKey(e) {
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape" && !e.metaKey && !e.ctrlKey && !e.altKey) {
    endCapture();
    return;
  }
  const combo = comboFromEvent(e);
  if (!combo) return;
  config.general.hotkey = combo;
  hotkeyDisplay.textContent = combo;
  endCapture();
  await pushConfig();
}

recordBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  startCapture();
});

// ---- Bubble state -------------------------------------------------------

function setBubbleState(state, message) {
  const wasDragging = bubble.classList.contains("dragging");
  bubble.className = state;
  if (wasDragging) bubble.classList.add("dragging");
  recording = state === "recording";
  if (state === "error" && message) flash(message);
}

function waveV(state, nx, ny, dist, angle, hash, col) {
  if (state === 'idle') {
    return 0.22
      + 0.18 * Math.sin(mosaicT * 0.55 + nx * 4.0 + ny * 2.5 + hash * 1.5)
      + 0.08 * Math.sin(mosaicT * 0.40 + nx * 1.5 - ny * 3.0 + hash * 2.0);
  } else if (state === 'listening') {
    const r1    = Math.sin(mosaicT * 2.8  - dist * 4.5);
    const r2    = Math.sin(mosaicT * 3.10 - dist * 5.5 + 2.1);
    const flick = Math.abs(Math.sin(mosaicT * 5.5 + col * 1.1))
                * Math.max(0, 1 - dist * 1.6) * 0.10;
    const bloom = smoothedAmp * Math.max(0, 1 - dist * 1.8) * 0.55;
    return 0.08
      + Math.max(0, r1) * (0.42 + smoothedAmp * 0.22)
      + Math.max(0, r2) * (0.24 + smoothedAmp * 0.14)
      + flick
      + bloom;
  } else {
    const wA = Math.sin(mosaicT * 1.6 + angle * 3 + dist * 6);
    const wB = Math.sin(mosaicT * 2.4 - dist * 8  + angle * 2);
    return 0.10
      + (wA * 0.5 + 0.5) * 0.38
      + (wB * 0.5 + 0.5) * 0.30;
  }
}

function drawMosaic(now) {
  const dt   = Math.min((now - mosaicPrev) / 1000, 0.05);
  mosaicT   += dt;
  mosaicPrev = now;

  // Envelope follower: fast attack (~100ms), slow release (~800ms)
  smoothedAmp += (amplitude - smoothedAmp) * (amplitude > smoothedAmp ? 0.20 : 0.04);
  amplitude   *= 0.9;

  // Detect state change → start transition
  const ms = bubble.classList.contains('recording') ? 'listening'
           : bubble.classList.contains('processing') ? 'processing'
           : 'idle';
  if (ms !== toState) {
    fromState = toState;
    toState   = ms;
    transP    = 0;
  }

  // Advance + ease transition progress
  if (transP < 1) transP = Math.min(1, transP + dt / TRANS_DUR);
  const ease = transP < 0.5
    ? 2 * transP * transP
    : -1 + (4 - 2 * transP) * transP;

  // Blend palettes once per frame. Reuse module-level arrays (bbg/bfg) instead
  // of allocating two new arrays every frame — avoids GC churn that inflates the
  // WebKit web process heap under the continuous animation loop.
  const pF = MOSAIC_PALETTES[fromState];
  const pT = MOSAIC_PALETTES[toState];
  bbg[0] = pF.bg[0] + (pT.bg[0] - pF.bg[0]) * ease;
  bbg[1] = pF.bg[1] + (pT.bg[1] - pF.bg[1]) * ease;
  bbg[2] = pF.bg[2] + (pT.bg[2] - pF.bg[2]) * ease;
  bfg[0] = pF.fg[0] + (pT.fg[0] - pF.fg[0]) * ease;
  bfg[1] = pF.fg[1] + (pT.fg[1] - pF.fg[1]) * ease;
  bfg[2] = pF.fg[2] + (pT.fg[2] - pF.fg[2]) * ease;

  mCtx.fillStyle = `rgb(${bbg[0]|0},${bbg[1]|0},${bbg[2]|0})`;
  mCtx.fillRect(0, 0, S, S);

  for (let row = 0; row < ROWS; row++) {
    for (let col = 0; col < COLS; col++) {
      const nx    = col / (COLS - 1);
      const ny    = row / (ROWS - 1);
      const dx    = nx - 0.5;
      const dy    = ny - 0.5;
      const dist  = Math.sqrt(dx * dx + dy * dy) * Math.SQRT2;
      const angle = Math.atan2(dy, dx);
      const hash  = ((col * 7 + row * 13) % 31) / 31;

      const vF = Math.min(1, Math.max(0, waveV(fromState, nx, ny, dist, angle, hash, col)));
      const vT = Math.min(1, Math.max(0, waveV(toState,   nx, ny, dist, angle, hash, col)));
      const v  = vF + (vT - vF) * ease;

      const ir = Math.round(bbg[0] + (bfg[0] - bbg[0]) * v);
      const ig = Math.round(bbg[1] + (bfg[1] - bbg[1]) * v);
      const ib = Math.round(bbg[2] + (bfg[2] - bbg[2]) * v);

      const sz = 0.68 + v * 0.32;
      const cw = (cellW - GAP) * sz;
      const ch = (cellH - GAP) * sz;

      mCtx.fillStyle = `rgb(${ir},${ig},${ib})`;
      mCtx.fillRect(
        col * cellW + GAP / 2 + (cellW - GAP - cw) / 2,
        row * cellH + GAP / 2 + (cellH - GAP - ch) / 2,
        cw, ch
      );
    }
  }
}

// Animation driver: always schedules the next frame, but skips the actual draw
// when the window is hidden (occluded/minimized) and throttles to IDLE_INTERVAL
// while idle. During state transitions and listening/processing it runs at full
// refresh so the audio-reactive visuals stay smooth.
function tick(now) {
  requestAnimationFrame(tick);
  if (document.hidden) return;
  const idle = toState === 'idle' && transP >= 1;
  if (idle && now - lastFrame < IDLE_INTERVAL) return;
  lastFrame = now;
  drawMosaic(now);
}

// ---- Events from Rust ---------------------------------------------------

listen("spoke:state", (e) => {
  const { state, message } = e.payload;
  setBubbleState(state, message);
});

listen("spoke:amplitude", (e) => {
  amplitude = Math.min(1, Math.sqrt(e.payload) * 1.6);
});

// ---- Dragging the bubble ------------------------------------------------

let dragOrigin = null;
let didDrag = false;
let micDevicesPopulated = false;

// Use one event model (Pointer Events) for both drag and toggle. Mixing mouse
// events for drag with pointer events for the toggle desyncs on WebKitGTK
// transparent/unfocused windows: mouseup may not fire, leaving dragOrigin/didDrag
// stale so the toggle's `if (didDrag) return` swallows the close.
bubble.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  // Stop propagation so the document "close on outside click" handler does not
  // run when the user clicks the bubble itself — toggle is handled below.
  e.stopPropagation();
  dragOrigin = { x: e.screenX, y: e.screenY };
  didDrag = false;
});

window.addEventListener("pointermove", (e) => {
  if (!dragOrigin) return;
  const dx = e.screenX - dragOrigin.x;
  const dy = e.screenY - dragOrigin.y;
  if (Math.hypot(dx, dy) > 5) {
    didDrag = true;
    dragOrigin = null;
    bubble.classList.add("dragging");
    appWindow.startDragging();
  }
});

bubble.addEventListener("pointerup", (e) => {
  if (e.button !== 0) return;
  // Always reset drag state on release so it can never go stale.
  dragOrigin = null;
  bubble.classList.remove("dragging");
  if (didDrag) {
    didDrag = false;
    return;
  }
  if (panel.classList.contains("hidden")) {
    openPanel();
    if (!micDevicesPopulated) {
      micDevicesPopulated = true;
      populateMicDevices();
    }
  } else {
    closePanel();
  }
});

// Safety net: if startDragging() swallows the pointerup (WM grabs the gesture),
// clear drag state when the pointer leaves so the next click is never blocked.
window.addEventListener("pointercancel", () => {
  dragOrigin = null;
  didDrag = false;
  bubble.classList.remove("dragging");
});

// ---- Form wiring + boot -------------------------------------------------

for (const el of Object.values(fields)) {
  if (el.id !== "model" && el.id !== "macAccel") {
    el.addEventListener("change", pushConfig);
  }
}

fields.model.addEventListener("change", async () => {
  await pushConfig();
  if (!modelDownloading) checkCurrentModel();
});

fields.macAccel.addEventListener("change", async () => {
  applyCoremlRowVisibility();
  await pushConfig();
  if (!modelDownloading) checkCurrentModel();
});

async function loadBuildInfo() {
  try {
    buildInfo = await invoke("get_build_info");
    const label = buildInfo.whisper ? buildInfo.acceleration : "CPU (no whisper)";
    accelBadge.textContent = label;
    accelBadge.dataset.accel = buildInfo.acceleration;

    // Show GPU selector on macOS whenever any GPU feature compiled in.
    if (buildInfo.is_macos && buildInfo.mac_options && buildInfo.mac_options.length > 1) {
      const sel = fields.macAccel;
      const valid = new Set([...buildInfo.mac_options, "none"]);
      // Remove options not supported by this build.
      for (const opt of Array.from(sel.options)) {
        if (!valid.has(opt.value)) opt.remove();
      }
      // Default selection: prefer metal, then coreml, else none.
      if (!valid.has(sel.value)) {
        sel.value = buildInfo.has_metal ? "metal" : buildInfo.has_coreml ? "coreml" : "none";
      }
      $("gpuRow").classList.remove("hide");
    }

    applyCoremlRowVisibility();
  } catch (e) {
    accelBadge.textContent = "unknown";
  }
}

function applyCoremlRowVisibility() {
  const show = buildInfo && buildInfo.has_coreml &&
    (fields.macAccel.value === "coreml" || fields.macAccel.value === "auto");
  $("coremlRow").classList.toggle("hide", !show);
}

// ---- Model download --------------------------------------------------

async function checkCurrentModel() {
  const model = fields.model.value;
  const statusEl = $("modelStatus");
  const dlBtn = $("downloadModel");
  statusEl.textContent = "…";
  statusEl.className = "model-status";
  dlBtn.classList.add("hide");
  try {
    const info = await invoke("check_model", { model });
    if (info.exists) {
      statusEl.textContent = "✓";
      statusEl.className = "model-status installed";
      dlBtn.classList.add("hide");
    } else {
      statusEl.textContent = "—";
      statusEl.className = "model-status";
      dlBtn.classList.remove("hide");
    }
    // Update CoreML bundle status if that row is visible.
    if (buildInfo && buildInfo.has_coreml && !$("coremlRow").classList.contains("hide")) {
      updateCoremlStatus(info.coreml_exists);
    }
  } catch (_) {
    statusEl.textContent = "—";
    statusEl.className = "model-status";
    dlBtn.classList.add("hide");
  }
}

function updateCoremlStatus(exists) {
  const statusEl = $("coremlStatus");
  const dlBtn = $("downloadCoreml");
  if (exists) {
    statusEl.textContent = "✓";
    statusEl.className = "model-status installed";
    dlBtn.classList.add("hide");
  } else {
    statusEl.textContent = "—";
    statusEl.className = "model-status";
    dlBtn.classList.remove("hide");
  }
}

$("downloadCoreml").addEventListener("click", async () => {
  if (coremlDownloading) return;
  const model = fields.model.value;
  const statusEl = $("coremlStatus");
  const dlBtn = $("downloadCoreml");
  coremlDownloading = true;
  dlBtn.disabled = true;
  dlBtn.textContent = "…";
  dlBtn.classList.remove("hide");
  statusEl.textContent = "0%";
  statusEl.className = "model-status downloading";
  fields.model.disabled = true;
  try {
    await invoke("download_coreml_bundle", { model });
  } catch (e) {
    statusEl.textContent = "✗";
    statusEl.className = "model-status error";
    dlBtn.disabled = false;
    dlBtn.textContent = "Download";
    coremlDownloading = false;
    fields.model.disabled = false;
    flash(String(e));
  }
});

listen("spoke:coreml-progress", (e) => {
  if (!coremlDownloading) return;
  const { model, percent, phase } = e.payload;
  if (model !== fields.model.value) return;
  const statusEl = $("coremlStatus");
  statusEl.className = "model-status downloading";
  statusEl.textContent = phase === "extract" ? "unzip…" : `${percent}%`;
});

listen("spoke:coreml-complete", (e) => {
  if (!coremlDownloading) return;
  const { model } = e.payload;
  if (model === fields.model.value) {
    updateCoremlStatus(true);
  }
  $("downloadCoreml").disabled = false;
  $("downloadCoreml").textContent = "Download";
  fields.model.disabled = false;
  coremlDownloading = false;
});

$("downloadModel").addEventListener("click", async () => {
  if (modelDownloading) return;
  const model = fields.model.value;
  const statusEl = $("modelStatus");
  const dlBtn = $("downloadModel");
  modelDownloading = true;
  dlBtn.disabled = true;
  dlBtn.textContent = "…";
  dlBtn.classList.remove("hide");
  statusEl.textContent = "0%";
  statusEl.className = "model-status downloading";
  fields.model.disabled = true;
  try {
    await invoke("download_model", { model });
  } catch (e) {
    statusEl.textContent = "✗";
    statusEl.className = "model-status error";
    dlBtn.disabled = false;
    dlBtn.textContent = "Download";
    modelDownloading = false;
    fields.model.disabled = false;
    flash(String(e));
  }
});

listen("spoke:download-progress", (e) => {
  if (!modelDownloading) return;
  const { model, percent } = e.payload;
  if (model === fields.model.value) {
    $("modelStatus").textContent = `${percent}%`;
    $("modelStatus").className = "model-status downloading";
  }
});

listen("spoke:download-complete", (e) => {
  if (!modelDownloading) return;
  const { model } = e.payload;
  if (model === fields.model.value) {
    $("modelStatus").textContent = "✓";
    $("modelStatus").className = "model-status installed";
    $("downloadModel").classList.add("hide");
  }
  $("downloadModel").disabled = false;
  $("downloadModel").textContent = "Download";
  fields.model.disabled = false;
  modelDownloading = false;
});

async function init() {
  try {
    config = await invoke("get_config");
    formFromConfig(config);
  } catch (e) {
    flash("Failed to load config: " + e);
  }
  loadBuildInfo();
  checkCurrentModel();
  requestAnimationFrame(tick);
}

init();
