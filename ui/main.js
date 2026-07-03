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
  accel: $("accel"),
  apikey: $("apikey"),
  saveAudio: $("saveAudio"),
  savePath: $("savePath"),
  saveMode: $("saveMode"),
  micDevice: $("micDevice"),
  copyToClipboard: $("copyToClipboard"),
};

let config = null;
let recording = false;
let modelDownloading = false;
let coremlDownloading = false;
let buildInfo = null;

let history = [];
const MAX_HISTORY = 50;
let historyVisible = false;

// ---- Permission warnings --------------------------------------------------

const permWarning = $("permWarning");
const permBadge = $("permBadge");
let permissions = null;
let lastPermSig = null;

const PERM_INFO = {
  microphone: { label: "Microphone access denied", hint: "Spoke can't hear you" },
  accessibility: { label: "Accessibility not granted", hint: "Spoke can't type for you" },
};

function missingPermissions() {
  if (!permissions) return [];
  const missing = [];
  if (permissions.microphone === "denied") missing.push("microphone");
  // Accessibility only matters for keystroke injection; clipboard mode doesn't
  // use it, so don't nag about it there.
  const injecting = !(config && config.general && config.general.copy_to_clipboard);
  if (injecting && permissions.accessibility === "denied") missing.push("accessibility");
  return missing;
}

function renderPermissionWarnings() {
  const missing = missingPermissions();
  const sig = missing.join(",");
  if (sig === lastPermSig) return;
  lastPermSig = sig;

  permBadge.classList.toggle("hide", missing.length === 0);
  permWarning.classList.toggle("hide", missing.length === 0);
  permWarning.innerHTML = "";
  for (const key of missing) {
    const row = document.createElement("div");
    row.className = "perm-row";
    const text = document.createElement("span");
    text.textContent = `⚠ ${PERM_INFO[key].label} — ${PERM_INFO[key].hint}`;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "mini-btn";
    btn.textContent = "Fix";
    btn.title = "Open the system settings pane for this permission";
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      invoke("open_permission_settings", { which: key });
    });
    row.appendChild(text);
    row.appendChild(btn);
    permWarning.appendChild(row);
  }
  // Visibility changed — force a frame on Wayland (see nudge_repaint).
  invoke("nudge_repaint");
}

async function checkPermissions() {
  try {
    permissions = await invoke("check_permissions");
  } catch (_) {
    permissions = null;
  }
  renderPermissionWarnings();
}

// ── Mosaic shape canvas ────────────────────────────────────────────────────
// Organic tile-mosaic blob on a transparent canvas — the tile alpha defines
// the silhouette (no containing circle). Each state morphs the boundary and
// palette; motion is procedural only (no audio input).
const S   = 48;
const dpr = Math.min(window.devicePixelRatio || 1, 2);
ring.width  = S * dpr;
ring.height = S * dpr;
const mCtx  = ring.getContext('2d');
mCtx.scale(dpr, dpr);

const COLS = 13;
const cell = S / COLS;
const GAP  = cell * 0.14;
const EDGE_SOFT = 1.6; // px of alpha falloff at the shape boundary
const STAR_PTS  = 6;
const CX = S / 2;
const CY = S / 2;

// Per-state palette + boundary parameters (radii in px at 48px scale).
const MODES = {
  idle: {
    pal: { bg: [10, 10, 12], fg: [228, 228, 234] },
    shape: { baseR: 17.5, blobAmp: 0.045, blobSpeed: 0.7, starAmp: 0, starSpeed: 0.5, breathAmp: 0.028, breathSpeed: 0.9, jitterAmp: 0 },
  },
  listening: {
    pal: { bg: [44, 10, 22], fg: [255, 108, 75] },
    shape: { baseR: 18.5, blobAmp: 0.105, blobSpeed: 2.7, starAmp: 0, starSpeed: 0.5, breathAmp: 0.055, breathSpeed: 3.4, jitterAmp: 0 },
  },
  processing: {
    pal: { bg: [8, 32, 44], fg: [38, 212, 196] },
    shape: { baseR: 17, blobAmp: 0.020, blobSpeed: 1.0, starAmp: 0.095, starSpeed: 1.7, breathAmp: 0.015, breathSpeed: 1.2, jitterAmp: 0 },
  },
};
const ERROR_PAL = { bg: [46, 26, 6], fg: [255, 186, 46] };

