# typwrtr Setup Skill

This document mirrors the Codex skill used for typwrtr setup, build, and run guidance.

## Start Here

1. Identify the OS and hardware.
2. Pick one path:
   - Windows + NVIDIA GPU: local Whisper with CUDA.
   - Windows CPU-only: Groq Cloud or local CPU Whisper.
   - macOS Apple Silicon: local Whisper with Metal or Groq Cloud.
   - macOS Intel: Groq Cloud or local CPU Whisper.
3. Prefer `medium.en` for the best speed/quality balance. Use `small.en` for faster CPU-only runs and `large-v3-turbo` only when the machine can handle the extra latency.

## Common Requirements

- Node.js 20+
- Rust via `rustup`
- Tauri prerequisites for the OS
- On Windows: Microsoft C++ Build Tools and WebView2 Runtime
- On macOS: Xcode Command Line Tools
- Keep a sibling `whisper.cpp` checkout next to `typwrtr` for local builds.

## Build and Run

- Install JS deps with `npm.cmd install` on Windows or `npm install` on macOS.
- Start dev mode with `npm.cmd run tauri dev` or `npm run tauri dev`.
- If PowerShell blocks scripts, use `npm.cmd`.
- Cloud mode only needs the Groq API key.
- Local mode needs a downloaded model and a staged `whisper.cpp` sidecar.
- Generated artifacts are intentionally ignored by Git: `dist/`, `node_modules/`, `src-tauri/binaries/`, `src-tauri/gen/`, `src-tauri/target/`, and `whisper.cpp/`. Rebuild or redownload them locally after cloning.

## Local Whisper Paths

### Windows + NVIDIA GPU

- Build `whisper.cpp` with CUDA enabled.
- Copy the produced `whisper-cli.exe` and required DLLs into `src-tauri/binaries`.
- Verify the sidecar reports the NVIDIA GPU before testing typwrtr.
- Start with `medium.en`; move to `large-v3-turbo` only if latency is acceptable.

### Windows CPU-only

- Build or stage the CPU `whisper.cpp` sidecar.
- Prefer `medium.en`, `small.en`, or Groq Cloud if latency is too high.

### macOS Apple Silicon

- Build `whisper.cpp` with Metal support.
- Stage the resulting CLI into `src-tauri/binaries`.
- Prefer `medium.en` or `small.en` for realtime dictation.

### macOS Intel

- Use CPU-only local Whisper or Groq Cloud.
- Do not assume GPU acceleration is available.

## What to Check When It Fails

- `cargo metadata` missing: reopen the shell or add `%USERPROFILE%\.cargo\bin` to PATH.
- Sidecar missing or wrong name: rebuild `whisper.cpp` and verify `src-tauri/binaries`.
- Very slow transcription: lower the model size or use the GPU/Metal path.
- Hotkey mismatch: use the Recording settings in the app and restart typwrtr after changing bindings.
- Default hotkey: `Ctrl+Shift+Space` on Windows and `Cmd+Shift+Space` on macOS.
