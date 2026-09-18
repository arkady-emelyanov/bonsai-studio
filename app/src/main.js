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

  refreshVram();
}

async function persist() {
  settings.profile = $("profile").value;
  for (const f of TEXT_FIELDS) settings[f] = $(f).value;
  for (const f of NUM_FIELDS) settings[f] = Number($(f).value) || 0;
  for (const f of BOOL_FIELDS) settings[f] = $(f).checked;
  settings.bind_lan = $("bind_lan").value === "true";
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
  $("status-msg").textContent =
    status.message ||
    (downloading && !running ? "Downloading — start is unavailable until it finishes." : "") ||
    (status.state === "ready" ? `Serving on ${status.base_url}` : "");

  if (status.state === "error" && status.message) showAlert(status.message);

  $("open-chat").disabled = status.state !== "ready";

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

for (const id of [...TEXT_FIELDS, ...NUM_FIELDS, ...BOOL_FIELDS, "bind_lan"]) {
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
$("open-chat").addEventListener("click", () => invoke("open_chat").catch(showAlert));

(async function init() {
  settings = await invoke("get_settings");
  await refreshModels();
  render();
  await pollStatus();
  // pollDownload runs first so `downloading` is current when pollStatus reads it.
  setInterval(async () => {
    await pollDownload();
    await pollStatus();
  }, 700);
})();