let mosaicT    = 0;
let mosaicPrev = performance.now();

// Animated state — mode weights and shape params are lerped every frame so
// state changes morph instead of cutting.
const shapeP = Object.assign({}, MODES.idle.shape);
const modeW  = { idle: 1, listening: 0, processing: 0 };
let errB = 0;                 // error blend 0..1 (overlays any mode)
let press = 0;                // squish spring on press
let pressTarget = 0;
let pressT = -10;             // time of last press (drives the ripple)
let popT = -10;               // time processing finished (drives the "spit" pop)
// Phase accumulators so speed changes never make the motion jump.
let phB = 0, phBr = 0, phSt = 0;

const lerp    = (a, b, k) => a + (b - a) * k;
const clamp01 = (x) => (x < 0 ? 0 : x > 1 ? 1 : x);

// Reused per-frame color buffers (see drawMosaic) — module-level to avoid
// allocating on every animation frame.
const bbg = [0, 0, 0];
const bfg = [0, 0, 0];

// Ambient animation is throttled to this rate once the idle state has fully
// settled; transitions, listening/processing, error, and the press spring run
// at full refresh. Continuous 60fps full-canvas redraws when idle are the main
// driver of WebKit web-process memory/CPU.
const IDLE_INTERVAL = 1000 / 20;
let lastFrame = 0;

// ---- Panel close ---------------------------------------------------------

// Don't import LogicalSize from @tauri-apps/api/window — use the global.
const LOGICAL_SIZE   = window.__TAURI__.window.LogicalSize;
const LOGICAL_POS    = window.__TAURI__.window.LogicalPosition;

const PANEL_W = 300;
const PANEL_H = 360;
const BUBBLE_W = 80;
const BUBBLE_H = 80;
const MARGIN = 24;

/// Resize while keeping the bubble visually fixed: the bubble sits at the
/// window's bottom-right corner, so anchor that corner to wherever the window
/// currently is (the user may have dragged it anywhere on screen).
/// `initial` places the window at the screen's bottom-right instead — used
/// once at boot, mirroring position_bubble() in Rust.
async function resizeAndReposition(w, h, initial = false) {
  try {
    let right, bottom;
    if (initial) {
      right = window.screen.width - MARGIN;
      bottom = window.screen.height - MARGIN;
    } else {
      const factor = await appWindow.scaleFactor();
      const pos = (await appWindow.outerPosition()).toLogical(factor);
      const size = (await appWindow.outerSize()).toLogical(factor);
      right = pos.x + size.width;
      bottom = pos.y + size.height;
    }
    await appWindow.setSize(new LOGICAL_SIZE(w, h));
    await appWindow.setPosition(new LOGICAL_POS(right - w, bottom - h));
  } catch (_) { /* best‑effort — some platforms may lack the API */ }
}

// On Wayland, WebKitGTK does not re-commit a transparent, unfocused window's
// surface on content change, so the panel's DOM state flips but the pixels do not
// update until an unrelated ~20s refresh. `nudge_repaint` resizes the window by
// 1px, forcing a surface reconfigure + commit that presents the new frame.
function closePanel() {
  panel.classList.add("hidden");
  resizeAndReposition(BUBBLE_W, BUBBLE_H);
  invoke("nudge_repaint");
}

