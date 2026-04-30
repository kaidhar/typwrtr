import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

interface Settings {
  microphone: string;
  whisperModel: string;
  modelDir: string;
  toggleHotkey: string;
  pushToTalkHotkey: string;
  language: string;
  initialPrompt: string;
  saveTranscriptions: boolean;
  keepAudioClips: boolean;
  fixupHotkey: string;
  streamingCaptions: boolean;
  vadSilenceMs: number;
}

interface DbHealth {
  transcriptions: number;
  corrections: number;
  vocabulary: number;
  app_profiles: number;
  snippets: number;
}

interface UsageWindow {
  words: number;
  dictations: number;
  duration_ms_total: number;
  edited: number;
  avg_latency_ms: number;
}

interface DailyBucket {
  date: string;
  words: number;
  dictations: number;
  edit_rate: number;
}

interface AppBreakdown {
  bundle_id: string;
  display_name: string;
  words: number;
  dictations: number;
}

interface SnippetRow {
  id: number;
  trigger: string;
  expansion: string;
  is_dynamic: boolean;
  created_at: number;
}

interface CorrectionRow {
  id: number;
  wrong: string;
  right: string;
  context: string | null;
  app_bundle_id: string | null;
  count: number;
  last_seen_at: number;
  source: string;
}

interface AutoLearnApplied {
  count: number;
  app: string;
  sample_wrong: string;
  sample_right: string;
}

interface VocabularyRow {
  id: number;
  term: string;
  weight: number;
  source: string;
  app_bundle_id: string | null;
  created_at: number;
}

interface AppProfileRow {
  bundle_id: string;
  display_name: string;
  prompt_template: string | null;
  postprocess_mode: string;
  preferred_model: string | null;
  enabled: boolean;
  auto_apply_replacements: boolean;
  phonetic_match: boolean;
  code_case: string;
  last_used_at: number | null;
  use_count: number;
  is_persisted: boolean;
}

const POSTPROCESS_MODES: ReadonlyArray<string> = ["default", "markdown", "plain", "code"];
const CODE_CASES: ReadonlyArray<{ value: string; label: string }> = [
  { value: "snake", label: "snake_case" },
  { value: "camel", label: "camelCase" },
  { value: "kebab", label: "kebab-case" },
];
/// The shipped model lineup. Keep this list short — only entries that
/// pull weight in real use survive. `engine` controls which Tauri-side
/// transcriber the model dispatches to.
const MODELS: ReadonlyArray<{ value: string; label: string; engine: "whisper" | "parakeet" }> = [
  { value: "large-v3-turbo", label: "Whisper Large v3 Turbo · GPU", engine: "whisper" },
  { value: "parakeet-tdt-0.6b-v2", label: "Parakeet TDT 0.6B v2 · CPU", engine: "parakeet" },
  { value: "medium.en", label: "Whisper Medium English · low-VRAM fallback", engine: "whisper" },
];

