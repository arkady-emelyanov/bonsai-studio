const invoke = window.__TAURI__.core.invoke;
const $ = (id) => document.getElementById(id);

let settings = null;
let status = { state: "stopped" };
let busy = false;
let downloading = false;

// Fields that map 1:1 onto Settings, by the kind of value they hold.
const TEXT_FIELDS = ["kv_type", "cors_origins", "api_key"];
const NUM_FIELDS = ["ctx", "ngl", "port"];
const BOOL_FIELDS = ["mmproj_cpu", "kv_offload"];
// A workload is sampling plus reasoning, held in one object on Settings, so the
// two halves are read and written together.
const SAMPLING_FIELDS = ["temp", "top_p", "top_k", "min_p", "presence_penalty"];
const REASONING_FIELDS = ["reasoning", "reasoning_budget", "reasoning_budget_message"];
const WORKLOAD_FIELDS = [...SAMPLING_FIELDS, ...REASONING_FIELDS];

// Mirrors the built-ins in config.rs. They are listed here only to label the
// picker; the numbers themselves come back from the backend.
const BUILTIN_PRESETS = [
  ["chat", "Chat — long answers, unbounded thinking"],
  ["instruct", "Instruct — non-thinking"],
  ["agent", "Agent / tool use — short structured answers, bounded thinking"],
  ["custom", "Custom"],
];

function render() {
  if (!settings) return;
  $("profile").value = settings.profile;
  for (const f of TEXT_FIELDS) $(f).value = settings[f];
  for (const f of NUM_FIELDS) $(f).value = settings[f];
  for (const f of BOOL_FIELDS) $(f).checked = settings[f];
  $("bind_lan").value = String(settings.bind_lan);
  $("base_url").value = `http://127.0.0.1:${settings.port}/v1`;

  // A profile owns these three; editing them directly means Custom.
  const locked = settings.profile !== "custom";
  $("ctx").disabled = locked;
  $("kv_type").disabled = locked;
  $("mmproj_cpu").disabled = locked;

  renderWorkload();
  refreshVram();
}

// --- workload ----------------------------------------------------------------

function isBuiltin(id) {
  return BUILTIN_PRESETS.some(([key]) => key === id);
}

// Sampling lives under workload.sampling; reasoning sits beside it. One lookup
// so the field loop does not care which half a control belongs to.
function workloadValue(field) {
  const w = settings.workload;
  return SAMPLING_FIELDS.includes(field) ? w.sampling[field] : w[field];
}

function setWorkloadValue(field, value) {
  const w = settings.workload;
  if (SAMPLING_FIELDS.includes(field)) w.sampling[field] = value;
  else w[field] = value;
}

function readField(field) {
  const el = $(field);
  if (el.type === "number") return Number(el.value) || 0;
  return el.value;
}

function renderWorkload() {
  const sel = $("workload_profile");
  sel.innerHTML = "";
  const saved = settings.workload_presets.map((p) => [p.id, p.name]);
  // Saved profiles sit above Custom, so the built-ins and the user's own names
  // read as one list of choices rather than two.
  const entries = [...BUILTIN_PRESETS.slice(0, 3), ...saved, BUILTIN_PRESETS[3]];
  for (const [value, label] of entries) {
    const opt = document.createElement("option");
    opt.value = value;
    opt.textContent = label;
    sel.appendChild(opt);
  }
  sel.value = settings.workload_profile;

  for (const f of WORKLOAD_FIELDS) $(f).value = workloadValue(f);

  // A named profile owns its numbers: editing one means you wanted Custom, the
  // same rule the memory profile uses.
  const named = settings.workload_profile !== "custom";
  for (const f of WORKLOAD_FIELDS) $(f).disabled = named;
  $("delete-preset").classList.toggle("hidden", isBuiltin(settings.workload_profile));

  const notes = {
    chat: "The card's thinking-mode values. Reasoning needs the exploration a high temperature buys.",
    instruct: "The card's non-thinking values, with thinking off and the presence penalty it specifies.",
    agent: "Many short, structured decisions. Thinking stays on but is bounded, which is what keeps a one-line answer from costing thousands of tokens.",
    custom: "Your own values. Edit the fields below.",
  };
  $("workload-note").textContent = notes[settings.workload_profile] || "Saved profile.";
}