function openPanel() {
  panel.classList.remove("hidden");
  resizeAndReposition(PANEL_W, PANEL_H);
  invoke("nudge_repaint");
  // Re-check on open so the banner reflects grants made since the last poll.
  checkPermissions();
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
  // The accel select is populated later by loadBuildInfo(), which re-applies
  // the saved value once the options exist.
  if (c.offline.accel) fields.accel.value = c.offline.accel;
  fields.apikey.value = c.online.api_key;
  fields.saveAudio.checked = c.recording.save_audio;
  fields.savePath.value = c.recording.save_path;
  fields.saveMode.value = c.recording.save_processed ? "processed" : "original";
  fields.copyToClipboard.checked = c.general.copy_to_clipboard;
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
  // Empty when the selector was never populated (CPU-only build) — keep the
  // stored value rather than clobbering it.
  if (fields.accel.value) c.offline.accel = fields.accel.value;
  c.online.api_key = fields.apikey.value;
  c.recording.save_audio = fields.saveAudio.checked;
  c.recording.save_path = fields.savePath.value.trim();
  c.recording.save_processed = fields.saveMode.value === "processed";
  c.recording.input_device = fields.micDevice.value;
  c.general.hotkey = config.general.hotkey;
  c.general.copy_to_clipboard = fields.copyToClipboard.checked;
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
  // Clipboard mode toggles whether the Accessibility warning is relevant.
  renderPermissionWarnings();
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
  const wasProcessing = bubble.classList.contains("processing");
  bubble.className = state;
  if (wasDragging) bubble.classList.add("dragging");
  // Processing → idle means transcription landed and typing begins: fire the
  // "spit" pop so the bubble visibly reacts to finishing.
  if (wasProcessing && state === "idle") popT = mosaicT;
  recording = state === "recording";
  if (state === "error" && message) flash(message);
}

// Advance mode weights, error blend, press spring, and shape params.
function mosaicStep(dt) {
  const mode = bubble.classList.contains('recording') ? 'listening'
             : bubble.classList.contains('processing') ? 'processing'
             : 'idle';
  const isError = bubble.classList.contains('error');
  const k  = 1 - Math.exp(-dt * 4);
  const k2 = 1 - Math.exp(-dt * 5);

  let sum = 0;
  for (const m in modeW) {
    modeW[m] = lerp(modeW[m], mode === m ? 1 : 0, k2);
    sum += modeW[m];
  }
  for (const m in modeW) modeW[m] /= sum;

  errB = lerp(errB, isError ? 1 : 0, k);

  // Press spring: fast attack, softer release.
  const pk = 1 - Math.exp(-dt * (pressTarget > press ? 22 : 8));
  press = lerp(press, pressTarget, pk);

  for (const key in shapeP) {
    let v = 0;
    for (const m in modeW) v += MODES[m].shape[key] * modeW[m];
    if (errB > 0.001) {
      if (key === 'jitterAmp')   v = lerp(v, 0.06, errB);
      if (key === 'breathAmp')   v = lerp(v, 0.06, errB);
      if (key === 'breathSpeed') v = lerp(v, 5.2, errB);
    }
    shapeP[key] = lerp(shapeP[key], v, k);
  }

  phB  += shapeP.blobSpeed   * dt;
  phBr += shapeP.breathSpeed * dt;
  phSt += shapeP.starSpeed   * dt;
}

// Organic boundary radius at angle th.
function shapeR(th) {
  const p = shapeP;
  const blob = Math.sin(th * 3 + phB) * 0.5
             + Math.sin(th * 5 - phB * 1.37 + 1.7) * 0.30
             + Math.sin(th * 2 + phB * 0.80 + 4.2) * 0.35;
  const star   = Math.cos(th * STAR_PTS - phSt);
  const jag    = Math.sin(th * 9 + mosaicT * 6) * Math.sin(th * 13 - mosaicT * 7.3);
  const breath = Math.sin(phBr);
  return p.baseR * (1 + p.breathAmp * breath + p.blobAmp * blob + p.starAmp * star + p.jitterAmp * jag);
}

