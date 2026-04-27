# typwrtr

<p align="center">
  <img src="src/assets/typwrtr-logo.svg" alt="typwrtr logo" width="120" />
</p>

<p align="center">
  <strong>Speak anywhere. Transcribe locally or in the cloud. Paste into the app you are already using.</strong>
</p>

<p align="center">
  <img alt="Tauri" src="https://img.shields.io/badge/Tauri-2.x-24C8DB?logo=tauri&logoColor=white">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-backend-000000?logo=rust&logoColor=white">
  <img alt="TypeScript" src="https://img.shields.io/badge/TypeScript-frontend-3178C6?logo=typescript&logoColor=white">
  <img alt="Whisper" src="https://img.shields.io/badge/Whisper-local-0E8F6D">
  <img alt="Groq" src="https://img.shields.io/badge/Groq-cloud-F55036">
  <img alt="Windows and macOS" src="https://img.shields.io/badge/Windows%20%7C%20macOS-supported-111827">
</p>

<p align="center">
  <img src="docs/assets/typwrtr-screenshot.png" alt="typwrtr app screenshot" width="760" />
</p>

`typwrtr` is a cross-platform desktop dictation app built with Tauri. It records microphone audio from a global hotkey, transcribes speech with either local `whisper.cpp` or Groq Cloud, cleans up the result, and pastes the text into the currently focused app.

> Building this on your own laptop? Start with [docs/skill.md](docs/skill.md). It tells you which setup path to use for your OS, CPU, GPU, and model choice.

## Why It Matters

- Dictate into any focused app instead of typing manually.
- Use local Whisper when privacy and offline transcription matter.
- Use Groq Cloud when you want the fastest setup with fewer native build steps.
- Pick the right model for your machine instead of guessing.
- Keep generated binaries, models, and build artifacts out of Git.

## Choose Your Setup

| Your machine | Recommended path | Start with |
| --- | --- | --- |
| Windows + NVIDIA GPU | Local Whisper with CUDA `whisper.cpp` | `medium.en`, then try `large-v3-turbo` |
| Windows CPU-only | Groq Cloud or CPU `whisper.cpp` | Groq Cloud or `small.en` |
| macOS Apple Silicon | Local Whisper with Metal `whisper.cpp` | `medium.en` |
| macOS Intel | Groq Cloud or CPU `whisper.cpp` | Groq Cloud or `small.en` |

For the full machine-specific build flow, use the reusable setup skill: [docs/skill.md](docs/skill.md).

## Quick Start

Install prerequisites first:

- Node.js 20+
- Rust via `rustup`
- Tauri prerequisites for your OS
- Windows: Microsoft C++ Build Tools and WebView2 Runtime
- macOS: Xcode Command Line Tools

Windows:

```powershell
npm.cmd install
npm.cmd run tauri dev
```

macOS or shells where `npm` works directly:

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
5. Press the hotkey and speak into any app where text can be pasted.

Settings and downloaded models live under the app config directory:

| OS | App data path |
| --- | --- |
| Windows | `%APPDATA%\com.typwrtr.app` |
| macOS | `~/Library/Application Support/com.typwrtr.app` |

## Hotkey

The default shortcut is:

| OS | Shortcut |
| --- | --- |
| Windows | `Ctrl+Shift+Space` |
| macOS | `Cmd+Shift+Space` |

In `Toggle` mode, press once to start recording and press again to stop. In `Push to Talk` mode, hold the same shortcut to record and release it to transcribe.

## Local Whisper

Local transcription needs two things:

- A model file such as `ggml-medium.en.bin`, downloaded from the app
- A `whisper.cpp` sidecar staged under `src-tauri/binaries`

Supported model choices in the UI:

| Model | Best for |
| --- | --- |
| `base.en` | Very fast tests |
| `small.en` | CPU-only low latency |
| `small` | Multilingual low latency |
| `medium.en` | Default English dictation |
| `medium` | Multilingual balanced quality |
| `large-v3-turbo` | Higher accuracy on stronger machines |
| `large-v3` | Maximum quality when latency is acceptable |

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

## Setup Skill

[docs/skill.md](docs/skill.md) is the important build guide for this repo. Use it when you are:

- Setting up typwrtr on a new laptop.
- Helping someone else build it on different hardware.
- Choosing between local Whisper and Groq Cloud.
- Deciding whether to use CPU, NVIDIA CUDA, or Apple Silicon Metal.
- Troubleshooting sidecar, model, or hotkey issues.

The skill is designed to be followed directly by a developer or coding agent. It keeps setup decisions tied to the actual machine instead of assuming every user has the same hardware.

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

## Add A Demo Image

For a more visual GitHub page, add a screenshot at:

```text
docs/assets/typwrtr-screenshot.png
```

Then place it near the top of this README under the badges. A short screenshot of the settings screen or recording overlay is enough to help users understand the app faster.