/// Turn a name into an id that will not collide with a built-in or an existing
/// preset, so switching the picker always lands on the profile the user meant.
function presetId(name) {
  const base = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  let id = base || "preset";
  let n = 2;
  while (isBuiltin(id) || settings.workload_presets.some((p) => p.id === id)) {
    id = `${base}-${n++}`;
  }
  return id;
}

// "Save as" reveals the name field; the second press commits. A native prompt()
// would be one less element, but webviews are inconsistent about honouring it.
async function savePreset() {
  const box = $("preset-name");
  if (box.classList.contains("hidden")) {
    box.classList.remove("hidden");
    box.value = "";
    box.focus();
    return;
  }
  const name = box.value;
  if (!name.trim()) {
    box.classList.add("hidden");
    return;
  }
  box.classList.add("hidden");
  // Snapshot whatever is in the fields, which is what the user was just looking
  // at -- including the values a built-in put there.
  const values = {
    sampling: {},
    reasoning: $("reasoning").value,
    reasoning_budget: Number($("reasoning_budget").value) || 0,
    reasoning_budget_message: $("reasoning_budget_message").value,
  };
  for (const f of SAMPLING_FIELDS) values.sampling[f] = Number($(f).value) || 0;
  const id = presetId(name);
  settings.workload_presets.push({ id, name: name.trim(), values });
  settings.workload_profile = id;
  settings.workload = values;
  await persist();
}

async function deletePreset() {
  const id = settings.workload_profile;
  if (isBuiltin(id)) return;
  settings.workload_presets = settings.workload_presets.filter((p) => p.id !== id);
  // Values stay as they were; only the label is gone.
  settings.workload_profile = "custom";
  await persist();
}

async function persist() {
  settings.profile = $("profile").value;
  for (const f of TEXT_FIELDS) settings[f] = $(f).value;
  for (const f of NUM_FIELDS) settings[f] = Number($(f).value) || 0;
  for (const f of BOOL_FIELDS) settings[f] = $(f).checked;
  settings.bind_lan = $("bind_lan").value === "true";
  settings.workload_profile = $("workload_profile").value;
  if (settings.workload_profile === "custom") {
    for (const f of WORKLOAD_FIELDS) setWorkloadValue(f, readField(f));
  }
  try {
    settings = await invoke("save_settings", { next: settings });
    showAlert(null);
    render();
  } catch (e) {
    showAlert(e);
  }
}

async function refreshVram() {
  const est = await invoke("estimate_vram");
  const box = $("vram");
  const free = est.free_mib === null ? "unknown" : `${est.free_mib} MiB free`;
  box.textContent = est.warning || `Estimated peak ~${est.needed_mib} MiB VRAM · ${free}`;
  box.classList.toggle("warn", Boolean(est.warning));
}

function showAlert(msg) {
  const el = $("alert");
  el.textContent = msg || "";
  el.classList.toggle("hidden", !msg);
}

// --- models ------------------------------------------------------------------

let variants = [];

async function refreshModels() {
  variants = await invoke("list_models");
  const sel = $("variant");
  sel.innerHTML = "";
  for (const v of variants) {
    const opt = document.createElement("option");
    opt.value = v.id;
    const gb = (v.bytes / 1e9).toFixed(1);
    opt.textContent = v.installed ? `${v.label}` : `${v.label} — not downloaded (${gb} GB)`;
    sel.appendChild(opt);
  }
  const installed = variants.find((v) => v.installed);
  if (installed) sel.value = installed.id;
  updateDownloadRow();
}

function currentVariant() {
  return variants.find((v) => v.id === $("variant").value);
}

// Re-downloading deletes several GB, so it takes two clicks rather than one.
let confirmForce = false;
let confirmTimer = null;

function resetConfirm() {
  confirmForce = false;
  clearTimeout(confirmTimer);
  const v = currentVariant();
  $("download-btn").textContent = v && v.installed ? "Re-download" : "Download";
}

function updateDownloadRow() {
  const v = currentVariant();
  $("download-row").classList.toggle("hidden", !v);
  $("model-line").textContent = v && v.installed ? `${v.label} · ready` : "No model downloaded";
  resetConfirm();
}

// Rolling sample used to turn byte counts into a rate and an ETA.
let rateSample = null;
let wasDownloading = false;

