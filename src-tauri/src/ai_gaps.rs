//! AI gaps: speaker diarization, music beat detection, and auto-reframe.
//!
//! Every result in this module carries a `method` field that states
//! honestly how the result was produced:
//!
//! * `"heuristic"` — computed locally from signal or timing features
//!   (pause-based speaker turns, energy-based onset detection). This is
//!   not machine learning, and the result must never be described as
//!   model output.
//! * `"centre-crop"` — a geometric crop with no subject detection.
//! * `"model"` — reserved for a future local model. No function in this
//!   module returns `"model"` today.
//! * `"unavailable"` — the capability does not exist in this build.
//!
//! ## Diarization
//!
//! The local transcription backend is whisper.cpp (`whisper-cli`). It
//! returns word and segment timestamps only; it exposes no speaker
//! segmentation, and no diarization model is bundled with StudioCut AI.
//! [`diarize_words`] therefore produces pause-based speaker-turn hints:
//! a pause of at least `min_pause_secs` starts a new turn and the
//! speaker label alternates between Speaker 1 and Speaker 2. The same
//! speaker may be split across turns, and different speakers may be
//! merged into one turn. See [`diarization_capability`].
//!
//! ## Beat detection
//!
//! [`detect_beats`] extracts mono audio with FFmpeg and runs
//! energy-based onset detection in Rust (short-time RMS flux,
//! autocorrelation tempo estimation, beat-grid peak picking). No beat
//! model is involved. The reported confidence is the share of onset
//! energy that falls on the estimated beat grid; a weak or unstable
//! pulse returns `bpm: None` instead of an invented tempo.
//!
//! ## Auto-reframe
//!
//! No face-detection model is bundled with StudioCut AI (no ONNX,
//! Caffe, or GGML face model is present in the repository), so face
//! tracking is unavailable. [`reframe_crop`] computes a centre crop
//! (or a crop centred on a caller-supplied focus point) for the 9:16
//! and 1:1 targets and returns the FFmpeg `crop` filter for it. The
//! method is labelled `"centre-crop"` and the note states that no face
//! is detected. See [`reframe_capability`].
//!
//! ## Coordinator integration (main.rs — this module does not register
//! ## commands itself, because the workspace state lives in main.rs)
//!
//! Add these wrappers to `src-tauri/src/main.rs`:
//!
//! ```rust
//! /// Speaker-turn hints for a word-timestamped transcript (heuristic —
//! /// pause-based, not machine learning; see ai_gaps::diarize_clip).
//! #[tauri::command]
//! async fn detect_speakers(
//!     state: State<'_, Workspace>,
//!     project_id: uuid::Uuid,
//!     revision: u64,
//!     clip_id: uuid::Uuid,
//!     words: Vec<studiocut_ai::transcription::TimedText>,
//!     min_pause_secs: f64,
//! ) -> Result<studiocut_ai::ai_gaps::DiarizationResult, String> {
//!     let project = analysis_snapshot(&state, project_id, revision)?;
//!     tauri::async_runtime::spawn_blocking(move || {
//!         studiocut_ai::ai_gaps::diarize_clip(&project, clip_id, &words, min_pause_secs)
//!     })
//!     .await
//!     .map_err(|e| e.to_string())?
//! }
//!
//! /// Music beat detection (heuristic — energy-based onset detection,
//! /// not machine learning; see ai_gaps::detect_beats_clip).
//! #[tauri::command]
//! async fn detect_beats(
//!     state: State<'_, Workspace>,
//!     project_id: uuid::Uuid,
//!     revision: u64,
//!     clip_id: uuid::Uuid,
//! ) -> Result<studiocut_ai::ai_gaps::BeatResult, String> {
//!     let project = analysis_snapshot(&state, project_id, revision)?;
//!     tauri::async_runtime::spawn_blocking(move || {
//!         studiocut_ai::ai_gaps::detect_beats_clip(&project, clip_id)
//!     })
//!     .await
//!     .map_err(|e| e.to_string())?
//! }
//!
//! /// Centre-crop reframe plan for a portrait/square target. Face
//! /// tracking is unavailable (see ai_gaps::reframe_clip).
//! #[tauri::command]
//! async fn reframe_clip(
//!     state: State<'_, Workspace>,
//!     project_id: uuid::Uuid,
//!     revision: u64,
//!     clip_id: uuid::Uuid,
//!     target: String,
//!     focus_x: Option<f64>,
//!     focus_y: Option<f64>,
//! ) -> Result<studiocut_ai::ai_gaps::ReframePlan, String> {
//!     let project = analysis_snapshot(&state, project_id, revision)?;
//!     tauri::async_runtime::spawn_blocking(move || {
//!         studiocut_ai::ai_gaps::reframe_clip(&project, clip_id, &target, focus_x, focus_y)
//!     })
//!     .await
//!     .map_err(|e| e.to_string())?
//! }
//!
//! /// Whether local whisper.cpp exposes speaker segmentation, and which
//! /// diarization method StudioCut AI uses.
//! #[tauri::command]
//! fn diarization_capability() -> studiocut_ai::ai_gaps::DiarizationCapability {
//!     studiocut_ai::ai_gaps::diarization_capability()
//! }
//!
//! /// Whether face tracking is available for auto-reframe, and which
//! /// reframe method StudioCut AI uses.
//! #[tauri::command]
//! fn reframe_capability() -> studiocut_ai::ai_gaps::ReframeCapability {
//!     studiocut_ai::ai_gaps::reframe_capability()
//! }
//! ```
//!
//! And add these names to the `generate_handler![...]` list:
//!
//! ```text
//! detect_speakers,
//! detect_beats,
//! reframe_clip,
//! diarization_capability,
//! reframe_capability
//! ```
//!
//! Frontend invoke calls (see `src/analysis.ts` for the `snapshot`
//! shape `{ projectId, revision, clipId }`):
//!
//! ```ts
//! const speakers = await invoke<DiarizationResult>('detect_speakers', {
//!   ...snapshot,
//!   words: transcript.items,
//!   minPauseSecs: 0.8,
//! });
//! const beats = await invoke<BeatResult>('detect_beats', { ...snapshot });
//! const plan = await invoke<ReframePlan>('reframe_clip', {
//!   ...snapshot,
//!   target: '9:16',
//!   focusX: null,
//!   focusY: null,
//! });
//! const diarization = await invoke<DiarizationCapability>('diarization_capability');
//! const reframe = await invoke<ReframeCapability>('reframe_capability');
//! ```

