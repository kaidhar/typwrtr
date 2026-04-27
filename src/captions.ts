import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface PartialPayload {
  text: string;
  elapsed_ms: number;
}

const card = document.getElementById("caption-card")!;
const textEl = document.getElementById("caption-text")!;
const win = getCurrentWindow();

let hideTimer: number | null = null;

function setText(text: string, kind: "partial" | "final") {
  textEl.textContent = text;
  card.classList.remove("partial", "final");
  card.classList.add(kind);
  card.classList.add("visible");
  void win.show();
  if (hideTimer !== null) {
    window.clearTimeout(hideTimer);
    hideTimer = null;
  }
}

function scheduleHide(ms: number) {
  if (hideTimer !== null) window.clearTimeout(hideTimer);
  hideTimer = window.setTimeout(async () => {
    card.classList.remove("visible");
    // Wait for the CSS fade-out to finish before hiding the window so we
    // don't see a hard cut.
    await new Promise((r) => setTimeout(r, 200));
    void win.hide();
    hideTimer = null;
  }, ms);
}

void listen<PartialPayload>("transcription://partial", (ev) => {
  setText(ev.payload.text, "partial");
});

void listen<string>("transcription://final", (ev) => {
  if (ev.payload && ev.payload.trim().length > 0) {
    setText(ev.payload, "final");
  }
  scheduleHide(500);
});

// If the recorder explicitly reports state going back to Ready (e.g. on a
// transcription error or a no-audio cancel), hide the overlay.
void listen<string>("recording-state", (ev) => {
  if (ev.payload === "Ready") {
    scheduleHide(120);
  }
});
