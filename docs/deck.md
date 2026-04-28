# typwrtr

**Dictation that learns. Locally. Free.**

A pitch deck for executives — what we built, why it beats the leaders, and what we ship next.

---

## Slide 1 — Title

> **typwrtr**
> Dictation that learns from how you actually talk.
> No subscription. No cloud. Your data stays on your machine.

**Speaker notes (≈30 s):**
"Dictation tools today fall into two camps: the cloud product everyone pays $144 a year for, and the free open-source toy that doesn't learn. We built the third option — a local-first, GPU-accelerated dictation app that watches your edits and learns your jargon, without sending a byte to anyone. In the next ten minutes I'll show why that wedge matters."

---

## Slide 2 — The problem

- Knowledge workers type ~45 wpm. Speak ~150–220 wpm.
- Existing dictation breaks one of three ways:
  - **Wrong words.** Names, jargon, in-jokes — every wrong word is a manual fix.
  - **Privacy tax.** Cloud transcription means audio + screenshots leave your machine.
  - **Subscription tax.** $12–15/month per seat just to type with your voice.
- Result: most people turn dictation off after a week.

**Speaker notes:**
"The dictation latency problem is solved — Whisper-class models are essentially perfect for clean speech. The remaining friction is *correction* and *trust*. Every product in the space leaks on at least one of these axes. typwrtr is the first that doesn't leak on any."

---

## Slide 3 — The quadrant

| | **Cloud-only** | **Local-capable** |
|---|---|---|
| **No learning** | (legacy dictation) | **Handy** (free, OSS, MIT) |
| **Learning** | **Wispr Flow** ($144/yr) | **typwrtr** *(us)* |

**Speaker notes:**
"Wispr Flow has the learning loop but it's a cloud product — no on-device mode, transcription always happens in their data centre. Handy is local and open-source but has zero learning. We sit in the empty quadrant: local + learns. That's not a small refinement, that's the unmet category."

---

## Slide 4 — What typwrtr does in one screen

```
┌──────────────┐    ┌─────────────────────────────────────┐
│  Microphone  │───▶│  whisper-rs (CUDA / Metal, in-proc) │
└──────────────┘    └────────────────┬────────────────────┘
                                     ▼
              ┌──────────────────────────────────────────┐
              │  Pipeline                                │
              │  cleanup → replacement table             │
              │       → voice commands → postprocess     │
              │       → deterministic scrub → paste      │
              └────────────────┬─────────────────────────┘
                               ▼
              ┌──────────────────────────────────────────┐
              │  Watcher                                 │
              │  reads focused field via UI Automation   │
              │  → diff vs paste → corrections + vocab   │
              │  → next dictation auto-corrects          │
              └──────────────────────────────────────────┘
```

**Speaker notes:**
"Three things happen on every dictation: transcription, cleanup, and a watcher. The watcher is the part nobody else has. It reads what's actually in the focused window, diffs it against what we pasted, and turns your edits into permanent corrections."

---

## Slide 5 — Local + GPU. Not cloud.

| | typwrtr | Wispr Flow | Handy |
|---|---|---|---|
| Where audio goes | **Your GPU** | Their data centre | Your CPU/GPU |
| Where transcripts are stored | Your SQLite (`<app_dir>/typwrtr.sqlite`) | Their servers (default) | Nowhere |
| Where post-processing runs | **In-process Rust (rules)** | Their cloud LLM | n/a (no cleanup) |
| Required for offline | Nothing | **Internet** | Nothing |
| GPU acceleration | CUDA + Metal, in-process | n/a (cloud) | CUDA + Metal |
| Engine | `whisper-rs` linking `whisper.cpp` natively | proprietary cloud | `whisper.cpp` + Parakeet V3 |

**Speaker notes:**
"Wispr Flow's privacy mode prevents *training*, but transcription itself still leaves the machine — there's no on-device option in their product. We do the same `whisper.cpp` Wispr's competitors use, GPU-accelerated, in your process. On an RTX 5070 we measured sub-200 ms decode for 5 s utterances. Post-processing is also fully on-machine — two deterministic Rust passes (collapse repeats, scrub canonical hallucinations) run after every dictation. No daemon, no API key, no cloud LLM."