use crate::{analysis, transcription, Project};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Method label for heuristic (non-ML) analysis results.
pub const METHOD_HEURISTIC: &str = "heuristic";
/// Method label for geometric centre-crop reframes (no face detection).
pub const METHOD_CENTRE_CROP: &str = "centre-crop";

fn round_ms(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------------------
// Speaker diarization (heuristic)
// ---------------------------------------------------------------------------

/// One speaker turn from [`diarize_words`]. `speaker` is a label
/// (`"Speaker 1"` / `"Speaker 2"`), not an identified person: turns are
/// split on pauses and the label alternates, so the same person may
/// appear under both labels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpeakerTurn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    pub word_count: usize,
}

/// Result of [`diarize_words`] / [`diarize_clip`]. `method` is always
/// `"heuristic"` in this build; see the module documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiarizationResult {
    pub method: String,
    pub turns: Vec<SpeakerTurn>,
    pub note: String,
}

/// What the local transcription backend can and cannot do for
/// diarization. `whisper_diarization_available` is `false`: whisper.cpp
/// (`whisper-cli`) returns word and segment timestamps only and exposes
/// no speaker segmentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiarizationCapability {
    pub whisper_diarization_available: bool,
    pub method: String,
    pub note: String,
}

pub fn diarization_capability() -> DiarizationCapability {
    DiarizationCapability {
        whisper_diarization_available: false,
        method: METHOD_HEURISTIC.into(),
        note: "whisper.cpp (whisper-cli) exposes no speaker segmentation: it returns word and segment timestamps only. No diarization model is bundled with StudioCut AI, so speaker turns are pause-based hints (see diarize_words), not model output."
            .into(),
    }
}

