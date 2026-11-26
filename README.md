# StudioCut AI

A local-first, browser/Tauri video editor with an AI Director. Your media stays
on your machine — originals are never copied or uploaded. The AI Director works
with your own API keys (BYOK); there is no built-in key and no cloud account.

> Status: **v0.1.0, in active development.** The editor, timeline, transcription,
> AI Director and FFmpeg export work; native desktop qualification on real
> Windows/macOS/Linux hardware is still open, and releases are Linux `.deb` only
> for now. See `PLAN.md` for the milestone record and its honest limits.

## What it does

- **Projects** — versioned `.studiocut` files (schema v1–v6), atomic save/reopen,
  50-step session undo.
- **Media import** — FFprobe metadata, SQLite index, thumbnails, waveforms.
  Originals are referenced by path + fingerprint, never copied.
- **Timeline** — multi-track video/audio/caption editing: move, trim, split,
  ripple delete, mute/solo/lock, linked A/V clips, zoom, speed 0.25–4×.
- **Preview** — real video preview driven by an audio-master clock (48 kHz).
- **Analysis** — silence detection (50 ms RMS), local Whisper transcription or
  BYOK cloud transcription, kinetic captions (4 presets).
- **AI Director & Chat-to-Edit** — describe the edit in plain language; the
  Director proposes one JSON EDL (max 64 ops) which you review and approve
  before anything touches the timeline. One undo checkpoint per approval.
- **Finishing & export** — color grading, LUTs (.cube), audio mastering with
  ducking, deterministic FFmpeg export (MP4/MOV, 1080p/4K, 24/30/60 fps) with
  progress, cancel, FFprobe validation and atomic publish. Existing files are
  never overwritten.
- **License gate** — offline Ed25519 license, bound to the machine fingerprint.
  It gates only AI Director proposals and export; editing, import and
  transcription work unlicensed. Development builds bypass the gate.

## Requirements

| Piece | Why | Install |
|---|---|---|
| Node.js + npm | Frontend dev/build | Any recent LTS |
| Rust ≥ 1.77.2 | Tauri backend | rustup |
| `ffmpeg` + `ffprobe` (v6+, with libass + libx264) | Import, preview audio, export | See `SETUP_BINARIES.md` |
| `whisper-cli` + GGML `.bin` model (optional) | Local transcription | See `SETUP_BINARIES.md` |

FFmpeg location: on `PATH`, or point at it explicitly — the backend honours
`FFMPEG_PATH` and `FFPROBE_PATH` (full path to the binary). Without FFmpeg,
import and export fail with a clear error; the editor UI still opens.

## Run it

```bash
npm install
npm run dev          # web preview only (port 3000; Tauri features disabled)
npm run tauri dev    # full desktop app (dev build: license gate bypassed)
npm run build        # tsc + vite build
npm run tauri build  # release desktop build (license-gated, .deb only)
```

## Settings (in-app, Settings button)

- **API keys** — 9 providers (Gemini, OpenAI, Claude, Groq, OpenRouter,
  OpenAI-compatible, Ollama, Bedrock, Vertex). Stored in the OS credential
  manager or session-only memory; never written to project files. Env-var
  fallback: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`,
  `GROQ_API_KEY`, `OPENROUTER_API_KEY`, `AWS_BEARER_TOKEN_BEDROCK`,
  `VERTEX_ACCESS_TOKEN`.
- **AI & encoding** — preferred encoder (Auto / libx264 / NVENC / VideoToolbox /
  AMF / QSV). It is the default in the Export dialog; hardware candidates are
  probe-checked and fall back to libx264 with a disclosure.
- **Transcription** — `whisper-cli` path/command and GGML model path. Saved
  persistently and shared with the Analysis panel.
- **License** — paste a `base64payload.signature` license file to activate.

## Tests

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
```

37 tests (16 unit + 21 integration). Export/qualification tests run real
FFmpeg against generated fixtures, so they need `ffmpeg`/`ffprobe` on `PATH`
(or `FFMPEG_PATH`/`FFPROBE_PATH`) and take a while.

## Layout

```
src/            React + TypeScript UI (store, timeline, panels, modals)
src-tauri/src/  Rust backend (media, timeline, analysis, director, render, …)
src-tauri/tests/  integration tests + fixtures
PLAN.md         milestone plan + qualification evidence (read the limits, not just the ticks)
SETUP_BINARIES.md  per-OS install guide for ffmpeg / whisper.cpp
```

## Known limits (honest list)

- No native desktop verification yet on real Windows/macOS/Linux; hardware
  encoding has only been shown to fall back to libx264 on Linux.
- Finishing controls are export-only; the preview does not show grading yet.
- Portrait/square canvases are letterboxed into 16:9 on export.
- One export job at a time; max 128 clips per render.
- No autosave yet — save explicitly (`.studiocut` file).
- No license-issuing tooling in this repo; release builds need a vendor-signed
  license to run Director/export.

## License

MIT — see `LICENSE` (Copyright 2026 AlphaFrame). The bundled LUT presets in
`src-tauri/luts/` are CC0 (see `src-tauri/luts/LICENSE.txt`).
