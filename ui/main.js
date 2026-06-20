// Vanilla bridge to the Rust core via Tauri's global API (withGlobalTauri).
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

// Form fields that map 1:1 to config and auto-save on change.
const fields = {
  mode: $("mode"),
  trigger: $("trigger"),
  language: $("language"),
  model: $("model"),
  apikey: $("apikey"),
  saveAudio: $("saveAudio"),
  savePath: $("savePath"),
};

// Local copy of the config; mutated by the UI and pushed back on change.
let config = null;
let amplitude = 0;
let recording = false;

// ---- Config <-> form ----------------------------------------------------

function formFromConfig(c) {
  fields.mode.value = c.general.mode;
  fields.trigger.value = c.general.trigger;
  fields.language.value = c.general.language;
  fields.model.value = c.offline.model;
  fields.apikey.value = c.online.api_key;
  fields.saveAudio.checked = c.recording.save_audio;
  fields.savePath.value = c.recording.save_path;
  hotkeyDisplay.textContent = c.general.hotkey || "—";
  applyModeVisibility(c.general.mode);
}

function configFromForm() {
  // Clone so we keep fields the UI doesn't expose (e.g. ui.* and use_gpu).
  const c = structuredClone(config);
  c.general.mode = fields.mode.value;
  c.general.trigger = fields.trigger.value;
  c.general.language = fields.language.value;
  c.offline.model = fields.model.value;
  c.online.api_key = fields.apikey.value;
  c.recording.save_audio = fields.saveAudio.checked;
  c.recording.save_path = fields.savePath.value.trim();
  // Hotkey is managed by the recorder, not a form field.
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

// ---- Hotkey recorder ----------------------------------------------------

// Map a KeyboardEvent.code to the token our Rust parser understands.
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
  if (!key) return null; // a modifier was pressed alone — keep waiting
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
    endCapture(); // cancel
    return;
  }
  const combo = comboFromEvent(e);
  if (!combo) return; // modifier-only, wait for the real key
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
  // Preserve a transient `dragging` class across state swaps.
  const wasDragging = bubble.classList.contains("dragging");
  bubble.className = state;
  if (wasDragging) bubble.classList.add("dragging");
  recording = state === "recording";
  if (state === "error" && message) flash(message);
}

// Amplitude ring drawn on canvas; decays smoothly when input is quiet.
function drawRing() {
  const ctx = ring.getContext("2d");
  const w = ring.width;
  const h = ring.height;
  ctx.clearRect(0, 0, w, h);
  if (recording) {
    const base = w * 0.38;
    const radius = base + amplitude * (w * 0.5 - base);
    ctx.beginPath();
    ctx.arc(w / 2, h / 2, radius, 0, Math.PI * 2);
    ctx.strokeStyle = `rgba(255, 77, 77, ${0.25 + amplitude * 0.6})`;
    ctx.lineWidth = 2;
    ctx.stroke();
    amplitude *= 0.9; // decay until the next sample arrives
  }
  requestAnimationFrame(drawRing);
}

// ---- Events from Rust ---------------------------------------------------

listen("spoke:state", (e) => {
  const { state, message } = e.payload;
  setBubbleState(state, message);
});

listen("spoke:amplitude", (e) => {
  // Light non-linear boost so quiet speech still moves the ring.
  amplitude = Math.min(1, Math.sqrt(e.payload) * 1.6);
});

// ---- Dragging the bubble ------------------------------------------------
// Distinguish a click (toggle panel) from a drag (move window): start the OS
// drag only after the pointer travels a few pixels, then suppress the click.

let dragOrigin = null;
let didDrag = false;

bubble.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  dragOrigin = { x: e.screenX, y: e.screenY };
  didDrag = false;
});

window.addEventListener("mousemove", (e) => {
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

window.addEventListener("mouseup", () => {
  dragOrigin = null;
  bubble.classList.remove("dragging");
});

bubble.addEventListener("click", () => {
  if (didDrag) {
    didDrag = false;
    return; // it was a drag, not a click
  }
  panel.classList.toggle("hidden");
});

// ---- Form wiring + boot -------------------------------------------------

for (const el of Object.values(fields)) {
  el.addEventListener("change", pushConfig);
}

async function init() {
  try {
    config = await invoke("get_config");
    formFromConfig(config);
  } catch (e) {
    flash("Failed to load config: " + e);
  }
  requestAnimationFrame(drawRing);
}

init();