function formatRate(bytesPerSec) {
  if (!bytesPerSec || !isFinite(bytesPerSec)) return "";
  return `${(bytesPerSec / 1e6).toFixed(1)} MB/s`;
}

function formatEta(seconds) {
  if (!seconds || !isFinite(seconds) || seconds < 0) return "";
  if (seconds < 60) return `${Math.round(seconds)}s left`;
  const m = Math.floor(seconds / 60);
  if (m < 60) return `${m}m ${Math.round(seconds % 60)}s left`;
  return `${Math.floor(m / 60)}h ${m % 60}m left`;
}

async function pollDownload() {
  const p = await invoke("download_progress");
  const bar = $("download-bar");
  const btn = $("download-btn");
  const cancel = $("cancel-btn");

  bar.classList.toggle("hidden", !p.active);
  cancel.classList.toggle("hidden", !p.active);
  btn.disabled = p.active;
  $("download-file").textContent = p.active ? p.file : "";
  downloading = p.active;

  if (p.active) {
    const now = performance.now();
    // Reset the sample when a new file starts, so one file's rate never bleeds
    // into the next.
    if (!rateSample || rateSample.file !== p.file) {
      rateSample = { file: p.file, at: now, received: p.received, rate: 0 };
    }
    const dt = (now - rateSample.at) / 1000;
    if (dt >= 1) {
      const instant = (p.received - rateSample.received) / dt;
      // Smooth it: raw deltas over a 700ms poll jump around too much to read.
      rateSample = {
        file: p.file,
        at: now,
        received: p.received,
        rate: rateSample.rate ? rateSample.rate * 0.7 + instant * 0.3 : instant,
      };
    }

    const known = p.total > 0;
    bar.classList.toggle("indeterminate", !known);
    const pct = known ? (p.received / p.total) * 100 : 0;
    $("download-fill").style.width = known ? `${pct}%` : "";

    const gb = (n) => (n / 1e9).toFixed(2);
    const parts = [known ? `${gb(p.received)} / ${gb(p.total)} GB` : `${gb(p.received)} GB`];
    if (known) parts.push(`${pct.toFixed(1)}%`);
    const rate = formatRate(rateSample.rate);
    if (rate) parts.push(rate);
    const eta = known && rateSample.rate ? formatEta((p.total - p.received) / rateSample.rate) : "";
    if (eta) parts.push(eta);
    $("download-text").textContent = parts.join(" · ");
  } else if (p.error) {
    rateSample = null;
    $("download-text").textContent = p.error;
  } else if (p.done) {
    rateSample = null;
    $("download-text").textContent = "Download complete.";
  }

  // A finished or abandoned download changes what is on disk -- a force
  // re-download purges the files outright. Without re-surveying, the button
  // would still offer "Re-download" for a variant that is no longer installed,
  // and confirming it would delete the part file a resume needs.
  if (wasDownloading && !p.active) {
    await refreshModels();
    await selectCurrent();
  }
  wasDownloading = p.active;
}

async function selectCurrent() {
  const v = currentVariant();
  if (!v || !v.installed) return;
  try {
    settings = await invoke("select_model", { id: v.id });
    render();
  } catch (e) {
    showAlert(e);
  }
}

// --- server ------------------------------------------------------------------

async function pollStatus() {
  status = await invoke("server_status");
  const running = status.state === "ready" || status.state === "starting";

  $("light").className = `light ${status.state}`;
  $("light").title = status.state;
  $("power").textContent = busy ? "…" : running ? "Stop" : "Start";
  // Starting mid-download would load a half-written GGUF; stopping stays
  // available, since a running server and a download do not conflict.
  $("power").disabled = busy || (downloading && !running);
  $("power").title = downloading && !running ? "Waiting for the download to finish" : "";
  // "Serving on <url>" said the same thing as the endpoint row below it, and
  // said it in prose that could not be copied. This line is now only for the
  // things that are genuinely messages.
  $("status-msg").textContent =
    status.message ||
    (downloading && !running ? "Downloading — start is unavailable until it finishes." : "");

  // The whole row goes when the server does: there is nothing to connect to,
  // and a stale address invites a client to be pointed at it.
  const serving = status.state === "ready" && status.base_url;
  $("endpoint").classList.toggle("hidden", !serving);
  $("endpoint-url").textContent = serving ? status.base_url : "";
  if (!serving) $("endpoint-copied").classList.add("hidden");

  if (status.state === "error" && status.message) showAlert(status.message);

  $("open-chat").disabled = status.state !== "ready";

  // Every setting is a command-line flag, so a change cannot reach a live
  // process. Say so where the eye already is -- under the Start button -- rather
  // than leaving the panel quietly describing something that is not running.
  $("restart-note").classList.toggle("hidden", !status.restart_needed || busy);

  // The estimate answers "will this fit" before starting. Once the server is
  // up, the live graph below is the real answer, and leaving a pre-flight
  // "N MiB free" next to it reads as a contradiction.
  $("vram").classList.toggle("hidden", running);

  const log = $("log");
  const pinned = log.scrollTop + log.clientHeight >= log.scrollHeight - 24;
  log.textContent = status.log.join("\n");
  if (pinned) log.scrollTop = log.scrollHeight;
}