/// Pause-based speaker-turn hints for a word-timestamped transcript.
///
/// A pause of at least `min_pause_secs` (0.3–5.0 s) between words starts
/// a new turn; the speaker label alternates between Speaker 1 and
/// Speaker 2 on each turn. Words are validated the same way as
/// [`transcription::validate`]: sorted, non-overlapping, at most 12000.
///
/// This is a heuristic, not diarization: the same speaker may be split
/// across turns, and different speakers may be merged into one turn.
/// Reliable speaker labels require a diarization model, which is not
/// bundled.
pub fn diarize_words(
    words: &[transcription::TimedText],
    min_pause_secs: f64,
) -> Result<DiarizationResult, String> {
    if !min_pause_secs.is_finite() || !(0.3..=5.0).contains(&min_pause_secs) {
        return Err("Speaker-turn pause must be 0.3–5.0 seconds".into());
    }
    let duration = words.last().map(|word| word.end).unwrap_or(0.0);
    transcription::validate(
        &transcription::Transcript {
            timing: "word".into(),
            items: words.to_vec(),
        },
        duration,
    )?;
    let mut turns: Vec<SpeakerTurn> = vec![];
    let mut speaker = 1usize;
    let mut turn_start = words[0].start;
    let mut turn_end = words[0].end;
    let mut count = 1usize;
    for pair in words.windows(2) {
        let (previous, next) = (&pair[0], &pair[1]);
        if next.start - previous.end >= min_pause_secs {
            turns.push(SpeakerTurn {
                speaker: format!("Speaker {speaker}"),
                start: round_ms(turn_start),
                end: round_ms(turn_end),
                word_count: count,
            });
            speaker = if speaker == 1 { 2 } else { 1 };
            turn_start = next.start;
            count = 0;
        }
        turn_end = next.end;
        count += 1;
    }
    turns.push(SpeakerTurn {
        speaker: format!("Speaker {speaker}"),
        start: round_ms(turn_start),
        end: round_ms(turn_end),
        word_count: count,
    });
    Ok(DiarizationResult {
        method: METHOD_HEURISTIC.into(),
        turns,
        note: format!(
            "Pause-based speaker-turn hints (heuristic, not machine learning). A pause of at least {min_pause_secs} s starts a new turn and the speaker label alternates between Speaker 1 and Speaker 2. The same speaker may be split across turns, and different speakers may be merged into one turn. Reliable speaker labels require a diarization model, which is not bundled."
        ),
    })
}

/// [`diarize_words`] for a clip in the open project. The words must be a
/// word-timestamped transcript of the clip (see `transcribe_clip`);
/// segment-only transcripts cannot produce speaker turns.
pub fn diarize_clip(
    project: &Project,
    clip_id: Uuid,
    words: &[transcription::TimedText],
    min_pause_secs: f64,
) -> Result<DiarizationResult, String> {
    let (_, clip) = analysis::selected_audio(project, clip_id)?;
    let duration = clip.duration as f64 / project.settings.fps as f64;
    transcription::validate(
        &transcription::Transcript {
            timing: "word".into(),
            items: words.to_vec(),
        },
        duration,
    )?;
    diarize_words(words, min_pause_secs)
}

// ---------------------------------------------------------------------------
// Music beat detection (heuristic)
// ---------------------------------------------------------------------------

/// Result of [`detect_beats`] / [`detect_beats_clip`]. `method` is
/// always `"heuristic"` in this build. `bpm` is `None` when no stable
/// pulse was found; `confidence` is the share of onset energy that falls
/// on the estimated beat grid (0–1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeatResult {
    pub method: String,
    pub bpm: Option<f64>,
    pub confidence: f64,
    pub beats: Vec<f64>,
    pub note: String,
}