---

## Slide 6 — The self-learning loop (the wedge)

**Two paths, both local:**

1. **Manual fix-up hotkey.** Select the wrong text in any app, press Ctrl+Shift+; → small window opens with what was pasted vs an editable copy. Edit, save. Done.
2. **Automatic.** After every paste, a watcher polls the focused field. When the user's edits stabilise (or focus moves elsewhere), the diff is captured and learned without any explicit save.

**What gets learned:**
- **Corrections table.** Every (wrong, right, context) triple. The replacement table fires pre-paste at `count ≥ 1` — one fix is enough; homophones get corrected before you see them on the next dictation.
- **Vocabulary table.** Proper-noun-shaped right-side tokens promoted automatically. Top 20 per-app + top 10 global appended to whisper's `initial_prompt` on every dictation, biasing the next decode.
- **Tombstones.** Click "Forget" on any row and it stays gone — no re-learning. The `count ≥ 1` threshold is safe because tombstones make every false positive recoverable in one click.

**Speaker notes:**
"This is the part Handy literally cannot do. Handy is a hotkey + Whisper, no data layer. Wispr Flow has a 'personal dictionary' that learns from corrections — but in their cloud, with their schema, that you can't audit, export, or take with you when you leave. We persist everything in SQLite under your app config dir; it's yours, it's portable, and you can `sqlite3` it whenever you want."

---

## Slide 7 — Per-app intelligence

Every focused app gets its own profile with:

- **Vocabulary prompt** — your domain jargon for VS Code vs Slack vs Gmail
- **Postprocess mode** — `default` / `markdown` / `plain` / `code`
- **Code identifier case** — `snake_case` / `camelCase` / `kebab-case` for the IDE
- **Preferred model** — small in Slack, large in your editor
- **Learning gates** — disable per app for sensitive contexts (HR tools, password managers)

**Detection:** `active-win-pos-rs` for foreground app probe. Real `CFBundleIdentifier` on macOS, exec-basename on Windows.

**Speaker notes:**
"Wispr Flow has tone adjustment per app — that's it. We have a five-axis configuration. The killer one is per-app learning gates: legal teams care that the dictation tool *doesn't* see what's in their privileged-comms app. We make that one switch."

---

## Slide 8 — Composable on top: voice commands, snippets, deterministic scrub

**Voice commands** in the same utterance:
- `new line` / `new paragraph`
- `period` / `comma` / `question mark`
- `scratch that` / `cap that` / `all caps on`
- `bullet list` / `clipboard instead` / `code mode`

**Snippets** with templating:
- `{{date}}` / `{{time}}` / `{{day}}` / `{{clipboard}}` / `{{selection}}`
- "insert email signature" → your saved sig, inline.

**Deterministic post-processing scrub.** Every dictation runs through two cheap Rust passes after voice commands and postprocess: `collapse_repeats` (kills `i i want` artifacts) and `scrub_hallucinations` (Aho-Corasick whole-line / trailing match against the canonical Whisper hallucination bag — `Thanks for watching.`, `Subtitles by the Amara.org community`, standalone `[Music]` / `♪`). O(n) per pass; runs unconditionally; no model, no daemon, no download. Replaces an earlier on-device T5 corrector that cost 3–5 s on CPU and earned its keep on a residual class of fixes (verb tense, subject-verb agreement) that turn out to be rare in deliberate single-speaker dictation.

**Speaker notes:**
"All three of these are stackable. You can dictate `code mode my new function` into VS Code and it pastes `myNewFunction` because the IDE profile is set to camelCase and the voice command activated code mode. None of our competitors compose this cleanly — Wispr Flow has voice commands, doesn't have local snippets-with-clipboard-templates; Handy has neither. And our scrub stage is the only post-processing layer in the category that runs as deterministic rules with zero latency — Wispr Flow does it in their cloud LLM, Handy doesn't do it at all. We tried an on-device T5 corrector, measured the value, and ripped it out for the rules — same observed wins, none of the cost."