/// Engine name for a given model id. Mirrors `engine_for_model` on the
/// Rust side so the UI can hide engine-specific controls.
function engineForModel(modelId: string): "whisper" | "parakeet" {
  const m = MODELS.find(x => x.value === modelId);
  return m?.engine ?? "whisper";
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
const modelSelect = document.getElementById("model-select") as HTMLSelectElement;
const modelDirInput = document.getElementById("model-dir") as HTMLInputElement;
const resetModelDirBtn = document.getElementById("reset-model-dir")!;
const selectModelDirBtn = document.getElementById("select-model-dir")!;
const downloadBtn = document.getElementById("download-btn")!;
const downloadProgress = document.getElementById("download-progress")!;
const progressText = document.getElementById("progress-text")!;
const progressFill = document.getElementById("progress-fill")!;
const toggleHotkeyText = document.getElementById("toggle-hotkey-text")!;
const pttHotkeyText = document.getElementById("ptt-hotkey-text")!;
const toggleHotkeySet = document.getElementById("toggle-hotkey-set")!;
const pttHotkeySet = document.getElementById("ptt-hotkey-set")!;
const fixupHotkeyText = document.getElementById("fixup-hotkey-text")!;
const fixupHotkeySet = document.getElementById("fixup-hotkey-set")!;
const streamingCaptionsToggle = document.getElementById("streaming-captions") as HTMLInputElement;
const streamingCaptionsRow = document.getElementById("streaming-captions-row")!;
const vadSilenceInput = document.getElementById("vad-silence-ms") as HTMLInputElement;
const hotkeyCaptureStatus = document.getElementById("hotkey-capture-status")!;
const heroStateLabel = document.getElementById("hero-state-label");
const saveTranscriptionsToggle = document.getElementById("save-transcriptions") as HTMLInputElement;
const keepAudioClipsToggle = document.getElementById("keep-audio-clips") as HTMLInputElement;
const dbHealthLine = document.getElementById("db-health-line")!;
const clearLearningBtn = document.getElementById("clear-learning-btn") as HTMLButtonElement;
const topCorrectionsTable = document.getElementById("top-corrections-table")!;
const topVocabTable = document.getElementById("top-vocab-table")!;
const dashboardWindowSelect = document.getElementById("dashboard-window") as HTMLSelectElement;
const statTimeSaved = document.getElementById("stat-time-saved")!;
const statTimeSavedDetail = document.getElementById("stat-time-saved-detail")!;
const statWords = document.getElementById("stat-words")!;
const statWordsMeta = document.getElementById("stat-words-meta")!;
const statDictations = document.getElementById("stat-dictations")!;
const statDictationsMeta = document.getElementById("stat-dictations-meta")!;
const statEditRate = document.getElementById("stat-edit-rate")!;
const statTopApp = document.getElementById("stat-top-app")!;
const statTopAppMeta = document.getElementById("stat-top-app-meta")!;
const chartWordsDaily = document.getElementById("chart-words-daily")!;
const chartEditRate = document.getElementById("chart-edit-rate")!;
const appBreakdownList = document.getElementById("app-breakdown-list")!;
const appProfilesList = document.getElementById("app-profiles-list")!;
const appProfilesEmpty = document.getElementById("app-profiles-empty")!;
const snippetsList = document.getElementById("snippets-list")!;
const snippetsEmpty = document.getElementById("snippets-empty")!;
const snippetAddBtn = document.getElementById("snippet-add-btn") as HTMLButtonElement;

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
let hotkeyCaptureTarget: "toggle" | "ptt" | "fixup" | null = null;
const isMacPlatform = /Mac|iPhone|iPad|iPod/.test(navigator.platform);

function formatHotkey(hotkey: string): string {
  const primaryModifier = isMacPlatform ? "Cmd" : "Ctrl";
  return hotkey
    .replace("CmdOrCtrl", primaryModifier)
    .split("Key").join("")
    .split("Digit").join("")
    .split("Arrow").join("")
    .split("Meta").join("Cmd")
    .split("Control").join("Ctrl")
    .split("Backquote").join("`")
    .split("Minus").join("-")
    .split("Equal").join("=")
    .split("BracketLeft").join("[")
    .split("BracketRight").join("]")
    .split("Backslash").join("\\")
    .split("Semicolon").join(";")
    .split("Quote").join("'")
    .split("Comma").join(",")
    .split("Period").join(".")
    .split("Slash").join("/")
    .split("Space").join("Space");
}

function normalizeShortcutCode(code: string): string | null {
  if (code.startsWith("Key")) return code;
  if (code.startsWith("Digit")) return code;
  if (code.startsWith("Numpad")) return code;
  switch (code) {
    case "Space":
    case "Enter":
    case "Tab":
    case "Escape":
    case "Backspace":
    case "Delete":
    case "Insert":
    case "Home":
    case "End":
    case "PageUp":
    case "PageDown":
    case "ArrowUp":
    case "ArrowDown":
    case "ArrowLeft":
    case "ArrowRight":
    case "Backquote":
    case "Minus":
    case "Equal":
    case "BracketLeft":
    case "BracketRight":
    case "Backslash":
    case "Semicolon":
    case "Quote":
    case "Comma":
    case "Period":
    case "Slash":
      return code;
    default:
      return null;
  }
}

function shortcutFromEvent(event: KeyboardEvent): string | null {
  const keyCode = normalizeShortcutCode(event.code);
  if (!keyCode) return null;
  if (["Control", "Shift", "Alt", "Meta"].includes(event.key)) return null;

  const parts: string[] = [];
  if (event.metaKey || event.ctrlKey) parts.push("CmdOrCtrl");
  if (event.shiftKey) parts.push("Shift");
  if (event.altKey) parts.push("Alt");
  parts.push(keyCode);
  return parts.join("+");
}

function setHotkeyStatus(message: string) {
  hotkeyCaptureStatus.textContent = message;
}

function renderHotkeyFields() {
  toggleHotkeyText.textContent = formatHotkey(currentSettings.toggleHotkey);
  pttHotkeyText.textContent = formatHotkey(currentSettings.pushToTalkHotkey);
  fixupHotkeyText.textContent = formatHotkey(currentSettings.fixupHotkey);
}

async function renderModelDirField() {
  // Always show a real path: the user override when set, otherwise the
  // default app-config-dir resolved server-side.
  if (currentSettings.modelDir && currentSettings.modelDir.trim().length > 0) {
    modelDirInput.value = currentSettings.modelDir;
    modelDirInput.classList.remove("is-default");
  } else {
    try {
      const def = await invoke<string>("resolved_model_dir");
      modelDirInput.value = def;
      modelDirInput.classList.add("is-default");
    } catch (e) {
      console.error("resolved_model_dir failed:", e);
      modelDirInput.value = "";
      modelDirInput.classList.remove("is-default");
    }
  }
  resetModelDirBtn.toggleAttribute(
    "disabled",
    !currentSettings.modelDir || currentSettings.modelDir.trim().length === 0,
  );
}

async function setHotkeyCaptureActive(active: boolean) {
  await invoke("set_hotkey_capture_active", { active });
}

async function startHotkeyCapture(target: "toggle" | "ptt" | "fixup") {
  hotkeyCaptureTarget = target;
  try {
    await setHotkeyCaptureActive(true);
  } catch (error) {
    hotkeyCaptureTarget = null;
    setHotkeyStatus("Unable to enter shortcut capture mode.");
    throw error;
  }
  const labels = {
    toggle: "Recording toggle hotkey. Press the new combo or Esc to cancel.",
    ptt: "Recording push-to-talk hotkey. Press the new combo or Esc to cancel.",
    fixup: "Recording fix-up hotkey. Press the new combo or Esc to cancel.",
  } as const;
  setHotkeyStatus(labels[target]);
  toggleHotkeySet.textContent = target === "toggle" ? "Press keys..." : "Set shortcut";
  pttHotkeySet.textContent = target === "ptt" ? "Press keys..." : "Set shortcut";
  fixupHotkeySet.textContent = target === "fixup" ? "Press keys..." : "Set shortcut";
}

async function stopHotkeyCapture(message = "Click a shortcut button, then press the new key combo.") {
  hotkeyCaptureTarget = null;
  try {
    await setHotkeyCaptureActive(false);
  } catch (error) {
    console.error("Failed to disable capture mode:", error);
  }
  toggleHotkeySet.textContent = "Set shortcut";
  pttHotkeySet.textContent = "Set shortcut";
  fixupHotkeySet.textContent = "Set shortcut";
  setHotkeyStatus(message);
}

function ensureDistinctHotkeys() {
  const { toggleHotkey, pushToTalkHotkey, fixupHotkey } = currentSettings;
  return (
    toggleHotkey !== pushToTalkHotkey &&
    toggleHotkey !== fixupHotkey &&
    pushToTalkHotkey !== fixupHotkey
  );
}

function ensureValidModelSelection(model: string): string {
  const available = Array.from(modelSelect.options).map((option) => option.value);
  return available.includes(model) ? model : "medium.en";
}

/// Show / hide engine-specific UI bits based on which model is currently
/// selected. Streaming captions disappear under Parakeet because the
/// `parakeet-rs` runtime doesn't expose a per-tick API today.
function refreshEngineGatedControls() {
  const engine = engineForModel(modelSelect.value);
  const streamingOk = engine === "whisper";
  streamingCaptionsRow.classList.toggle("hidden", !streamingOk);
  if (!streamingOk) {
    streamingCaptionsToggle.checked = false;
  }
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

  // Model
  currentSettings.whisperModel = ensureValidModelSelection(currentSettings.whisperModel);
  modelSelect.value = currentSettings.whisperModel;
  await checkModelStatus();
  await renderModelDirField();

  // Streaming captions + VAD
  streamingCaptionsToggle.checked = currentSettings.streamingCaptions ?? false;
  vadSilenceInput.value = String(currentSettings.vadSilenceMs ?? 800);
  refreshEngineGatedControls();

  // Hotkeys
  renderHotkeyFields();
  if (!ensureDistinctHotkeys()) {
    setHotkeyStatus("Hotkeys must be different. Update one of them before saving.");
  } else {
    setHotkeyStatus("Click a shortcut button, then press the new key combo.");
  }

  // Learning tab — toggles + storage line.
  saveTranscriptionsToggle.checked = currentSettings.saveTranscriptions;
  keepAudioClipsToggle.checked = currentSettings.keepAudioClips;
  void refreshDbHealth();
  void refreshDashboard();
  void refreshAppProfiles();
  void refreshLearningTables();
  void refreshSnippets();
}

function dashboardSinceDays(): number {
  return Number(dashboardWindowSelect?.value ?? "30");
}

function dashboardWindowLabel(): string {
  const v = dashboardSinceDays();
  if (v === 0) return "lifetime";
  if (v === 7) return "this week";
  return "this month";
}

function formatDuration(secondsTotal: number): { value: string; detail: string } {
  if (secondsTotal <= 0) {
    return { value: "0 min", detail: "Dictate something to start the clock." };
  }
  const minutes = Math.round(secondsTotal / 60);
  if (minutes < 60) {
    return { value: `${minutes} min`, detail: "" };
  }
  const hours = Math.floor(minutes / 60);
  const mins = minutes - hours * 60;
  return { value: mins > 0 ? `${hours} h ${mins} min` : `${hours} h`, detail: "" };
}

const SPEAKING_WPM = 150;
const TYPING_WPM = 45;

function timeSavedSeconds(words: number): number {
  if (words <= 0) return 0;
  // Saved = how long typing would have taken minus how long speaking did.
  return words / TYPING_WPM * 60 - words / SPEAKING_WPM * 60;
}

function renderSparkline(svg: Element, values: number[], invert = false) {
  svg.innerHTML = "";
  if (values.length === 0) return;
  const W = 320;
  const H = 60;
  const PAD = 4;
  const max = Math.max(...values, 1);
  const min = Math.min(...values, 0);
  const range = Math.max(max - min, 1e-6);
  const innerH = H - 2 * PAD;
  const innerW = W - 2 * PAD;
  const step = values.length > 1 ? innerW / (values.length - 1) : 0;
  const points = values.map((v, i) => {
    const x = PAD + i * step;
    const norm = (v - min) / range;
    const y = invert ? PAD + norm * innerH : PAD + (1 - norm) * innerH;
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  });
  const polyAttrs = `points="${points.join(" ")}"`;
  // Filled area below the line for the words/day chart.
  const lastX = (PAD + (values.length - 1) * step).toFixed(1);
  const baseY = (H - PAD).toFixed(1);
  const areaPoints = `${PAD},${baseY} ${points.join(" ")} ${lastX},${baseY}`;
  svg.innerHTML = `
    <polygon class="area" points="${areaPoints}"></polygon>
    <polyline ${polyAttrs}></polyline>
  `;
}

function rolling7Avg(buckets: DailyBucket[]): number[] {
  const out: number[] = [];
  for (let i = 0; i < buckets.length; i++) {
    const start = Math.max(0, i - 6);
    let dictations = 0;
    let edited = 0;
    for (let j = start; j <= i; j++) {
      dictations += buckets[j].dictations;
      edited += Math.round(buckets[j].edit_rate * buckets[j].dictations);
    }
    out.push(dictations > 0 ? edited / dictations : 0);
  }
  return out;
}

async function refreshDashboard() {
  const sinceDays = dashboardSinceDays();
  const label = dashboardWindowLabel();
  try {
    const [w, daily, apps] = await Promise.all([
      invoke<UsageWindow>("usage_window", { sinceDays }),
      invoke<DailyBucket[]>("daily_buckets", { days: sinceDays === 0 ? 90 : sinceDays }),
      invoke<AppBreakdown[]>("app_breakdown", { sinceDays, limit: 5 }),
    ]);

    const saved = timeSavedSeconds(w.words);
    const fmt = formatDuration(saved);
    statTimeSaved.textContent = fmt.value;
    statTimeSavedDetail.textContent = w.words > 0
      ? `Based on ${w.words.toLocaleString()} words at ${SPEAKING_WPM} wpm vs ${TYPING_WPM} wpm typing.`
      : (fmt.detail || `No dictations ${label}.`);

    statWords.textContent = w.words.toLocaleString();
    statWordsMeta.textContent = label;
    statDictations.textContent = w.dictations.toLocaleString();
    const dictMins = Math.round(w.duration_ms_total / 60000);
    statDictationsMeta.textContent = w.dictations > 0
      ? `${dictMins} min recorded`
      : label;

    const editPct = w.dictations > 0
      ? Math.round((w.edited / w.dictations) * 100)
      : 0;
    statEditRate.textContent = `${editPct}%`;

    if (apps.length > 0) {
      statTopApp.textContent = apps[0].display_name;
      statTopAppMeta.textContent = `${apps[0].words.toLocaleString()} words`;
    } else {
      statTopApp.textContent = "—";
      statTopAppMeta.textContent = `No app activity ${label}`;
    }

    renderSparkline(chartWordsDaily, daily.map(b => b.words));
    // Edit rate trend: invert so the line falls when rate falls (good).
    renderSparkline(chartEditRate, rolling7Avg(daily), true);

    appBreakdownList.innerHTML = "";
    if (apps.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      empty.textContent = `No app activity ${label}.`;
      appBreakdownList.appendChild(empty);
    } else {
      const totalWords = apps.reduce((s, a) => s + a.words, 0) || 1;
      for (const a of apps) {
        const pct = Math.max(2, Math.round((a.words / totalWords) * 100));
        const row = document.createElement("div");
        row.className = "app-bar-row";
        row.innerHTML = `
          <span class="app-name">${escapeHtml(a.display_name)}</span>
          <span class="app-bar"><span class="app-bar-fill" style="width:${pct}%"></span></span>
          <span class="app-meta">${a.words.toLocaleString()} · ${pct}%</span>
        `;
        appBreakdownList.appendChild(row);
      }
    }
  } catch (e) {
    console.error("dashboard refresh failed:", e);
  }
}

dashboardWindowSelect?.addEventListener("change", () => {
  void refreshDashboard();
});

async function refreshDbHealth() {
  try {
    const h = await invoke<DbHealth>("db_health");
    dbHealthLine.textContent = `Stored: ${h.transcriptions} transcriptions, ${h.corrections} corrections, ${h.vocabulary} vocab terms.`;
  } catch (e) {
    dbHealthLine.textContent = `DB unavailable (${e}).`;
  }
}

async function refreshLearningTables() {
  try {
    const corrections = await invoke<CorrectionRow[]>("list_top_corrections", { limit: 20 });
    topCorrectionsTable.innerHTML = "";
    if (corrections.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      empty.textContent = "No corrections recorded yet.";
      topCorrectionsTable.appendChild(empty);
    } else {
      for (const c of corrections) {
        const row = document.createElement("div");
        row.className = "learning-row";
        const sourceChip = c.source === "auto"
          ? `<span class="row-source-chip" title="Auto-learned from edits in the focused app">🤖 auto</span>`
          : "";
        row.innerHTML = `
          <span class="wrong">${escapeHtml(c.wrong || "(insertion)")}</span>
          <span class="right">${escapeHtml(c.right || "(deletion)")}${sourceChip}</span>
          <span class="meta">×${c.count} · ${escapeHtml(c.app_bundle_id ?? "any")}</span>
          <button class="btn-secondary" data-id="${c.id}">Forget</button>
        `;
        row.querySelector<HTMLButtonElement>("button")!.addEventListener("click", async () => {
          if (!window.confirm(`Forget "${c.wrong}" → "${c.right}"? It won't re-learn this session.`))
            return;
          try {
            await invoke("forget_correction", { id: c.id });
            void refreshLearningTables();
            void refreshDbHealth();
          } catch (e) {
            console.error("forget_correction failed:", e);
          }
        });
        topCorrectionsTable.appendChild(row);
      }
    }
  } catch (e) {
    console.error("list_top_corrections failed:", e);
  }

  try {
    const vocab = await invoke<VocabularyRow[]>("list_top_vocabulary", { limit: 30 });
    topVocabTable.innerHTML = "";
    if (vocab.length === 0) {
      const empty = document.createElement("div");
      empty.className = "empty-state";
      empty.textContent = "No vocabulary terms recorded yet.";
      topVocabTable.appendChild(empty);
    } else {
      for (const v of vocab) {
        const row = document.createElement("div");
        row.className = "learning-row";
        row.innerHTML = `
          <span class="term">${escapeHtml(v.term)}</span>
          <span class="meta">${escapeHtml(v.app_bundle_id ?? "global")}</span>
          <span class="meta">w ${v.weight.toFixed(2)}</span>
          <button class="btn-secondary" data-id="${v.id}">Forget</button>
        `;
        row.querySelector<HTMLButtonElement>("button")!.addEventListener("click", async () => {
          if (!window.confirm(`Forget vocab term "${v.term}"?`)) return;
          try {
            await invoke("forget_vocabulary", { id: v.id });
            void refreshLearningTables();
            void refreshDbHealth();
          } catch (e) {
            console.error("forget_vocabulary failed:", e);
          }
        });
        topVocabTable.appendChild(row);
      }
    }
  } catch (e) {
    console.error("list_top_vocabulary failed:", e);
  }
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function formatRelative(unixSec: number | null): string {
  if (!unixSec) return "never used yet";
  const diffSec = Math.max(0, Math.floor(Date.now() / 1000) - unixSec);
  if (diffSec < 60) return "just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
}

function renderAppProfile(row: AppProfileRow): HTMLElement {
  const card = document.createElement("div");
  card.className = "profile-card";
  if (!row.enabled) card.classList.add("is-disabled");
  card.dataset.bundleId = row.bundle_id;

  const modelOptions = [`<option value="">Use global default</option>`]
    .concat(
      MODELS.map(
        (m) =>
          `<option value="${m.value}"${(row.preferred_model ?? "") === m.value ? " selected" : ""}>${m.label}</option>`,
      ),
    )
    .join("");

  const postprocessOptions = POSTPROCESS_MODES.map(
    (m) =>
      `<option value="${m}"${row.postprocess_mode === m ? " selected" : ""}>${m}</option>`,
  ).join("");

  const codeCaseOptions = CODE_CASES.map(
    (c) =>
      `<option value="${c.value}"${row.code_case === c.value ? " selected" : ""}>${c.label}</option>`,
  ).join("");

  const used = row.use_count > 0 ? `${row.use_count} dictation${row.use_count === 1 ? "" : "s"}` : "no usage yet";

  card.innerHTML = `
    <div class="profile-card-header">
      <div class="profile-name">
        <strong>${escapeHtml(row.display_name)}</strong>
        <span class="bundle-id">${escapeHtml(row.bundle_id)}</span>
      </div>
      <div class="profile-meta">
        <span>${used}</span>
        <span>${formatRelative(row.last_used_at)}</span>
      </div>
    </div>
    <div class="profile-grid">
      <div class="profile-field full-width">
        <label>Vocabulary prompt</label>
        <textarea data-field="prompt_template" placeholder="e.g. TypeScript, Tauri, rusqlite, Karpathy">${escapeHtml(row.prompt_template ?? "")}</textarea>
      </div>
      <div class="profile-field">
        <label>Postprocess mode</label>
        <select data-field="postprocess_mode">${postprocessOptions}</select>
      </div>
      <div class="profile-field">
        <label>Preferred model</label>
        <select data-field="preferred_model">${modelOptions}</select>
      </div>
      <div class="profile-field">
        <label>Code identifier case</label>
        <select data-field="code_case">${codeCaseOptions}</select>
      </div>
    </div>
    <div class="profile-actions">
      <div class="left">
        <label class="switch">
          <input type="checkbox" data-field="enabled"${row.enabled ? " checked" : ""} />
          <span class="switch-track" aria-hidden="true"></span>
        </label>
        <span>Learning enabled</span>
        <label class="switch" style="margin-left:14px;">
          <input type="checkbox" data-field="auto_apply_replacements"${row.auto_apply_replacements ? " checked" : ""} />
          <span class="switch-track" aria-hidden="true"></span>
        </label>
        <span>Auto-apply replacements</span>
        <label class="switch" style="margin-left:14px;">
          <input type="checkbox" data-field="phonetic_match"${row.phonetic_match ? " checked" : ""} />
          <span class="switch-track" aria-hidden="true"></span>
        </label>
        <span>Phonetic match</span>
      </div>
      <div class="right">
        <button class="btn-secondary" data-action="reset"${row.is_persisted ? "" : " disabled"}>Reset</button>
        <button class="btn-primary" data-action="save">Save</button>
      </div>
    </div>
  `;

  const saveBtn = card.querySelector<HTMLButtonElement>('[data-action="save"]')!;
  const resetBtn = card.querySelector<HTMLButtonElement>('[data-action="reset"]')!;
  const promptEl = card.querySelector<HTMLTextAreaElement>('[data-field="prompt_template"]')!;
  const postprocessEl = card.querySelector<HTMLSelectElement>('[data-field="postprocess_mode"]')!;
  const modelEl = card.querySelector<HTMLSelectElement>('[data-field="preferred_model"]')!;
  const enabledEl = card.querySelector<HTMLInputElement>('[data-field="enabled"]')!;
  const autoApplyEl = card.querySelector<HTMLInputElement>('[data-field="auto_apply_replacements"]')!;
  const phoneticEl = card.querySelector<HTMLInputElement>('[data-field="phonetic_match"]')!;
  const codeCaseEl = card.querySelector<HTMLSelectElement>('[data-field="code_case"]')!;

  // Disable Save until the user actually changes something. Because each
  // save triggers refreshAppProfiles which rebuilds the card, the next render
  // starts clean again.
  const originalSnapshot = JSON.stringify({
    prompt_template: row.prompt_template ?? "",
    postprocess_mode: row.postprocess_mode,
    preferred_model: row.preferred_model ?? "",
    enabled: row.enabled,
    auto_apply_replacements: row.auto_apply_replacements,
    phonetic_match: row.phonetic_match,
    code_case: row.code_case,
  });
  const checkDirty = () => {
    const current = JSON.stringify({
      prompt_template: (promptEl.value || "").trim(),
      postprocess_mode: postprocessEl.value,
      preferred_model: (modelEl.value || "").trim(),
      enabled: enabledEl.checked,
      auto_apply_replacements: autoApplyEl.checked,
      phonetic_match: phoneticEl.checked,
      code_case: codeCaseEl.value,
    });
    saveBtn.disabled = current === originalSnapshot;
  };
  for (const el of [promptEl, postprocessEl, modelEl, enabledEl, autoApplyEl, phoneticEl, codeCaseEl] as HTMLElement[]) {
    el.addEventListener("input", checkDirty);
    el.addEventListener("change", checkDirty);
  }
  saveBtn.disabled = true;

  saveBtn.addEventListener("click", async () => {
    const profile: AppProfileRow = {
      bundle_id: row.bundle_id,
      display_name: row.display_name,
      prompt_template: (promptEl.value || "").trim() || null,
      postprocess_mode: postprocessEl.value,
      preferred_model: (modelEl.value || "").trim() || null,
      enabled: enabledEl.checked,
      auto_apply_replacements: autoApplyEl.checked,
      phonetic_match: phoneticEl.checked,
      code_case: codeCaseEl.value,
      last_used_at: row.last_used_at,
      use_count: row.use_count,
      is_persisted: true,
    };
    saveBtn.disabled = true;
    saveBtn.textContent = "Saving…";
    try {
      await invoke("save_app_profile", { profile });
      saveBtn.textContent = "Saved ✓";
      saveBtn.classList.add("flash-success");
      setTimeout(() => {
        void refreshAppProfiles();
      }, 700);
    } catch (e) {
      console.error("save_app_profile failed:", e);
      saveBtn.textContent = "Save";
      saveBtn.disabled = false;
      window.alert(`Failed to save profile: ${e}`);
    }
  });

  resetBtn.addEventListener("click", async () => {
    const ok = window.confirm(
      `Reset the profile for "${row.display_name}"? Saved customisations will be removed; transcription history is kept.`,
    );
    if (!ok) return;
    try {
      await invoke("delete_app_profile", { bundleId: row.bundle_id });
      void refreshAppProfiles();
    } catch (e) {
      console.error("delete_app_profile failed:", e);
      window.alert(`Failed to reset profile: ${e}`);
    }
  });

  return card;
}

function renderSnippet(row: SnippetRow): HTMLElement {
  const card = document.createElement("div");
  card.className = "profile-card";
  card.dataset.snippetId = String(row.id);

  card.innerHTML = `
    <div class="profile-card-header">
      <div class="profile-name">
        <strong>${escapeHtml(row.trigger)}</strong>
        <span class="bundle-id">${row.is_dynamic ? "dynamic" : "static"}</span>
      </div>
    </div>
    <div class="profile-grid">
      <div class="profile-field full-width">
        <label>Trigger phrase</label>
        <input type="text" data-field="trigger" value="${escapeHtml(row.trigger)}" placeholder="insert email signature" />
      </div>
      <div class="profile-field full-width">
        <label>Expansion</label>
        <textarea data-field="expansion" placeholder="Best,&#10;[your name]">${escapeHtml(row.expansion)}</textarea>
      </div>
    </div>
    <div class="profile-actions">
      <div class="left">
        <label class="switch">
          <input type="checkbox" data-field="is_dynamic"${row.is_dynamic ? " checked" : ""} />
          <span class="switch-track" aria-hidden="true"></span>
        </label>
        <span>Resolve <code>{{date}}</code> / <code>{{time}}</code> / <code>{{day}}</code> / <code>{{clipboard}}</code> / <code>{{selection}}</code></span>
      </div>
      <div class="right">
        <button class="btn-secondary" data-action="delete">Delete</button>
        <button class="btn-primary" data-action="save">Save</button>
      </div>
    </div>
  `;

  const saveBtn = card.querySelector<HTMLButtonElement>('[data-action="save"]')!;
  const deleteBtn = card.querySelector<HTMLButtonElement>('[data-action="delete"]')!;
  const triggerEl = card.querySelector<HTMLInputElement>('[data-field="trigger"]')!;
  const expansionEl = card.querySelector<HTMLTextAreaElement>('[data-field="expansion"]')!;
  const dynamicEl = card.querySelector<HTMLInputElement>('[data-field="is_dynamic"]')!;

  // Track dirty state — Save is enabled only when the form actually differs
  // from what's persisted. After save, refreshSnippets rebuilds the card so
  // the new render starts clean again.
  const originalSnapshot = JSON.stringify({
    trigger: row.trigger,
    expansion: row.expansion,
    is_dynamic: row.is_dynamic,
  });
  const checkDirty = () => {
    const current = JSON.stringify({
      trigger: triggerEl.value.trim(),
      expansion: expansionEl.value,
      is_dynamic: dynamicEl.checked,
    });
    saveBtn.disabled = current === originalSnapshot;
  };
  for (const el of [triggerEl, expansionEl, dynamicEl] as HTMLElement[]) {
    el.addEventListener("input", checkDirty);
    el.addEventListener("change", checkDirty);
  }
  saveBtn.disabled = true;

  saveBtn.addEventListener("click", async () => {
    const payload: SnippetRow = {
      id: row.id,
      trigger: triggerEl.value.trim(),
      expansion: expansionEl.value,
      is_dynamic: dynamicEl.checked,
      created_at: row.created_at,
    };
    if (!payload.trigger || !payload.expansion) {
      window.alert("Trigger and expansion are required.");
      return;
    }
    saveBtn.disabled = true;
    saveBtn.textContent = "Saving…";
    try {
      await invoke("save_snippet", { snippet: payload });
      // Show the confirmation on the *current* card (which is still in the DOM
      // because we haven't called refreshSnippets yet), wait briefly, *then*
      // refresh. The list rebuild blows the card away so timing matters.
      saveBtn.textContent = "Saved ✓";
      saveBtn.classList.add("flash-success");
      setTimeout(() => {
        void refreshSnippets();
      }, 700);
    } catch (e) {
      console.error("save_snippet failed:", e);
      window.alert(`Failed to save: ${e}`);
      saveBtn.textContent = "Save";
      saveBtn.disabled = false;
    }
  });

  deleteBtn.addEventListener("click", async () => {
    if (!window.confirm(`Delete snippet "${row.trigger}"?`)) return;
    try {
      await invoke("delete_snippet", { id: row.id });
      void refreshSnippets();
    } catch (e) {
      console.error("delete_snippet failed:", e);
      window.alert(`Failed to delete: ${e}`);
    }
  });

  return card;
}

async function refreshSnippets() {
  try {
    const rows = await invoke<SnippetRow[]>("list_snippets");
    snippetsList.innerHTML = "";
    if (rows.length === 0) {
      snippetsList.appendChild(snippetsEmpty);
      return;
    }
    for (const row of rows) {
      snippetsList.appendChild(renderSnippet(row));
    }
  } catch (e) {
    console.error("list_snippets failed:", e);
    snippetsList.innerHTML = "";
    const err = document.createElement("div");
    err.className = "empty-state";
    err.textContent = `Failed to load snippets (${e}).`;
    snippetsList.appendChild(err);
  }
}

snippetAddBtn.addEventListener("click", async () => {
  // Insert a fresh blank row at the top by creating + saving with id=0.
  const draftTrigger = window.prompt(
    "Trigger phrase (you'll say this while dictating):",
    "insert ",
  );
  if (!draftTrigger || !draftTrigger.trim()) return;
  try {
    await invoke("save_snippet", {
      snippet: {
        id: 0,
        trigger: draftTrigger.trim(),
        expansion: "(edit me)",
        is_dynamic: false,
        created_at: 0,
      },
    });
    void refreshSnippets();
  } catch (e) {
    console.error("save_snippet failed:", e);
    window.alert(`Failed to add: ${e}`);
  }
});

async function refreshAppProfiles() {
  try {
    const rows = await invoke<AppProfileRow[]>("list_app_profiles");
    appProfilesList.innerHTML = "";
    if (rows.length === 0) {
      appProfilesList.appendChild(appProfilesEmpty);
      return;
    }
    for (const row of rows) {
      appProfilesList.appendChild(renderAppProfile(row));
    }
  } catch (e) {
    console.error("list_app_profiles failed:", e);
    appProfilesList.innerHTML = "";
    const err = document.createElement("div");
    err.className = "empty-state";
    err.textContent = `Failed to load app profiles (${e}).`;
    appProfilesList.appendChild(err);
  }
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
  // currentSettings.modelDir is the source of truth — Select Folder / Reset
  // mutate it directly. The input is read-only and just shows the resolved
  // path (override or default), so we deliberately don't read it back here.
  currentSettings.saveTranscriptions = saveTranscriptionsToggle.checked;
  currentSettings.keepAudioClips = keepAudioClipsToggle.checked;
  currentSettings.streamingCaptions = streamingCaptionsToggle.checked;
  const parsedVad = Math.max(0, Math.min(2000, Math.round(Number(vadSilenceInput.value) || 0)));
  currentSettings.vadSilenceMs = parsedVad;
  vadSilenceInput.value = String(parsedVad);

  if (!ensureDistinctHotkeys()) {
    setHotkeyStatus("Hotkeys must be different. Update one of them before saving.");
    throw new Error("Hotkeys must be different");
  }

  await invoke("save_settings", { settings: currentSettings });
  renderHotkeyFields();
  renderModelDirField();
}

// Learning tab listeners
saveTranscriptionsToggle.addEventListener("change", () => {
  void saveSettings().catch((error) => console.error("Failed to save settings:", error));
});

keepAudioClipsToggle.addEventListener("change", () => {
  void saveSettings().catch((error) => console.error("Failed to save settings:", error));
});

streamingCaptionsToggle.addEventListener("change", () => {
  void saveSettings().catch((error) => console.error("Failed to save settings:", error));
});

vadSilenceInput.addEventListener("change", () => {
  void saveSettings().catch((error) => console.error("Failed to save settings:", error));
});

clearLearningBtn.addEventListener("click", async () => {
  const ok = window.confirm(
    "This will delete every saved transcription, correction, and learned vocabulary term on this machine. Continue?",
  );
  if (!ok) return;
  clearLearningBtn.disabled = true;
  try {
    await invoke("wipe_learning_data");
    await refreshDbHealth();
  } catch (e) {
    console.error("Failed to wipe learning data:", e);
    dbHealthLine.textContent = `Failed to wipe (${e}).`;
  } finally {
    clearLearningBtn.disabled = false;
  }
});

// Dashboard sits on the General tab — refresh when the user opens it so the
// stats stay live without polling.
const generalNavItem = document.querySelector<HTMLElement>('[data-section="general"]');
generalNavItem?.addEventListener("click", () => {
  void refreshDashboard();
});

// Refresh the DB-health line + tables whenever the user opens the Learning
// tab so counts stay live without polling.
const learningNavItem = document.querySelector<HTMLElement>('[data-section="learning"]');
learningNavItem?.addEventListener("click", () => {
  void refreshDbHealth();
  void refreshLearningTables();
});

const appsNavItem = document.querySelector<HTMLElement>('[data-section="apps"]');
appsNavItem?.addEventListener("click", () => {
  void refreshAppProfiles();
});

const snippetsNavItem = document.querySelector<HTMLElement>('[data-section="snippets"]');
snippetsNavItem?.addEventListener("click", () => {
  void refreshSnippets();
});

// Live refresh whenever something in the learning DB changes — emitted by the
// recorder after each transcription, by save_correction, by forget_*, etc.
void listen<number>("learning://changed", () => {
  void refreshDbHealth();
  void refreshDashboard();
  void refreshLearningTables();
  void refreshAppProfiles();
});

// Bottom-right toast stack — shared by auto-learn and `clipboard instead`.
const toastStack = document.getElementById("toast-stack")!;

function showToast(html: string, ttlMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.innerHTML = html;
  toastStack.appendChild(el);
  // Trigger the CSS transition on the next frame.
  requestAnimationFrame(() => el.classList.add("visible"));
  window.setTimeout(() => {
    el.classList.remove("visible");
    window.setTimeout(() => el.remove(), 220);
  }, ttlMs);
}

void listen<AutoLearnApplied>("auto-learn://applied", (ev) => {
  const { count, app, sample_wrong, sample_right } = ev.payload;
  const noun = count === 1 ? "correction" : "corrections";
  const sample =
    sample_wrong && sample_right
      ? `<div class="sample">${escapeHtml(sample_wrong)} → ${escapeHtml(sample_right)}</div>`
      : "";
  showToast(
    `<strong>Learned ${count} ${noun} from ${escapeHtml(app)}</strong>${sample}`,
  );
});

// Plain-text toasts emitted from the backend (e.g., `clipboard instead`).
void listen<string>("toast", (ev) => {
  if (ev.payload && ev.payload.trim().length > 0) {
    showToast(`<div>${escapeHtml(ev.payload)}</div>`);
  }
});

// Event listeners
micSelect.addEventListener("change", () => {
  void saveSettings().catch((error) => console.error("Failed to save settings:", error));
});

modelSelect.addEventListener("change", async () => {
  refreshEngineGatedControls();
  await checkModelStatus();
  void saveSettings().catch((error) => console.error("Failed to save settings:", error));
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

selectModelDirBtn.addEventListener("click", async () => {
  try {
    const startingDir = currentSettings.modelDir?.trim() || modelDirInput.value;
    const picked = await openDialog({
      directory: true,
      multiple: false,
      defaultPath: startingDir || undefined,
      title: "Pick a folder for Whisper models",
    });
    if (typeof picked === "string" && picked.trim().length > 0) {
      currentSettings.modelDir = picked;
      modelDirInput.value = picked;
      modelDirInput.classList.remove("is-default");
      await saveSettings();
    }
  } catch (error) {
    console.error("Failed to pick model directory:", error);
  }
});

resetModelDirBtn.addEventListener("click", async () => {
  // Revert to the default app-config dir.
  currentSettings.modelDir = "";
  try {
    await saveSettings();
  } catch (error) {
    console.error("Failed to reset model directory:", error);
  }
});

toggleHotkeySet.addEventListener("click", async () => {
  if (hotkeyCaptureTarget) {
    await stopHotkeyCapture();
    return;
  }
  try {
    await startHotkeyCapture("toggle");
  } catch (error) {
    console.error("Failed to start hotkey capture:", error);
  }
});

pttHotkeySet.addEventListener("click", async () => {
  if (hotkeyCaptureTarget) {
    await stopHotkeyCapture();
    return;
  }
  try {
    await startHotkeyCapture("ptt");
  } catch (error) {
    console.error("Failed to start hotkey capture:", error);
  }
});

fixupHotkeySet.addEventListener("click", async () => {
  if (hotkeyCaptureTarget) {
    await stopHotkeyCapture();
    return;
  }
  try {
    await startHotkeyCapture("fixup");
  } catch (error) {
    console.error("Failed to start hotkey capture:", error);
  }
});

document.addEventListener("keydown", async (event) => {
  if (!hotkeyCaptureTarget) return;

  event.preventDefault();
  event.stopPropagation();

  if (event.key === "Escape") {
    await stopHotkeyCapture();
    return;
  }

  if (event.repeat) {
    return;
  }

  const shortcut = shortcutFromEvent(event);
  if (!shortcut) {
    return;
  }

  const target = hotkeyCaptureTarget;
  const conflicts = [
    target !== "toggle" ? currentSettings.toggleHotkey : null,
    target !== "ptt" ? currentSettings.pushToTalkHotkey : null,
    target !== "fixup" ? currentSettings.fixupHotkey : null,
  ].filter((s): s is string => !!s);
  if (conflicts.includes(shortcut)) {
    setHotkeyStatus("That shortcut is already used by another action.");
    return;
  }

  if (target === "toggle") {
    currentSettings.toggleHotkey = shortcut;
  } else if (target === "ptt") {
    currentSettings.pushToTalkHotkey = shortcut;
  } else {
    currentSettings.fixupHotkey = shortcut;
  }

  renderHotkeyFields();

  try {
    await saveSettings();
    await stopHotkeyCapture("Shortcut saved.");
  } catch (error) {
    console.error("Failed to save hotkey:", error);
    await stopHotkeyCapture("Save failed. Try again.");
  }
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