const MAX_WAV_SAMPLES: usize = 32_000_000;
const BEAT_HOP: usize = 512;
const BEAT_WINDOW: usize = 1024;

/// Decode a 16-bit PCM WAV file (mono or stereo; stereo is downmixed to
/// mono by averaging channels) into samples and its sample rate.
/// Returns an error for anything that is not PCM WAV.
pub fn parse_wav_pcm16(bytes: &[u8]) -> Result<(Vec<i16>, u32), String> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("Audio is not a WAV file".into());
    }
    let mut offset = 12usize;
    let mut channels: Option<u16> = None;
    let mut sample_rate: Option<u32> = None;
    let mut bits: Option<u16> = None;
    let mut format: Option<u16> = None;
    let mut data: Option<&[u8]> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start.saturating_add(size).min(bytes.len());
        if id == b"fmt " {
            if end - start < 16 {
                return Err("WAV format chunk is truncated".into());
            }
            format = Some(u16::from_le_bytes(
                bytes[start..start + 2].try_into().unwrap(),
            ));
            channels = Some(u16::from_le_bytes(
                bytes[start + 2..start + 4].try_into().unwrap(),
            ));
            sample_rate = Some(u32::from_le_bytes(
                bytes[start + 4..start + 8].try_into().unwrap(),
            ));
            bits = Some(u16::from_le_bytes(
                bytes[start + 14..start + 16].try_into().unwrap(),
            ));
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        // RIFF chunks are word-aligned.
        offset = start + size + (size % 2);
    }
    if format != Some(1) {
        return Err("WAV audio is not PCM".into());
    }
    let channels = channels.ok_or("WAV file has no format chunk")?;
    if channels != 1 && channels != 2 {
        return Err("WAV audio must be mono or stereo".into());
    }
    if bits != Some(16) {
        return Err("WAV audio must be 16-bit PCM".into());
    }
    let sample_rate = sample_rate.ok_or("WAV file has no format chunk")?;
    if !(1000..=192_000).contains(&sample_rate) {
        return Err("WAV sample rate is out of range".into());
    }
    let raw = data.ok_or("WAV file has no audio data")?;
    if raw.len() % 2 != 0 {
        return Err("WAV audio data has an incomplete sample".into());
    }
    let frames = raw.len() / 2 / channels as usize;
    if frames == 0 || frames > MAX_WAV_SAMPLES {
        return Err("WAV audio is empty or too long".into());
    }
    let mut samples = Vec::with_capacity(frames);
    for frame in raw.chunks_exact(2 * channels as usize) {
        let mut sum = 0i32;
        for channel in 0..channels as usize {
            let at = channel * 2;
            sum += i16::from_le_bytes(frame[at..at + 2].try_into().unwrap()) as i32;
        }
        samples.push((sum / channels as i32) as i16);
    }
    Ok((samples, sample_rate))
}

/// Short-time RMS energy flux: the positive part of the frame-to-frame
/// RMS change, one value per 512-sample hop. Onsets (beats, drum hits,
/// note attacks) appear as flux peaks.
pub fn onset_envelope(samples: &[i16], sample_rate: u32) -> Vec<f32> {
    if samples.is_empty() || sample_rate == 0 {
        return vec![];
    }
    let mut envelope = Vec::new();
    let mut previous = 0.0f32;
    let mut start = 0usize;
    while start < samples.len() {
        let end = (start + BEAT_WINDOW).min(samples.len());
        let mut sum = 0.0f64;
        for sample in &samples[start..end] {
            let value = *sample as f64 / 32768.0;
            sum += value * value;
        }
        let rms = (sum / (end - start) as f64).sqrt() as f32;
        envelope.push((rms - previous).max(0.0));
        previous = rms;
        start += BEAT_HOP;
    }
    envelope
}