function drawMosaic(now) {
  const dt   = Math.min((now - mosaicPrev) / 1000, 0.05);
  mosaicT   += dt;
  mosaicPrev = now;
  mosaicStep(dt);

  // Blend palettes once per frame. Reuse module-level arrays (bbg/bfg) instead
  // of allocating on every frame — avoids GC churn that inflates the WebKit
  // web process heap under the continuous animation loop.
  bbg[0] = bbg[1] = bbg[2] = 0;
  bfg[0] = bfg[1] = bfg[2] = 0;
  for (const m in modeW) {
    const pal = MODES[m].pal;
    for (let i = 0; i < 3; i++) {
      bbg[i] += pal.bg[i] * modeW[m];
      bfg[i] += pal.fg[i] * modeW[m];
    }
  }
  for (let i = 0; i < 3; i++) {
    bbg[i] = lerp(bbg[i], ERROR_PAL.bg[i], errB * 0.85);
    bfg[i] = lerp(bfg[i], ERROR_PAL.fg[i], errB * 0.85);
  }

  // Transparent outside the blob — everything beyond the boundary stays clear.
  mCtx.clearRect(0, 0, S, S);

  const wI = modeW.idle, wL = modeW.listening, wP = modeW.processing;
  // Damped-spring "spit" when processing finishes: quick outward push that
  // rebounds and settles (~0.9s), plus a bright ripple in the tile loop.
  const popAge = mosaicT - popT;
  const pop = popAge >= 0 && popAge < 1
    ? Math.exp(-popAge * 4.5) * Math.sin(popAge * 14)
    : 0;
  const sx = 1 + press * 0.09 + pop * 0.08;
  const sy = 1 - press * 0.13 + pop * 0.13;
  const pressAge = mosaicT - pressT;
  const blink = Math.pow(Math.max(0, Math.sin(mosaicT * 3.2)), 3);

  // Fill the silhouette with the background palette color first so the gaps
  // between tiles read as part of the blob instead of showing the desktop
  // through. Trace the boundary once and reuse the path for the outline.
  const N = 64;
  const path = new Path2D();
  for (let i = 0; i <= N; i++) {
    const th = (i / N) * Math.PI * 2;
    const R = shapeR(th);
    const X = CX + Math.cos(th) * R * sx;
    const Y = CY + Math.sin(th) * R * sy;
    if (i === 0) path.moveTo(X, Y);
    else path.lineTo(X, Y);
  }
  path.closePath();
  mCtx.fillStyle = `rgb(${bbg[0] | 0},${bbg[1] | 0},${bbg[2] | 0})`;
  mCtx.fill(path);
  // Clip the tiles to the silhouette so partially-faded edge tiles never
  // poke past the boundary (they read as grey stubs on light desktops).
  mCtx.save();
  mCtx.clip(path);

  for (let row = 0; row < COLS; row++) {
    for (let col = 0; col < COLS; col++) {
      const x = col * cell + cell / 2;
      const y = row * cell + cell / 2;
      const dx = (x - CX) / sx;
      const dy = (y - CY) / sy;
      const th = Math.atan2(dy, dx);
      const d  = Math.sqrt(dx * dx + dy * dy);
      const R  = shapeR(th);
      const a  = clamp01((R - d) / EDGE_SOFT + 0.5);
      if (a < 0.02) continue;

      const distN = clamp01(d / R);
      const nx = x / S;
      const ny = y / S;
      const hash = ((col * 7 + row * 13) % 31) / 31;

      let v = 0;
      if (wI > 0.02) {
        v += wI * (0.22
          + 0.18 * Math.sin(mosaicT * 0.55 + nx * 4.0 + ny * 2.5 + hash * 1.5)
          + 0.08 * Math.sin(mosaicT * 0.40 + nx * 1.5 - ny * 3.0 + hash * 2.0));
      }
      if (wL > 0.02) {
        const r1 = Math.sin(mosaicT * 2.8 - distN * 4.5);
        const r2 = Math.sin(mosaicT * 3.1 - distN * 5.5 + 2.1);
        const fl = Math.abs(Math.sin(mosaicT * 5.5 + col * 1.1)) * Math.max(0, 1 - distN * 1.6);
        v += wL * (0.08 + Math.max(0, r1) * 0.52 + Math.max(0, r2) * 0.30 + fl * 0.15);
      }
      if (wP > 0.02) {
        const waveA = Math.sin(mosaicT * 1.6 + th * 3 + distN * 6);
        const waveB = Math.sin(mosaicT * 2.4 - distN * 8 + th * 2);
        v += wP * (0.10 + (waveA * 0.5 + 0.5) * 0.38 + (waveB * 0.5 + 0.5) * 0.30);
      }
      if (errB > 0.02) {
        const ve = 0.12 + Math.max(0, Math.sin(mosaicT * 5 - distN * 4)) * 0.55 + blink * 0.35;
        v = lerp(v, ve, errB * 0.85);
      }
      // Press ripple radiating outward from the click.
      if (pressAge < 0.8) {
        const band = Math.max(0, 1 - Math.abs(distN * 1.15 - pressAge * 1.9) * 5) * (1 - pressAge / 0.8);
        v += band * 0.6;
      }
      // Brighter, faster ripple for the done-processing spit.
      if (popAge >= 0 && popAge < 0.7) {
        const band = Math.max(0, 1 - Math.abs(distN * 1.15 - popAge * 2.4) * 4) * (1 - popAge / 0.7);
        v += band * 0.85;
      }
      v = clamp01(v);

      const ir = Math.round(bbg[0] + (bfg[0] - bbg[0]) * v);
      const ig = Math.round(bbg[1] + (bfg[1] - bbg[1]) * v);
      const ib = Math.round(bbg[2] + (bfg[2] - bbg[2]) * v);
      const sz = 0.68 + v * 0.32;
      const cw = (cell - GAP) * sz;
      mCtx.globalAlpha = a;
      mCtx.fillStyle = `rgb(${ir},${ig},${ib})`;
      mCtx.fillRect(x - cw / 2, y - cw / 2, cw, cw);
    }
  }
  mCtx.globalAlpha = 1;
  mCtx.restore();

  // Soft outline tracing the boundary — helps the silhouette read against
  // busy desktop backgrounds.
  const baseA = 0.34 + errB * blink * 0.45;
  mCtx.lineWidth = 1;
  mCtx.strokeStyle = `rgba(${bfg[0] | 0},${bfg[1] | 0},${bfg[2] | 0},${baseA.toFixed(3)})`;
  mCtx.stroke(path);
}

