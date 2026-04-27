import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface Settings {
  microphone: string;
  engine: string;
  whisperModel: string;
  groqApiKey: string;
  recordingMode: string;
  hotkey: string;
}

interface MicDevice {
  name: string;
  is_default: boolean;
}

interface DownloadProgress {
  downloaded: number;
  total: number;
  percent: number;
}

// DOM elements
const statusDot = document.getElementById("status-dot")!;
const statusText = document.getElementById("status-text")!;
const micSelect = document.getElementById("mic-select") as HTMLSelectElement;
const engineLocal = document.getElementById("engine-local")!;
const engineCloud = document.getElementById("engine-cloud")!;
const localSettings = document.getElementById("local-settings")!;
const cloudSettings = document.getElementById("cloud-settings")!;
const modelSelect = document.getElementById("model-select") as HTMLSelectElement;
const downloadBtn = document.getElementById("download-btn")!;
const downloadProgress = document.getElementById("download-progress")!;
const progressText = document.getElementById("progress-text")!;
const progressFill = document.getElementById("progress-fill")!;
const groqKey = document.getElementById("groq-key") as HTMLInputElement;
const modeToggle = document.getElementById("mode-toggle")!;
const modePtt = document.getElementById("mode-ptt")!;
const hotkeyText = document.getElementById("hotkey-text")!;
const heroStateLabel = document.getElementById("hero-state-label");

// Section navigation
const navItems = document.querySelectorAll(".nav-item");
const sections = document.querySelectorAll(".content-section");

navItems.forEach((item) => {
  const activateSection = () => {
    const target = item.getAttribute("data-section");
    navItems.forEach((n) => n.classList.remove("active"));
    sections.forEach((s) => s.classList.remove("active"));
    item.classList.add("active");
    document.getElementById(`section-${target}`)?.classList.add("active");
  };

  item.addEventListener("click", activateSection);
  item.addEventListener("keydown", (event) => {
    if (event instanceof KeyboardEvent && (event.key === "Enter" || event.key === " ")) {
      event.preventDefault();
      activateSection();
    }
  });
});

// Window drag — titlebar and sidebar empty space
const titlebar = document.getElementById("titlebar")!;
const sidebar = document.getElementById("sidebar")!;
const appWindow = getCurrentWindow();

titlebar.addEventListener("mousedown", (e) => {
  if ((e.target as HTMLElement).closest("button, select, input, a, .nav-item")) return;
  appWindow.startDragging();
});

sidebar.addEventListener("mousedown", (e) => {
  if ((e.target as HTMLElement).closest("button, select, input, a, .nav-item")) return;
  appWindow.startDragging();
});

let currentSettings: Settings;
let isDownloadingModel = false;

function formatHotkey(hotkey: string): string {
  const primaryModifier = /Mac|iPhone|iPad|iPod/.test(navigator.platform) ? "Cmd" : "Ctrl";
  return hotkey.replace("CmdOrCtrl", primaryModifier);
}

function ensureValidModelSelection(model: string): string {
  const available = Array.from(modelSelect.options).map((option) => option.value);
  return available.includes(model) ? model : "medium.en";
}

async function loadSettings() {
  currentSettings = await invoke<Settings>("get_settings");

  // Populate mic dropdown
  const mics = await invoke<MicDevice[]>("list_microphones");
  micSelect.innerHTML = "";
  mics.forEach((mic) => {
    const option = document.createElement("option");
    option.value = mic.name;
    option.textContent = mic.name + (mic.is_default ? " (default)" : "");
    micSelect.appendChild(option);
  });
  micSelect.value = currentSettings.microphone;

  // Engine
  setEngine(currentSettings.engine);

  // Model
  currentSettings.whisperModel = ensureValidModelSelection(currentSettings.whisperModel);
  modelSelect.value = currentSettings.whisperModel;
  await checkModelStatus();

  // Groq key
  groqKey.value = currentSettings.groqApiKey;

  // Recording mode
  setRecordingMode(currentSettings.recordingMode);

  // Hotkey
  hotkeyText.textContent = formatHotkey(currentSettings.hotkey);
}

function setEngine(engine: string) {
  currentSettings.engine = engine;
  engineLocal.classList.toggle("active", engine === "local");
  engineCloud.classList.toggle("active", engine === "cloud");
  localSettings.classList.toggle("hidden", engine !== "local");
  cloudSettings.classList.toggle("hidden", engine !== "cloud");
}

