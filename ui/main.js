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

const fields = {
  mode: $("mode"),
  trigger: $("trigger"),
  language: $("language"),
  model: $("model"),
  apikey: $("apikey"),
  saveAudio: $("saveAudio"),
  savePath: $("savePath"),
  micDevice: $("micDevice"),
};

let config = null;
let amplitude = 0;
let recording = false;

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
  fields.apikey.value = c.online.api_key;
  fields.saveAudio.checked = c.recording.save_audio;
  fields.savePath.value = c.recording.save_path;
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
  c.online.api_key = fields.apikey.value;
  c.recording.save_audio = fields.saveAudio.checked;
  c.recording.save_path = fields.savePath.value.trim();
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
    amplitude *= 0.9;
  }
  requestAnimationFrame(drawRing);
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