// Animation driver: always schedules the next frame, but skips the actual draw
// when the window is hidden (occluded/minimized) and throttles to IDLE_INTERVAL
// once the idle state has fully settled. Transitions, active states, error,
// and the press spring run at full refresh so the morphing stays smooth.
function tick(now) {
  requestAnimationFrame(tick);
  if (document.hidden) return;
  const settled = modeW.idle > 0.995 && errB < 0.005 &&
                  press < 0.01 && pressTarget === 0 &&
                  mosaicT - popT > 1.2;
  if (settled && now - lastFrame < IDLE_INTERVAL) return;
  lastFrame = now;
  drawMosaic(now);
}

// ---- Events from Rust ---------------------------------------------------

listen("spoke:state", (e) => {
  const { state, message } = e.payload;
  setBubbleState(state, message);
});

listen("spoke:transcript", (e) => {
  const text = e.payload.text;
  if (!text) return;
  history.unshift({ text, time: Date.now() });
  if (history.length > MAX_HISTORY) history.pop();
  if (historyVisible) renderHistory();
});

async function copyToClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    flash("Copied");
  } catch {
    flash("Copy failed");
  }
}

function renderHistory() {
  const list = $("historyList");
  list.innerHTML = "";
  if (history.length === 0) {
    list.innerHTML = '<div class="history-empty">No transcriptions yet</div>';
    return;
  }
  for (const entry of history) {
    const row = document.createElement("div");
    row.className = "history-entry";
    const label = document.createElement("span");
    label.className = "history-text";
    label.textContent = entry.text;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "mini-btn";
    btn.textContent = "Copy";
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      copyToClipboard(entry.text);
    });
    row.appendChild(label);
    row.appendChild(btn);
    list.appendChild(row);
  }
}

