# Setup External Binaries — StudioCut AI

StudioCut is local-first: editing, preview, transcription and rendering all run on
your machine. This document explains where to place the four external pieces the
app uses — `ffmpeg`, `ffprobe`, `whisper-cli` (whisper.cpp) and its GGML models —
on Windows, macOS and Linux.

The app must be able to find each binary on `PATH`, or you can point at exact
locations:

- **ffmpeg / ffprobe** — the Rust backend spawns them by name. If they are not on
  `PATH`, export `FFMPEG_PATH` / `FFPROBE_PATH` with the absolute binary path
  before launching StudioCut.
- **whisper-cli** — configured in **Settings › Transcription** as a path or
  command; the **Check availability** button verifies it immediately.
- **GGML models** — a path to a single `.bin` model file, also set in
  **Settings › Transcription**.

## 1. ffmpeg and ffprobe

Both binaries must come from the same FFmpeg build and must be version 6 or
newer (the filtergraph compiler uses `adelay`, `sidechaincompress`, `lut3d`,
`-progress pipe:1` and libass caption burn-in).

### Windows (x86_64)

1. Download a ready build from <https://www.gyan.dev/ffmpeg/builds/> (`ffmpeg-release-essentials.zip`) or <https://github.com/BtbN/FFmpeg-Builds/releases> (`ffmpeg-master-latest-win64-gpl.zip`).
2. Unzip to a fixed location, for example `C:\StudioCut\ffmpeg` so that
   `C:\StudioCut\ffmpeg\bin\ffmpeg.exe` and `...\bin\ffprobe.exe` exist.
3. Add `C:\StudioCut\ffmpeg\bin` to `PATH`:
   *Settings › System › About › Advanced system settings › Environment Variables*,
   edit the user `Path` entry, add the folder, confirm, then restart StudioCut.
   Alternative without touching `PATH`: set user environment variables
   `FFMPEG_PATH=C:\StudioCut\ffmpeg\bin\ffmpeg.exe` and
   `FFPROBE_PATH=C:\StudioCut\ffmpeg\bin\ffprobe.exe`.

### macOS (aarch64 / x86_64)

1. Install [Homebrew](https://brew.sh) if you do not have it.
2. `brew install ffmpeg` (Apple Silicon: native arm64 bottle; Intel: supported).
3. Verify: `which ffmpeg && which ffprobe` should print
   `/opt/homebrew/bin/...` (Apple Silicon) or `/usr/local/bin/...` (Intel).
   Homebrew's location is already on `PATH`, so no extra setup is needed.

### Linux (x86_64)

Use your distribution's packages; they place both binaries on `PATH`:

```bash
# Ubuntu / Debian
sudo apt-get install -y ffmpeg
# Fedora
sudo dnf install -y ffmpeg   # enable RPM Fusion first if needed
# Arch
sudo pacman -S ffmpeg
```

Verify with `ffmpeg -version && ffprobe -version`.

## 2. whisper-cli (whisper.cpp) and GGML models

Local transcription is optional; without it the app shows a clear unavailable
state and you can still use cloud transcription with a saved API key.

### Get a binary

- **Prebuilt:** download a release for your platform from
  <https://github.com/ggml-org/whisper.cpp/releases> and unzip it somewhere
  stable (for example `C:\StudioCut\whisper`, `~/StudioCut/whisper`,
  `/opt/studiocut/whisper`).
- **Build from source:** `git clone https://github.com/ggml-org/whisper.cpp && cd whisper.cpp && cmake -B build && cmake --build build -j` — the binary lands in `build/bin/whisper-cli`.

### Get a GGML model

Download one model `.bin` from the whisper.cpp models repository, for example:

```bash
curl -L -o ~/models/ggml-base.en.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

Recommended sizes: `base.en` or `small` for speed, `medium`/`large-v3` for
accuracy. Any model you download must stay a single `.bin` file on local disk —
StudioCut passes its path to `whisper-cli` and never copies it into project data.

### Wire it up

Open **Settings › Transcription** in StudioCut and set:

- **whisper-cli path or command** — e.g. `C:\StudioCut\whisper\whisper-cli.exe`,
  `/opt/studiocut/whisper/whisper-cli`, or just `whisper-cli` if it is on `PATH`.
- **GGML model path** — the full path to the model `.bin` file.
- Press **Check availability**. A working setup reports the binary and model as
  ready; otherwise the message says exactly what is missing.

The transcription input is a 16 kHz mono WAV extracted by StudioCut with your
own FFmpeg build, so transcription quality and language support follow the
model you chose.

## 3. Verification checklist

| Check | Command / place |
| --- | --- |
| ffmpeg present | `ffmpeg -version` |
| ffprobe present | `ffprobe -version` |
| libass available for caption burn-in | `ffmpeg -filters | grep subtitles` |
| whisper-cli present | Settings › Transcription › Check availability |
| Model loads | same check, with the GGML model path set |
| Export works end-to-end | open a project, Export a short 1080p MP4, then inspect it with `ffprobe` |

## 4. Troubleshooting

- **Export fails with "ffmpeg not found"** — the binary is not on `PATH`; set
  `FFMPEG_PATH`/`FFPROBE_PATH` or install per section 1 and restart the app.
- **Captions do not burn in** — your FFmpeg build lacks libass; use a full
  build (the gyan.dev/BtbN GPL builds, Homebrew, and distro packages all have it).
- **whisper-cli check fails** — run the binary once in a terminal; the error it
  prints there (missing DLL, wrong architecture, corrupt model) is the cause.
- **Cloud providers grey out / error** — keys are set in **Settings › API keys**;
  they are stored in the OS credential manager and are never written to projects.
