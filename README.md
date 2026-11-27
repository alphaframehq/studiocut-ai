# StudioCut AI

A local-first, browser/Tauri video editor with an AI Director. Your media stays
on your machine — originals are never copied or uploaded. The AI Director works
with your own API keys (BYOK); there is no built-in key and no cloud account.

> Status: **v0.1.0, in active development.** The editor, timeline, transcription,
> AI Director and FFmpeg export work; native desktop qualification on real
> Windows/macOS/Linux hardware is still open, and installers are configured
> for Linux `.deb`, Windows NSIS/MSI, and macOS `.dmg`. See `PLAN.md` for the
> milestone record and its honest limits.

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
npm run tauri build  # release desktop build (license-gated; builds the installer for the host OS)
```

## Settings (in-app, Settings button)

- **API keys** — 9 providers (Gemini, OpenAI, Claude, Groq, OpenRouter,
  OpenAI-compatible, Ollama, Bedrock, Vertex). Stored in the OS credential
  manager or session-only memory; never written to project files. Env-var
  fallback: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`,
  `GROQ_API_KEY`, `OPENROUTER_API_KEY`, `AWS_BEARER_TOKEN_BEDROCK`,
  `VERTEX_ACCESS_TOKEN`.
- **API connections** — save a base URL and key once in the API Connections
  panel (toolbar toggle, off by default), test it there, then pick it in the
  AI Director (OpenAI-compatible provider) or in a custom MCP server form.
  OpenAI-compatible APIs work immediately; any other API needs a connector
  and is out of scope for this feature. Choosing a connection is always
  explicit — nothing is switched automatically.
- **AI & encoding** — preferred encoder (Auto / libx264 / NVENC / VideoToolbox /
  AMF / QSV). It is the default in the Export dialog; hardware candidates are
  probe-checked and fall back to libx264 with a disclosure.
- **Transcription** — `whisper-cli` path/command and GGML model path. Saved
  persistently and shared with the Analysis panel.
- **License** — paste a `base64payload.signature` license file to activate.

## Release

### Installers

`npm run tauri build` builds the installer for the host operating system
only: `.deb` on Linux, NSIS/MSI on Windows, `.dmg` on macOS. Build each
platform on its native operating system (or in CI, one job per operating
system); cross-platform installer builds are not supported. The bundle
targets are configured in `src-tauri/tauri.conf.json` (`deb`, `nsis`,
`msi`, `dmg`). No code-signing identity or certificate is configured in
this repository: before distributing a build, sign it with the platform
identity (Apple Developer ID with notarization on macOS; a code-signing
certificate on Windows) and record the identity used.

### Issuing a license

Release builds require an offline Ed25519 license signed by the vendor
key; development builds bypass the gate. Licenses are issued with the
`license-tool` binary (`src-tauri/src/bin/license-tool.rs`):

```bash
cargo run --locked --release --bin license-tool -- generate-key
cargo run --locked --release --bin license-tool -- issue --seed <hex-seed> --name "Jane Doe" --email jane@example.com --fingerprint <machine-fingerprint> --expires never
cargo run --locked --release --bin license-tool -- verify --license <license-text> --fingerprint <machine-fingerprint> --public-key <hex-public-key>
```

`generate-key` prints a new private seed and the matching public key.
Store the private seed offline and never commit it. Paste the printed
public key into `LICENSE_PUBLIC_KEY` in `src-tauri/src/license.rs` and
rebuild before issuing licenses with the new key; licenses signed with a
previous key stop verifying after the public key is replaced, so plan key
rotation before any public release. The machine fingerprint is shown in
the application under Settings › License; a license is bound to that
single machine. `--expires` accepts `never` or a Unix timestamp in
seconds. On Windows, `generate-key` needs `--seed` with 32 random bytes
(for example from `openssl rand -hex 32`); on Linux and macOS it reads
`/dev/urandom`.

### Updates

The updater is configured in `src-tauri/tauri.conf.json` under
`plugins.updater`, but the configuration is a placeholder and must be
replaced before any release that offers updates:

- `endpoints` points at `https://example.invalid/studiocut-updates/...`
  (`example.invalid` never resolves). Replace it with the real update
  server or static manifest URL.
- `pubkey` is the literal string `REPLACE_WITH_UPDATER_PUBLIC_KEY`.
  Replace it with the public key from `tauri signer generate`.
- Set `bundle.createUpdaterArtifacts` to `true` and provide the private
  signing key as the `TAURI_SIGNING_PRIVATE_KEY` environment variable at
  build time; without it, the build fails. Never commit the private key.

The updater plugin (`tauri-plugin-updater`, with `tauri-plugin-process`
for restart) is not yet wired into the build: add both crates to
`src-tauri/Cargo.toml`, register the plugin in `src-tauri/src/main.rs`,
and grant the `updater` capability in `src-tauri/capabilities/main.json`.
The plugin requires Rust 1.90 or later, while this crate currently
declares Rust 1.77.2.

### Still manual before release

- Desktop verification on real Windows, macOS, and Linux hardware
  (install, launch, license activation, Director, export).
- Code signing and, on macOS, notarization.
- The first published update served from the real update endpoint, with
  the signature verified by a fresh install.

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
- Release builds need a vendor-signed license to run Director/export;
  see the Release section for the issuing tool.
- The updater endpoint and public key in `src-tauri/tauri.conf.json` are
  placeholders; they must be replaced before any release that offers
  updates.

## License

MIT — see `LICENSE` (Copyright 2026 AlphaFrame). The bundled LUT presets in
`src-tauri/luts/` are CC0 (see `src-tauri/luts/LICENSE.txt`).