async function togglePower() {
  const running = status.state === "ready" || status.state === "starting";
  busy = true;
  await pollStatus();
  try {
    showAlert(null);
    await invoke(running ? "stop_server" : "start_server");
  } catch (e) {
    showAlert(e);
  } finally {
    busy = false;
    await pollStatus();
  }
}

// --- wiring ------------------------------------------------------------------

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
    document.querySelectorAll(".panel").forEach((p) =>
      p.classList.toggle("hidden", p.dataset.panel !== tab.dataset.tab)
    );
  });
});

$("power").addEventListener("click", togglePower);
$("profile").addEventListener("change", persist);
$("restart-btn").addEventListener("click", async () => {
  busy = true;
  render();
  try {
    await invoke("stop_server");
    await invoke("start_server");
  } catch (e) {
    showAlert(e);
  }
  busy = false;
  await pollStatus();
});
$("save-preset").addEventListener("click", savePreset);
$("preset-name").addEventListener("keydown", (e) => {
  if (e.key === "Enter") savePreset();
  if (e.key === "Escape") $("preset-name").classList.add("hidden");
});
$("delete-preset").addEventListener("click", deletePreset);
$("variant").addEventListener("change", async () => {
  updateDownloadRow();
  await selectCurrent();
});
$("download-btn").addEventListener("click", async () => {
  const v = currentVariant();
  if (!v) return;

  // An installed variant needs confirmation; a missing one just downloads.
  if (v.installed && !confirmForce) {
    confirmForce = true;
    const gb = (v.bytes / 1e9).toFixed(1);
    $("download-btn").textContent = `Confirm — replaces ${gb} GB`;
    confirmTimer = setTimeout(resetConfirm, 5000);
    return;
  }

  const force = confirmForce;
  resetConfirm();
  try {
    showAlert(null);
    await invoke("download_model", { id: v.id, force });
  } catch (e) {
    showAlert(e);
  }
});
$("cancel-btn").addEventListener("click", () => invoke("cancel_download"));

for (const id of [
  ...TEXT_FIELDS,
  ...NUM_FIELDS,
  ...BOOL_FIELDS,
  ...WORKLOAD_FIELDS,
  "bind_lan",
  "workload_profile",
]) {
  $(id).addEventListener("change", persist);
}

$("genkey").addEventListener("click", async () => {
  $("api_key").value = await invoke("generate_api_key");
  await persist();
});
$("copy-url").addEventListener("click", async () => {
  await navigator.clipboard.writeText($("base_url").value);
  $("copy-url").textContent = "Copied";
  setTimeout(() => ($("copy-url").textContent = "Copy"), 1200);
});
let copiedTimer = null;
$("endpoint-copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("endpoint-url").textContent);
  } catch (e) {
    showAlert(`Could not copy: ${e}`);
    return;
  }
  // Confirming next to the icon, rather than swapping the icon itself, keeps
  // the button from resizing the row as it changes.
  const note = $("endpoint-copied");
  note.classList.remove("hidden");
  clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => note.classList.add("hidden"), 1200);
});
$("open-chat").addEventListener("click", () => invoke("open_chat").catch(showAlert));


// --- GPU memory sparkline ----------------------------------------------------
//
// One series over time, so no legend: the caption names it. The y-scale is
// pinned to 0..total rather than fitted to the data -- auto-scaling a
// utilisation plot turns a flat 78% into a dramatic mountain range and hides
// how much headroom is actually left.

