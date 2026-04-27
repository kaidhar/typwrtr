import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface FixupMatch {
  kind: "match";
  transcription: {
    id: number;
    created_at: number;
    cleaned_text: string;
    app_bundle_id: string | null;
  };
  selection: string;
}
interface FixupNoSelection {
  kind: "no-selection";
}
interface FixupNoMatch {
  kind: "no-match";
  selection: string;
}
type FixupEvent = FixupMatch | FixupNoSelection | FixupNoMatch;

const subtitle = document.getElementById("fixup-subtitle")!;
const main = document.getElementById("fixup-main")!;
const original = document.getElementById("fixup-original") as HTMLTextAreaElement;
const corrected = document.getElementById("fixup-corrected") as HTMLTextAreaElement;
const saveBtn = document.getElementById("fixup-save") as HTMLButtonElement;
const cancelBtn = document.getElementById("fixup-cancel") as HTMLButtonElement;
const status = document.getElementById("fixup-status")!;

const win = getCurrentWindow();

let active: FixupMatch | null = null;

function applyEvent(ev: FixupEvent) {
  active = null;
  saveBtn.disabled = true;
  status.textContent = "";
  if (ev.kind === "no-selection") {
    main.classList.add("hidden");
    subtitle.textContent =
      "Select the pasted text in your app first, then press the fix-up shortcut again.";
    return;
  }
  if (ev.kind === "no-match") {
    main.classList.add("hidden");
    subtitle.textContent = `No transcription within the last 30 minutes matched "${ev.selection.slice(0, 60)}${ev.selection.length > 60 ? "…" : ""}".`;
    return;
  }
  // ev.kind === "match"
  active = ev;
  main.classList.remove("hidden");
  subtitle.textContent = `Matched a transcription from ${ev.transcription.app_bundle_id ?? "unknown app"}. Edit the right side and save.`;
  original.value = ev.transcription.cleaned_text;
  corrected.value = ev.transcription.cleaned_text;
  corrected.setSelectionRange(0, corrected.value.length);
  corrected.focus();
  saveBtn.disabled = false;
}

cancelBtn.addEventListener("click", () => {
  void win.hide();
});

corrected.addEventListener("input", () => {
  if (!active) {
    saveBtn.disabled = true;
    return;
  }
  saveBtn.disabled = corrected.value.trim() === "" || corrected.value === active.transcription.cleaned_text;
});

saveBtn.addEventListener("click", async () => {
  if (!active) return;
  saveBtn.disabled = true;
  status.textContent = "Saving…";
  try {
    const applied = await invoke<number>("save_correction", {
      transcriptionId: active.transcription.id,
      finalText: corrected.value,
      cleanedText: active.transcription.cleaned_text,
      appBundleId: active.transcription.app_bundle_id,
    });
    status.textContent = `Saved. Recorded ${applied} correction${applied === 1 ? "" : "s"}.`;
    // Hide after a short beat so the user sees the confirmation.
    setTimeout(() => {
      void win.hide();
    }, 600);
  } catch (e) {
    console.error("save_correction failed:", e);
    status.textContent = `Failed: ${e}`;
    saveBtn.disabled = false;
  }
});

document.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape") {
    void win.hide();
  }
});

void listen<FixupEvent>("fixup://event", (ev) => {
  applyEvent(ev.payload);
});

// Pull whatever the hotkey handler parked while we were mounting. This avoids
// the emit-before-listener race on cold launch and on every re-show after the
// window was hidden.
async function pullPending() {
  try {
    const pending = await invoke<FixupEvent | null>("take_pending_fixup");
    if (pending) applyEvent(pending);
  } catch (e) {
    console.error("take_pending_fixup failed:", e);
  }
}

void pullPending();
void win.onFocusChanged(({ payload: focused }) => {
  if (focused) void pullPending();
});