---

## Slide 9 — Privacy posture

**typwrtr default install:**
- Audio: never transmitted. Optional retention to `<app_dir>/audio/`, off by default.
- Transcripts: local SQLite. Toggle off and the recorder runs end-to-end without writing.
- Post-processing: deterministic Rust passes inside the recorder. No daemon, no cloud LLM, no API keys to manage.
- Clipboard: snapshotted before every paste and restored ~120 ms later, so dictation never silently overwrites what the user had copied.
- Screenshots: never captured.

**Wispr Flow default install:**
- Audio: cloud, always.
- Transcripts: cloud, retained for "model improvement" until Privacy Mode is flipped on.
- Screenshots: **active-window screenshots taken every few seconds** as part of context-awareness. Sent to their servers.
- Certifications: SOC 2 Type II, ISO 27001 (Enterprise), HIPAA on plans with BAA.

**Handy default install:**
- Audio: never transmitted. (Same as us.)
- Transcripts: not stored at all. (No learning loop.)

**Speaker notes:**
"This is the slide for compliance / legal / IT review. Wispr Flow has the certifications; we have a fundamentally different architecture. If a buyer's risk team treats 'audio leaves the machine' as a no-go, Wispr Flow is disqualified, full stop. That's a meaningful slice of the regulated market — healthcare, finance, government contracting."

---

## Slide 10 — Cost

| | typwrtr | Wispr Flow | Handy |
|---|---|---|---|
| Free tier | full app | 2,000 words/wk | full app |
| Paid tier | n/a — free | $12/mo annual, $15/mo monthly | n/a — free |
| 100-seat year | **$0** | **$14,400** | $0 |
| 1,000-seat year | **$0** | **$144,000** | $0 |

**Speaker notes:**
"At 100 seats, choosing typwrtr over Wispr Flow saves $14,400 a year. At 1,000, $144k. We charge nothing because the model runs on the user's machine — there's no per-transcription cost for us to pass on. The business model question — and I'll come to it — is whether typwrtr ever charges, and for what."

---

## Slide 11 — Performance

Measured on a Windows 11 laptop with NVIDIA RTX 5070 Laptop GPU, `whisper-rs 0.16` with CUDA feature, `large-v3-turbo` model:

| Stage | Cold | Warm |
|---|---|---|
| Mic capture stop → resampled to 16 kHz mono | ~5 ms | ~5 ms |
| Whisper inference (5 s audio) | ~150 ms | ~80 ms |
| Cleanup + replacement + commands + postprocess | <5 ms | <5 ms |
| Native paste (CGEvent on macOS, enigo on Windows) | ~15 ms | ~15 ms |
| **End-to-end (release-of-hotkey → text on screen)** | **~250 ms** | **~150 ms** |

Plus opt-in **streaming captions overlay**: partial transcriptions every 700 ms during recording, with energy-based VAD that auto-finalises the session after configurable silence.

**Speaker notes:**
"Wispr Flow advertises *'4× faster than typing.'* Mathematically that means 180 wpm. We measured ours at the same neighbourhood, but with two advantages they can't claim: zero network latency variance, and the streaming captions show partial text *while you speak*, like a real-time autocomplete. Their cloud architecture cannot do that — every chunk has to go to their servers and back."

---

## Slide 12 — What competitors don't have

| Capability | typwrtr | Wispr Flow | Handy |
|---|---|---|---|
| Local-only by default | ✅ | ❌ | ✅ |
| GPU acceleration | ✅ CUDA + Metal | n/a | ✅ |
| Self-learning from edits | ✅ auto + manual | ✅ (cloud) | ❌ |
| Per-app profiles | ✅ 5-axis | partial (tone) | ❌ |
| Inline voice commands | ✅ | ✅ | ❌ |
| Snippets with templating | ✅ `{{vars}}` | ✅ | ❌ |
| Post-processing cleanup | ✅ deterministic Rust (no model) | cloud LLM (always-on) | ❌ |
| Streaming captions | ✅ | ❌ | ❌ |
| Zero API keys / cloud accounts required | ✅ | ❌ | ✅ |
| Open-source | ✅ MIT (planned) | ❌ proprietary | ✅ MIT |
| Forkable | ✅ | ❌ | ✅ (their explicit positioning) |
| Subscription | ❌ free | $144/yr | ❌ free |