/// Estimate the tempo (60–180 BPM) from an onset envelope by
/// autocorrelation. The fastest tempo whose autocorrelation score is
/// within 90% of the best score wins, which resolves the common
/// half/double-tempo ambiguity towards the faster reading. Returns
/// `None` when the envelope is too short or carries no onset energy.
pub fn estimate_bpm(flux: &[f32], hop_seconds: f64) -> Option<f64> {
    if flux.len() < 64 || !hop_seconds.is_finite() || hop_seconds <= 0.0 {
        return None;
    }
    if flux.iter().sum::<f32>() <= 0.0 {
        return None;
    }
    let mut scores: Vec<(f64, f64)> = vec![];
    let mut bpm = 60.0;
    while bpm <= 180.0 {
        let lag = ((60.0 / bpm) / hop_seconds).round() as usize;
        if lag >= 1 && lag < flux.len() {
            let mut score = 0.0f64;
            for i in lag..flux.len() {
                score += flux[i] as f64 * flux[i - lag] as f64;
            }
            scores.push((bpm, score));
        }
        bpm += 0.5;
    }
    let best = scores
        .iter()
        .map(|(_, score)| *score)
        .fold(0.0f64, f64::max);
    if best <= 0.0 {
        return None;
    }
    // The pulse must stand out from the average lag score; otherwise the
    // audio has no stable tempo (speech, ambience, rubato music).
    let mean = scores.iter().map(|(_, score)| *score).sum::<f64>() / scores.len() as f64;
    if best < 1.5 * mean {
        return None;
    }
    let fastest = scores
        .iter()
        .filter(|(_, score)| *score >= 0.9 * best)
        .map(|(bpm, _)| *bpm)
        .fold(0.0f64, f64::max);
    if fastest > 0.0 {
        Some(fastest)
    } else {
        None
    }
}

/// Pick beat times on a fixed-period grid: start at the strongest onset
/// in the first two periods, then step one period at a time, taking the
/// strongest onset within ±30% of each expected position.
pub fn pick_beats(flux: &[f32], hop_seconds: f64, period_seconds: f64) -> Vec<f64> {
    if flux.is_empty()
        || !hop_seconds.is_finite()
        || hop_seconds <= 0.0
        || !period_seconds.is_finite()
        || period_seconds <= 0.0
    {
        return vec![];
    }
    let period_frames = (period_seconds / hop_seconds).max(1.0);
    let peak = flux.iter().copied().fold(0.0f32, f32::max);
    let search = ((2.0 * period_frames) as usize).min(flux.len());
    let mut first = 0usize;
    for i in 0..search {
        if flux[i] > flux[first] {
            first = i;
        }
    }
    if flux[first] <= 0.0 {
        return vec![];
    }
    let mut beats = vec![round_ms(first as f64 * hop_seconds)];
    let mut expected = first as f64 + period_frames;
    while (expected as usize) < flux.len() && beats.len() < 10_000 {
        let low = (expected - 0.3 * period_frames).max(0.0) as usize;
        let high = ((expected + 0.3 * period_frames) as usize).min(flux.len() - 1);
        let mut best = low;
        for i in low..=high {
            if flux[i] > flux[best] {
                best = i;
            }
        }
        // Stop when the window holds no real onset (below 10% of the
        // strongest onset): the pulse has ended, and recording the
        // expected grid position would invent a beat.
        if flux[best] < 0.1 * peak {
            break;
        }
        beats.push(round_ms(best as f64 * hop_seconds));
        expected = best as f64 + period_frames;
    }
    beats
}

/// Share of total onset energy that falls on the estimated beat grid
/// (0–1). A clean, steady pulse scores near 1; speech or rubato music
/// scores near 0.
fn beat_grid_energy(flux: &[f32], beats: &[f64], hop_seconds: f64) -> f64 {
    let total: f64 = flux.iter().map(|value| *value as f64).sum();
    if total <= 0.0 || beats.is_empty() {
        return 0.0;
    }
    let mut on_grid = 0.0;
    for beat in beats {
        let frame = (beat / hop_seconds).round() as usize;
        if let Some(value) = flux.get(frame) {
            on_grid += *value as f64;
        }
    }
    (on_grid / total).clamp(0.0, 1.0)
}

