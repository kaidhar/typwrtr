# typwrtr

`typwrtr` is a cross-platform desktop dictation app built with Tauri. It records microphone audio from a global hotkey, transcribes speech with either local `whisper.cpp` or Groq Cloud, cleans up the result, and pastes the text into the currently focused app.

## Features

- Tauri desktop shell with a Vite + TypeScript frontend
- Rust backend for microphone capture, transcription, and paste automation
- Toggle and push-to-talk recording modes using one shared global hotkey
- Local Whisper transcription through a bundled `whisper.cpp` sidecar
- Optional Groq Cloud transcription for a simple cloud-backed setup
- On-screen mic overlay positioned near the bottom center of the primary display
- Local model downloader with progress handling

## Hotkey

The default shortcut is:

- Windows: `Ctrl+Shift+Space`
- macOS: `Cmd+Shift+Space`

In `Toggle` mode, press once to start recording and press again to stop. In `Push to Talk` mode, hold the same shortcut to record and release it to transcribe.

## Prerequisites

Install the following before running the app:

- Node.js 20+
- Rust via `rustup`
- Tauri prerequisites for your OS
- Windows: Microsoft C++ Build Tools and WebView2 Runtime
- macOS: Xcode Command Line Tools

On Windows, use `npm.cmd` if PowerShell blocks plain `npm` script execution.

## Run Locally

Install dependencies:

```powershell
npm.cmd install
```

Start the desktop app:

```powershell
npm.cmd run tauri dev
```

On macOS or shells where `npm` works directly:

```bash
npm install
npm run tauri dev
```

The Tauri config starts the Vite dev server automatically at `http://localhost:1420`.

## First Launch

1. Select your microphone.
2. Choose `Local Whisper` or `Groq Cloud`.
3. For `Groq Cloud`, enter a Groq API key.
4. For `Local Whisper`, download a model from the app and make sure the `whisper.cpp` sidecar is available.

Settings and downloaded models live under the app config directory:

- Windows: `%APPDATA%\com.typwrtr.app`
- macOS: `~/Library/Application Support/com.typwrtr.app`

## Local Whisper

Local transcription needs two things:

- A model file such as `ggml-medium.en.bin`, downloaded from the app
- A `whisper.cpp` sidecar staged under `src-tauri/binaries`

Supported model choices in the UI:

- `base.en`
- `small.en`
- `small`
- `medium.en`
- `medium`
- `large-v3-turbo`
- `large-v3`

Use `medium.en` as the default local choice for English dictation. Use `small.en` for lower latency on CPU-only machines. Use `large-v3-turbo` when you want higher accuracy and your machine can handle the extra runtime cost.

## Building `whisper.cpp`

Place `whisper.cpp` next to this repository:

```text
GitHub/
  typwrtr/
  whisper.cpp/
```

The Rust build script looks for a compiled `whisper.cpp` CLI in the sibling checkout and stages it into `src-tauri/binaries` using the target-specific name Tauri expects.

Recommended paths:

- Windows + NVIDIA GPU: build `whisper.cpp` with CUDA.
- Windows CPU-only: use a CPU build or Groq Cloud.
- macOS Apple Silicon: build `whisper.cpp` with Metal.
- macOS Intel: use CPU local Whisper or Groq Cloud.

The repo does not vendor the sidecar binary or downloaded model files.

## Generated Files

These artifacts are intentionally ignored by Git:

- `node_modules/`
- `dist/`
- `src-tauri/target/`
- `src-tauri/binaries/`
- `src-tauri/gen/`
- `src-tauri/icons/android/`
- `src-tauri/icons/ios/`
- `whisper.cpp/`

Rebuild dependencies, frontend output, Tauri targets, sidecars, and models locally after cloning.

## Useful Commands

```powershell
npm.cmd run dev
npm.cmd run build
npm.cmd run tauri dev
```

```powershell
cd src-tauri
cargo check
```

## Setup Skill

A reusable setup guide is included at [docs/skill.md](docs/skill.md). Use it when setting up typwrtr on a new laptop or helping someone else build the app for their own machine.

The skill walks through the hardware-specific decisions that matter for transcription speed:

- Windows with NVIDIA GPU: use the CUDA `whisper.cpp` path.
- Windows CPU-only: use a CPU sidecar or Groq Cloud.
- macOS Apple Silicon: use the Metal `whisper.cpp` path.
- macOS Intel: use CPU local Whisper or Groq Cloud.

It also covers which model to start with, where the `whisper.cpp` sidecar should live, which generated files are ignored, and what to check when setup fails. If you are adapting typwrtr for a different laptop, read [docs/skill.md](docs/skill.md) first and follow the path that matches that machine's OS, CPU, and GPU.