**Speaker notes:**
"The only column where Wispr Flow wins outright is mobile — they ship iOS and Android, we don't. Everything else is either a tie or a typwrtr win. Against Handy, we win on every learning-related row because Handy's positioning is *'most forkable'*, not *'best-of-breed'*."

---

## Slide 13 — Roadmap

**Shipped (this session):**
- Phases 0–6 of the implementation plan: data layer, app context, self-learning loop, voice commands, postprocess + cleanup scrub, streaming captions, snippets.
- Auto-learn watcher with focus-change + idle-debounce trigger and false-positive guards.
- **Lever pass for accuracy** — `beam_size=5` decoding, hallucination guards (`no_speech_thold`, `suppress_nst`), per-app phonetic-match replacements via Metaphone.
- **Deterministic post-processing scrub** — `collapse_repeats` + `scrub_hallucinations` (Aho-Corasick) run on every dictation, O(n), no model. Replaces the earlier on-device T5 grammar corrector (240 MB download, 3–5 s CPU latency); kept the observed wins, dropped the cost. Three retired backends in three iterations (cloud Groq → self-hosted Ollama → on-device T5 → deterministic rules) — each `config.json` migration is one-shot and silent.
- **Replacement table threshold lowered to `count ≥ 1`** — one learned correction is enough; tombstones cover the false-positive recovery path so the threshold is safe.
- **Clipboard-safe paste** — prior clipboard snapshot + restore around the synthesised paste keystroke.
- **Module reorg** — `cleanup/`, `audio/`, `clipboard/`, `db/` split into per-domain submodules; `recorder.rs` shed its grammar-corrector field; commands + context flattened from single-file folders.
- 116 unit tests, all passing.

**Next 4 weeks:**
1. **Phase 7** — Karpathy-flavoured per-user trigram language model, rescores whisper top-k. Held-out eval determines whether it ships enabled or disabled.
2. **macOS focused-text via NSAccessibility** — auto-learn parity with Windows.
3. **Linux focused-text via AT-SPI2** — same.
4. **Code signing** — EV cert on Windows, Developer ID + notarization on macOS. Removes "unrecognised publisher" warning.

**Next quarter:**
- WER chart in the Learning tab (data is already in the DB; UI only).
- GitHub Actions release pipeline — tag → Win/Mac/Linux artifacts auto-built and signed.
- Speculative streaming paste — pipe stable partials directly into the focused app, finalise on stop. Turns dictation into real-time text, not stop-and-commit.

**Speaker notes:**
"Phase 7 is the part that earns the *'Karpathy-flavoured'* label — small model, your own data, your own loop, with a held-out eval as the kill switch. Whatever number it produces decides whether the feature ships or stays behind a flag. That's the discipline our competitors can't match because they'd never build a feature *and instrument it to disable itself if the data doesn't support it.*"

---

## Slide 14 — Asks

1. **Sign-off to keep this open-source MIT.** That's the only path that makes the *forkable, local-first* positioning real and durable.
2. **Budget for code-signing** — EV cert (~$300/yr Windows) + Apple Developer ($99/yr macOS) so the installers don't get SmartScreen / Gatekeeper warnings. Without this, distribution stalls.
3. **A pilot inside the org** — 10 seats for two weeks, measured against current dictation cost and any Wispr Flow seats already on the books. Auto-learn metrics in the SQLite layer make the eval objective.
4. **Decision on monetisation timing.** Three plausible models: enterprise self-host + support contract; an optional cloud sync layer for cross-device vocab; both. Current recommendation: stay free + OSS through Phase 7, then revisit when the WER-reduction story is provable.

**Speaker notes:**
"Three small asks, one strategic. The strategic one — when do we monetise — I'd defer until Phase 7's eval is in. If our trigram improves WER by 5% on real corpora, we have a story; if it doesn't, we don't, and we should keep this as a developer-tools play that earns goodwill rather than ARR."