/// Energy-based onset detection over raw PCM samples. No beat model is
/// involved. When the pulse is too weak or unstable (`confidence <
/// 0.25`), `bpm` is `None` and `beats` is empty rather than an invented
/// tempo.
pub fn detect_beats(samples: &[i16], sample_rate: u32) -> BeatResult {
    let hop_seconds = BEAT_HOP as f64 / sample_rate as f64;
    let flux = onset_envelope(samples, sample_rate);
    let Some(bpm) = estimate_bpm(&flux, hop_seconds) else {
        return BeatResult {
            method: METHOD_HEURISTIC.into(),
            bpm: None,
            confidence: 0.0,
            beats: vec![],
            note: "No stable pulse was detected: the audio has too little onset energy or no steady tempo (speech, ambience, or rubato music). No tempo was estimated rather than reporting an unreliable one."
                .into(),
        };
    };
    let beats = pick_beats(&flux, hop_seconds, 60.0 / bpm);
    let confidence = beat_grid_energy(&flux, &beats, hop_seconds);
    if confidence < 0.25 {
        return BeatResult {
            method: METHOD_HEURISTIC.into(),
            bpm: None,
            confidence: (confidence * 1000.0).round() / 1000.0,
            beats: vec![],
            note: "The detected pulse is too weak to trust (onset energy does not fall on a steady beat grid). No tempo was reported rather than an unreliable one."
                .into(),
        };
    }
    BeatResult {
        method: METHOD_HEURISTIC.into(),
        bpm: Some((bpm * 10.0).round() / 10.0),
        confidence: (confidence * 1000.0).round() / 1000.0,
        beats,
        note: "Energy-based onset detection (heuristic, not machine learning). The tempo is estimated from the onset envelope and may be off by a half or double factor on syncopated or sparse music; confidence is the share of onset energy on the beat grid."
            .into(),
    }
}

/// [`detect_beats`] for a clip in the open project. Audio is extracted
/// with FFmpeg as 16 kHz mono WAV (see
/// [`transcription::extract_wav`]); at most the first 10 minutes of the
/// clip are analysed.
pub fn detect_beats_clip(project: &Project, clip_id: Uuid) -> Result<BeatResult, String> {
    let (asset, clip) = analysis::selected_audio(project, clip_id)?;
    let fps = project.settings.fps as f64;
    let start = clip.source_in as f64 / fps;
    let duration = (clip.duration as f64 / fps).min(600.0);
    let wav = transcription::extract_wav(asset, start, duration)?;
    let (samples, sample_rate) = parse_wav_pcm16(&wav)?;
    Ok(detect_beats(&samples, sample_rate))
}

// ---------------------------------------------------------------------------
// Auto-reframe (centre crop; no face tracking)
// ---------------------------------------------------------------------------

/// Crop rectangle in source pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Result of [`reframe_crop`] / [`reframe_clip`]. `method` is always
/// `"centre-crop"` in this build. `ffmpeg_filter` is a ready-to-use
/// FFmpeg `crop` filter for the source (e.g.
/// `"crop=606:1080:656:0"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReframePlan {
    pub method: String,
    pub target: String,
    pub source_width: u32,
    pub source_height: u32,
    pub crop: CropRect,
    pub ffmpeg_filter: String,
    pub note: String,
}

/// What auto-reframe can and cannot do. `face_tracking_available` is
/// `false`: no face-detection model is bundled with StudioCut AI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReframeCapability {
    pub face_tracking_available: bool,
    pub method: String,
    pub note: String,
}