$("historyToggle").addEventListener("click", () => {
  historyVisible = !historyVisible;
  $("historySection").classList.toggle("hide", !historyVisible);
  $("historyToggle").textContent = historyVisible ? "Hide" : "Show";
  if (historyVisible) renderHistory();
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
  // Squish the blob while held (see press spring in mosaicStep).
  pressTarget = 1;
  pressT = mosaicT;
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
  pressTarget = 0;
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
  pressTarget = 0;
  bubble.classList.remove("dragging");
});

// Release the squish if the pointer slides off the bubble mid-press,
// but not during an active drag (the pointer may leave the small bubble
// element while the window drag is still in flight).
bubble.addEventListener("pointerleave", () => {
  if (!didDrag) pressTarget = 0;
});

// ---- Form wiring + boot -------------------------------------------------

for (const el of Object.values(fields)) {
  if (el.id !== "model" && el.id !== "accel") {
    el.addEventListener("change", pushConfig);
  }
}

fields.model.addEventListener("change", async () => {
  await pushConfig();
  if (!modelDownloading) checkCurrentModel();
});

fields.accel.addEventListener("change", async () => {
  applyCoremlRowVisibility();
  updateAccelBadge();
  await pushConfig();
  if (!modelDownloading) checkCurrentModel();
});

// True when the given backend id was compiled into this build.
function hasBackend(id) {
  return !!(buildInfo && buildInfo.backends && buildInfo.backends.some((b) => b.id === id));
}

// Badge label for the currently selected accel value. "auto" resolves to the
// build's best backend; unknown/stale ids fall back to CPU.
function getEffectiveAccel(accel, info) {
  if (!info) return "—";
  if (!info.whisper) return "CPU";
  if (accel === "auto" || !accel) return info.acceleration || "CPU";
  const backend = (info.backends || []).find((b) => b.id === accel);
  return backend ? backend.badge : "CPU";
}

function updateAccelBadge() {
  const label = getEffectiveAccel(fields.accel.value, buildInfo);
  accelBadge.textContent = label;
  accelBadge.dataset.accel = label;
}

async function loadBuildInfo() {
  try {
    buildInfo = await invoke("get_build_info");
    if (!buildInfo.whisper) {
      accelBadge.textContent = "CPU (no whisper)";
      accelBadge.dataset.accel = "CPU";
      return;
    }

    // Backends come from the build (best first, CPU always last). Show the
    // selector only when there is an actual choice beyond CPU.
    const backends = buildInfo.backends || [];
    if (backends.length > 1) {
      const sel = fields.accel;
      sel.innerHTML = "";
      const auto = document.createElement("option");
      auto.value = "auto";
      auto.textContent = `Auto (${buildInfo.acceleration})`;
      sel.appendChild(auto);
      for (const b of backends) {
        const opt = document.createElement("option");
        opt.value = b.id;
        opt.textContent = b.label;
        sel.appendChild(opt);
      }
      // Restore the saved choice; values from another build fall back to auto.
      const saved = config && config.offline ? config.offline.accel : "";
      sel.value = saved === "auto" || backends.some((b) => b.id === saved) ? saved : "auto";
      $("gpuRow").classList.remove("hide");
    }

    applyCoremlRowVisibility();
    updateAccelBadge();
  } catch (e) {
    accelBadge.textContent = "unknown";
  }
}

function applyCoremlRowVisibility() {
  const show = hasBackend("coreml") &&
    (fields.accel.value === "coreml" || fields.accel.value === "auto");
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
    if (hasBackend("coreml") && !$("coremlRow").classList.contains("hide")) {
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
  checkPermissions();
  // Permissions can change behind our back (System Settings, TCC resets on
  // rebuild); poll cheaply so warnings appear and clear without a restart.
  setInterval(checkPermissions, 15000);
  requestAnimationFrame(tick);
  // Window starts at 320×320; shrink to bubble-only size immediately.
  resizeAndReposition(BUBBLE_W, BUBBLE_H, true);
}

init();