---

## Slide 15 — Why this wins

> Cloud dictation tools optimise for **their** flywheel:
> more usage → more training data → better models → more usage.
>
> typwrtr optimises for **yours**:
> more usage → more corrections → better personal vocabulary → fewer corrections.
>
> Theirs gets better for everyone slowly.
> Ours gets better for **you** immediately.

**Speaker notes:**
"This is the closer. Wispr Flow's incentive is to keep your data; ours is to keep your trust. We're the only product in the category that gets meaningfully better the more *one specific user* uses it, and that improvement is fully observable, fully reversible, and never leaves their machine. That's the strategic difference, in one slide."

---

## Appendix A — One-page feature matrix (handout)

```
                      typwrtr      Wispr Flow   Handy
Local-only            ✓            ✗            ✓
GPU (CUDA/Metal)      ✓            n/a          ✓
Self-learns           ✓            ✓ (cloud)    ✗
Per-app profiles      5-axis       partial      ✗
Voice commands        ✓ (10+)      ✓            ✗
Snippets              ✓ vars       ✓            ✗
Post-process cleanup  determ. rules         cloud LLM      ✗
Streaming captions    ✓            ✗            ✗
Clipboard-safe paste  ✓            ?            ?
Zero cloud accounts   ✓            ✗            ✓
Open source           MIT          ✗            MIT
Subscription          free         $144/yr      free
```

## Appendix B — Speaker-Q&A prep

**"What about mobile?"**
> Out of scope for v1. Tauri ships mobile but the audio + UIA stack we built is desktop-class. We'd build mobile when the macOS + Linux desktop parity is shipped.

**"How do we know auto-learn isn't a privacy nightmare?"**
> The watcher only reads the field your paste went into, only when it sees a recognisable chunk of the original paste in it (anchor guard), and never sends anything off-device. The exact code path is in `src-tauri/src/recorder.rs::schedule_auto_correction_check`. Auditable.

**"Why not just buy Wispr Flow seats?"**
> $144/yr/seat, plus their architecture sends audio + screenshots to a third-party cloud. For a 1,000-person org that's $144k/yr and a SOC 2 vendor review. typwrtr is $0 and a defensible *'no audio leaves the machine'* answer for the security questionnaire.

**"How does this compare to Apple/Google built-in dictation?"**
> Both cloud-tied (Apple's offline mode is degraded). Neither learns from corrections. Neither has voice commands at the granularity we ship. They're the floor; we're the ceiling.

**"Won't whisper.cpp churn break the build?"**
> We pin `whisper-rs 0.16` and rebuild whisper.cpp + GGML + CUDA kernels from a vendored copy at compile time. We control the upgrade cadence; nothing forces us to follow upstream.

**"Karpathy what?"**
> Andrej Karpathy's been vocal about the *"small models, your own data, your own loop"* ethos. Phase 7 of our roadmap is exactly that pattern: a tiny trigram language model trained nightly on the user's own corrections, rescoring whisper's top-k beams. Cheaper than a neural rescorer and falsifiable via held-out eval.

---

## Sources

- [Handy — open-source local-first dictation (cjpais/Handy on GitHub)](https://github.com/cjpais/Handy)
- [Wispr Flow privacy posture](https://wisprflow.ai/privacy)
- [Wispr Flow homepage features](https://wisprflow.ai/)
- [Wispr Flow review with screenshot-capture critique (Voibe Resources)](https://www.getvoibe.com/resources/wispr-flow-review/)
- [Wispr Flow vs Superwhisper comparison (Voibe Resources)](https://www.getvoibe.com/resources/wispr-flow-vs-superwhisper/)
- [Wispr Flow pricing 2026 (Voibe Resources)](https://www.getvoibe.com/resources/wispr-flow-pricing/)
- [Does Wispr Flow work offline? (Weesper Neon Flow Blog)](https://weesperneonflow.ai/en/blog/2026-02-09-wispr-flow-review-cloud-dictation-2026/)
- [OpenWhispr vs Handy comparison](https://openwhispr.com/compare/handy)