pub fn reframe_capability() -> ReframeCapability {
    ReframeCapability {
        face_tracking_available: false,
        method: METHOD_CENTRE_CROP.into(),
        note: "No face-detection model is bundled with StudioCut AI (no ONNX, Caffe, or GGML face model is present in the repository), so face tracking is unavailable. Reframing uses a centre crop; see reframe_crop."
            .into(),
    }
}

fn even_down(value: u32) -> u32 {
    value & !1
}

/// Centre crop (or crop centred on a caller-supplied focus point) for
/// the `"9:16"` or `"1:1"` target. Crop dimensions are rounded down to
/// even values so the result stays encodable with 4:2:0 chroma
/// subsampling.
///
/// `focus_x` / `focus_y` are normalised frame coordinates (0–1); when
/// given, the crop window is centred on the focus point as far as the
/// frame allows. No face is detected: a focus point is only a hint the
/// caller supplies (for example from a future face tracker). When the
/// source already matches the target aspect ratio, the crop is the full
/// frame.
pub fn reframe_crop(
    source_width: u32,
    source_height: u32,
    target: &str,
    focus_x: Option<f64>,
    focus_y: Option<f64>,
) -> Result<ReframePlan, String> {
    if !(16..=8192).contains(&source_width) || !(16..=8192).contains(&source_height) {
        return Err("Source dimensions must be 16–8192 pixels".into());
    }
    let aspect = match target {
        "9:16" => 9.0 / 16.0,
        "1:1" => 1.0,
        _ => return Err("Reframe target must be \"9:16\" or \"1:1\"".into()),
    };
    for (name, focus) in [("focus_x", focus_x), ("focus_y", focus_y)] {
        if let Some(value) = focus {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(format!("{name} must be 0–1"));
            }
        }
    }
    let source_aspect = source_width as f64 / source_height as f64;
    // Centre the crop window on the focus point (default: frame centre),
    // clamped so the window stays inside the frame.
    let place = |source: u32, crop: u32, focus: Option<f64>| -> u32 {
        let centre = focus
            .map(|f| f * source as f64)
            .unwrap_or(source as f64 / 2.0);
        let at = (centre - crop as f64 / 2.0).round() as i64;
        even_down(at.clamp(0, (source - crop) as i64) as u32)
    };
    let crop = if (source_aspect - aspect).abs() / aspect < 0.005 {
        CropRect {
            x: 0,
            y: 0,
            width: source_width,
            height: source_height,
        }
    } else if source_aspect > aspect {
        let height = source_height;
        let width = even_down((source_height as f64 * aspect).floor() as u32).max(2);
        CropRect {
            x: place(source_width, width, focus_x),
            y: 0,
            width,
            height,
        }
    } else {
        let width = source_width;
        let height = even_down((source_width as f64 / aspect).floor() as u32).max(2);
        CropRect {
            x: 0,
            y: place(source_height, height, focus_y),
            width,
            height,
        }
    };
    Ok(ReframePlan {
        method: METHOD_CENTRE_CROP.into(),
        target: target.into(),
        source_width,
        source_height,
        crop,
        ffmpeg_filter: format!(
            "crop={}:{}:{}:{}",
            crop.width, crop.height, crop.x, crop.y
        ),
        note: "Centre crop — face tracking unavailable. No face-detection model is bundled with StudioCut AI, so no face is detected or tracked; the crop is centred on the frame (or on the supplied focus point). Supply a focus point from a future face tracker to re-centre the crop."
            .into(),
    })
}

/// [`reframe_crop`] for a clip in the open project, using the asset's
/// recorded dimensions.
pub fn reframe_clip(
    project: &Project,
    clip_id: Uuid,
    target: &str,
    focus_x: Option<f64>,
    focus_y: Option<f64>,
) -> Result<ReframePlan, String> {
    let (asset, _) = analysis::selected_video(project, clip_id)?;
    let width = asset.width.ok_or("Selected clip has no video dimensions")?;
    let height = asset
        .height
        .ok_or("Selected clip has no video dimensions")?;
    reframe_crop(width, height, target, focus_x, focus_y)
}