const GPU_CAP = 90;          // samples retained; at ~0.7s each, about a minute
const GPU_W = 320;
const GPU_H = 56;

let gpuSamples = [];
let gpuHover = null;

// Newest sample pinned to the right edge, older ones trailing left. Anchoring
// at the left instead makes a partly-filled buffer look like a chart that
// stops halfway, and moves "now" every tick until it fills.
function gpuX(i, n) {
  return GPU_W - (n - 1 - i) * (GPU_W / (GPU_CAP - 1));
}

function gpuPath(samples, total, close) {
  if (!samples.length || !total) return "";
  const n = samples.length;
  const x = (i) => gpuX(i, n);
  const y = (v) => GPU_H - Math.max(0, Math.min(1, v / total)) * GPU_H;
  const pts = samples.map((s, i) => `${x(i).toFixed(1)},${y(s.used).toFixed(1)}`);
  let d = `M${pts.join(" L")}`;
  if (close) {
    d += ` L${x(samples.length - 1).toFixed(1)},${GPU_H} L${x(0).toFixed(1)},${GPU_H} Z`;
  }
  return d;
}

const gb = (mib) => (mib / 1024).toFixed(1);

function renderGpu() {
  const fig = $("gpu");
  if (!gpuSamples.length) {
    fig.classList.add("hidden");
    return;
  }
  fig.classList.remove("hidden");

  const total = gpuSamples[gpuSamples.length - 1].total;
  $("gpu-line").setAttribute("d", gpuPath(gpuSamples, total, false));
  $("gpu-area").setAttribute("d", gpuPath(gpuSamples, total, true));

  // Hovering reads that sample; otherwise the headline is the live value.
  const shown = gpuHover !== null ? gpuSamples[gpuHover] : gpuSamples[gpuSamples.length - 1];
  const pct = total ? Math.round((shown.used / total) * 100) : 0;
  $("gpu-value").textContent = `${gb(shown.used)} / ${gb(total)} GiB \u00b7 ${pct}%`;

  const peak = gpuSamples.reduce((m, s) => Math.max(m, s.used), 0);
  if (gpuHover !== null) {
    const agoMs = gpuSamples[gpuSamples.length - 1].t - shown.t;
    const ago = agoMs < 1000 ? "now" : `${Math.round(agoMs / 1000)}s ago`;
    $("gpu-foot").textContent = `${ago} \u00b7 peak ${gb(peak)} GiB`;
  } else {
    $("gpu-foot").textContent = `peak ${gb(peak)} GiB`;
  }
}

async function pollGpu() {
  const s = await invoke("gpu_sample");
  // No NVIDIA GPU (Apple Silicon, AMD, Intel) -- hide rather than show zeroes.
  if (!s) {
    gpuSamples = [];
    renderGpu();
    return;
  }
  gpuSamples.push({ used: s.used_mib, total: s.total_mib, t: Date.now() });
  if (gpuSamples.length > GPU_CAP) gpuSamples.shift();
  renderGpu();
}

$("gpu-plot").addEventListener("mousemove", (e) => {
  if (!gpuSamples.length) return;
  const rect = e.currentTarget.getBoundingClientRect();
  const ratio = (e.clientX - rect.left) / rect.width;
  const slotsFromRight = Math.round((1 - ratio) * (GPU_CAP - 1));
  const i = gpuSamples.length - 1 - slotsFromRight;
  gpuHover = Math.max(0, Math.min(gpuSamples.length - 1, i));
  const cursor = $("gpu-cursor");
  const x = gpuX(gpuHover, gpuSamples.length);
  cursor.setAttribute("x1", x);
  cursor.setAttribute("x2", x);
  cursor.style.display = "";
  renderGpu();
});

$("gpu-plot").addEventListener("mouseleave", () => {
  gpuHover = null;
  $("gpu-cursor").style.display = "none";
  renderGpu();
});

(async function init() {
  settings = await invoke("get_settings");
  await pollGpu();
  await refreshModels();
  render();
  await pollStatus();
  // pollDownload runs first so `downloading` is current when pollStatus reads it.
  setInterval(async () => {
    await pollDownload();
    await pollStatus();
    await pollGpu();
  }, 700);
})();