function setRecordingMode(mode: string) {
  currentSettings.recordingMode = mode;
  modeToggle.classList.toggle("active", mode === "toggle");
  modePtt.classList.toggle("active", mode === "push-to-talk");
}

async function checkModelStatus() {
  const downloaded = await invoke<boolean>("check_model_downloaded", {
    modelSize: modelSelect.value,
  });
  if (isDownloadingModel) {
    return;
  }
  downloadBtn.textContent = downloaded ? "\u2713" : "Download";
  (downloadBtn as HTMLButtonElement).disabled = downloaded;
}

function setDownloadUiState(isDownloading: boolean) {
  isDownloadingModel = isDownloading;
  modelSelect.disabled = isDownloading;
  (downloadBtn as HTMLButtonElement).disabled = isDownloading;
  if (isDownloading) {
    downloadBtn.textContent = "Downloading...";
    progressText.textContent = "Preparing download...";
    progressFill.style.width = "0%";
    progressFill.classList.remove("indeterminate");
    downloadProgress.classList.remove("hidden");
  }
}

function formatDownloadProgress(downloaded: number, total: number, percent: number) {
  const downloadedMb = (downloaded / (1024 * 1024)).toFixed(0);
  if (total > 0) {
    const totalMb = (total / (1024 * 1024)).toFixed(0);
    const safePercent = Math.max(0, Math.min(100, percent));
    progressText.textContent = `${safePercent.toFixed(0)}% (${downloadedMb} / ${totalMb} MB)`;
    progressFill.classList.remove("indeterminate");
    progressFill.style.width = `${safePercent}%`;
  } else {
    progressText.textContent = `Downloading... (${downloadedMb} MB)`;
    progressFill.classList.add("indeterminate");
    progressFill.style.width = "100%";
  }
}

async function saveSettings() {
  currentSettings.microphone = micSelect.value;
  currentSettings.whisperModel = modelSelect.value;
  currentSettings.groqApiKey = groqKey.value;
  await invoke("save_settings", { settings: currentSettings });
}

// Event listeners
engineLocal.addEventListener("click", () => {
  setEngine("local");
  saveSettings();
});

engineCloud.addEventListener("click", () => {
  setEngine("cloud");
  saveSettings();
});

micSelect.addEventListener("change", () => saveSettings());

modelSelect.addEventListener("change", async () => {
  await checkModelStatus();
  saveSettings();
});

downloadBtn.addEventListener("click", async () => {
  setDownloadUiState(true);

  try {
    await invoke("download_model", { modelSize: modelSelect.value });
    progressText.textContent = "Download complete";
    progressFill.classList.remove("indeterminate");
    progressFill.style.width = "100%";
    downloadBtn.textContent = "\u2713";
  } catch (e) {
    progressText.textContent = "Download failed";
    progressFill.classList.remove("indeterminate");
    progressFill.style.width = "0%";
    downloadBtn.textContent = "Retry";
    (downloadBtn as HTMLButtonElement).disabled = false;
    console.error("Download failed:", e);
  } finally {
    isDownloadingModel = false;
    modelSelect.disabled = false;
    await checkModelStatus();
    window.setTimeout(() => {
      downloadProgress.classList.add("hidden");
      progressFill.classList.remove("indeterminate");
    }, 600);
  }
});

groqKey.addEventListener("change", () => saveSettings());

modeToggle.addEventListener("click", () => {
  setRecordingMode("toggle");
  saveSettings();
});

modePtt.addEventListener("click", () => {
  setRecordingMode("push-to-talk");
  saveSettings();
});

// Listen for recording state changes
listen<string>("recording-state", (event) => {
  const state = event.payload;
  statusDot.className = "";
  if (state === "Recording") {
    statusDot.classList.add("recording");
    statusText.textContent = "Recording...";
    if (heroStateLabel) heroStateLabel.textContent = "Listening now";
  } else if (state === "Transcribing") {
    statusDot.classList.add("transcribing");
    statusText.textContent = "Transcribing...";
    if (heroStateLabel) heroStateLabel.textContent = "Cleaning up text";
  } else {
    statusDot.classList.add("ready");
    statusText.textContent = "Ready";
    if (heroStateLabel) heroStateLabel.textContent = "Ready to listen";
  }
});

// Listen for download progress
listen<DownloadProgress>("download-progress", (event) => {
  const { downloaded, total, percent } = event.payload;
  if (!isDownloadingModel) {
    return;
  }
  formatDownloadProgress(downloaded, total, percent);
});

// Initialize
loadSettings();
