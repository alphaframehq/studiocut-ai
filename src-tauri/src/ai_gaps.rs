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
//! * `"motion-weighted"` — a crop centred on the highest-motion region
//!   (see [`reframe_motion_clip`]); motion is a proxy for the subject,
//!   not subject detection.
//! * `"face-tracked"` — a crop whose centre follows a detected face
//!   over time (see [`reframe_face_track_clip`]).
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
//! a pause of at least `min_pause_secs` starts a new turn. When the
//! clip audio is available, [`diarize_clip`] also measures per-word
//! loudness (RMS energy from the extracted 16 kHz mono WAV, the same
//! extraction [`detect_beats_clip`] uses) and each turn receives a
//! confidence score from the pause length, the loudness contrast with
//! the neighbouring turn, and the turn length. A turn whose confidence
//! is below [`DIARIZATION_CONFIDENCE_THRESHOLD`] (0.5) is reported
//! with `speaker: null` ("unknown speaker") instead of an invented
//! label; the label only alternates across turns confident enough to
//! carry one. The same speaker may still be split across turns, and
//! different speakers may be merged into one turn. See
//! [`diarization_capability`].
//!
//! ## Beat detection
//!
//! [`detect_beats`] extracts mono audio with FFmpeg and runs
//! energy-based onset detection in Rust (short-time RMS flux, weighted
//! by frame loudness so onsets in quiet passages count for less,
//! autocorrelation tempo estimation, beat-grid peak picking). No beat
//! model is involved. The reported confidence combines the share of
//! onset energy that falls on the estimated beat grid with the
//! stability of the tempo estimate (autocorrelation prominence); a
//! weak or unstable pulse returns `bpm: None` instead of an invented
//! tempo.
//!
//! ## Auto-reframe
//!
//! [`reframe_crop`] computes a centre crop (or a crop centred on a
//! caller-supplied focus point) for the 9:16 and 1:1 targets and
//! returns the FFmpeg `crop` filter for it. The method is labelled
//! `"centre-crop"` and the note states that no face is detected by the
//! crop itself. [`reframe_motion_clip`] offers a motion-weighted
//! alternative: it compares two frames 0.5 s apart, finds the region
//! with the strongest luma change, and centres the crop there
//! (method `"motion-weighted"`). When the frames are effectively
//! static it falls back to the centre crop and says so. Motion is a
//! proxy for the subject, not subject detection: a moving background
//! can pull the crop away from a still subject. See
//! [`reframe_capability`].
//!
//! [`reframe_face_track_clip`] is the face-tracked reframe: it samples
//! frames across the clip, runs the bundled BlazeFace full-range
//! model (see [`crate::face`]) on each sample, selects the one face
//! that appears consistently, smooths its track, and returns a plan
//! (method `"face-tracked"`) whose crop centre follows the face over
//! time. When no face is found consistently, the plan falls back to
//! the motion-weighted reframe and says so — it never claims a face
//! was tracked when it was not.
//!
//! YuNet and UltraFace were considered as face-detection candidates
//! and rejected as the primary model: both are trained on WIDER FACE
//! (CC BY-NC-ND 4.0, academic-research-only terms), so whether the
//! dataset terms encumber their MIT-licensed weights for a commercial
//! open-source app is unsettled, and neither is unambiguously
//! license-clean for this use. An earlier revision of this document
//! called them license-clean; that was inaccurate and is corrected
//! here. BlazeFace was chosen instead: it is Apache-2.0 licensed and
//! was trained on consented images, not on WIDER FACE. It runs under
//! `tract` (pure Rust, CPU-only) from a small ONNX file converted
//! in-house from the official `.tflite` — see
//! `assets/face-detection/README.md`.
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
//! /// Motion-weighted reframe plan for a portrait/square target: the
//! /// crop follows the highest-motion region at the playhead instead
//! /// of the static centre, falling back to the centre crop when the
//! /// frames are static. Face tracking is available separately (see
//! /// ai_gaps::reframe_face_track_clip).
//! #[tauri::command]
//! async fn reframe_motion_clip(
//!     state: State<'_, Workspace>,
//!     project_id: uuid::Uuid,
//!     revision: u64,
//!     clip_id: uuid::Uuid,
//!     target: String,
//!     at_seconds: f64,
//! ) -> Result<studiocut_ai::ai_gaps::ReframePlan, String> {
//!     let project = analysis_snapshot(&state, project_id, revision)?;
//!     tauri::async_runtime::spawn_blocking(move || {
//!         studiocut_ai::ai_gaps::reframe_motion_clip(&project, clip_id, &target, at_seconds)
//!     })
//!     .await
//!     .map_err(|e| e.to_string())?
//! }
//!
//! /// Face-tracked reframe plan for a portrait/square target: the crop
//! /// centre follows the one face detected consistently across the clip
//! /// (see ai_gaps::reframe_face_track_clip). When no face is found
//! /// consistently, the plan falls back to the motion-weighted reframe
//! /// and says so.
//! #[tauri::command]
//! async fn reframe_face_track(
//!     state: State<'_, Workspace>,
//!     project_id: uuid::Uuid,
//!     revision: u64,
//!     clip_id: uuid::Uuid,
//!     target: String,
//! ) -> Result<studiocut_ai::ai_gaps::ReframePlan, String> {
//!     let project = analysis_snapshot(&state, project_id, revision)?;
//!     tauri::async_runtime::spawn_blocking(move || {
//!         studiocut_ai::ai_gaps::reframe_face_track_clip(&project, clip_id, &target)
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
//! reframe_motion_clip,
//! reframe_face_track,
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
//! const motionPlan = await invoke<ReframePlan>('reframe_motion_clip', {
//!   ...snapshot,
//!   target: '9:16',
//!   atSeconds: 0,
//! });
//! const facePlan = await invoke<ReframePlan>('reframe_face_track', {
//!   ...snapshot,
//!   target: '9:16',
//! });
//! const diarization = await invoke<DiarizationCapability>('diarization_capability');
//! const reframe = await invoke<ReframeCapability>('reframe_capability');
//! ```

use crate::{analysis, face, media, scopes, transcription, Project};
use serde::{Deserialize, Serialize};
use std::{io::Read, time::Duration};
use uuid::Uuid;

/// Method label for heuristic (non-ML) analysis results.
pub const METHOD_HEURISTIC: &str = "heuristic";
/// Method label for geometric centre-crop reframes (no face detection).
pub const METHOD_CENTRE_CROP: &str = "centre-crop";
/// Method label for motion-weighted reframes (no face detection: the
/// crop follows the highest-motion region, not a detected subject).
pub const METHOD_MOTION_WEIGHTED: &str = "motion-weighted";
/// Method label for face-tracked reframes: the crop centre follows
/// the one face detected consistently across the clip (see
/// [`reframe_face_track_clip`]).
pub const METHOD_FACE_TRACKED: &str = "face-tracked";
/// Minimum turn confidence for a speaker label to be reported.
/// Below this threshold the turn's `speaker` is `null` ("unknown
/// speaker") rather than an invented label.
pub const DIARIZATION_CONFIDENCE_THRESHOLD: f64 = 0.5;

fn round_ms(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------------------
// Speaker diarization (heuristic)
// ---------------------------------------------------------------------------

/// One speaker turn from [`diarize_words`]. `speaker` is a label
/// (`"Speaker 1"` / `"Speaker 2"`) or `None` when the turn is not
/// confident enough to carry a label (see
/// [`DIARIZATION_CONFIDENCE_THRESHOLD`]). Labels are indices in order
/// of first appearance, not identified people: turns are split on
/// pauses and the label alternates across confident turns, so the same
/// person may appear under both labels and different speakers may be
/// merged into one turn. `confidence` (0–1) scores how much evidence
/// the pause length, the loudness contrast with the neighbouring
/// turn, and the turn length give that this turn starts a new speaker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpeakerTurn {
    pub speaker: Option<String>,
    pub start: f64,
    pub end: f64,
    pub word_count: usize,
    pub confidence: f64,
}

/// Result of [`diarize_words`] / [`diarize_clip`]. `method` is always
/// `"heuristic"` in this build; see the module documentation.
/// `confidence` is the mean turn confidence (0–1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiarizationResult {
    pub method: String,
    pub turns: Vec<SpeakerTurn>,
    pub confidence: f64,
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

/// Pause length scored 0–1: a pause at the threshold scores 0.5, a
/// pause twice the threshold (or longer) scores 1.
fn pause_score(pause_secs: f64, min_pause_secs: f64) -> f64 {
    (pause_secs / (2.0 * min_pause_secs)).clamp(0.0, 1.0)
}

/// Loudness contrast between neighbouring turns, scored 0–1: a
/// difference of 0.5 (half the normalised loudness range) or more
/// scores 1.
fn energy_contrast_score(contrast: f64) -> f64 {
    (contrast / 0.5).clamp(0.0, 1.0)
}

/// Turn length scored 0–1: eight words or more scores 1.
fn length_score(word_count: usize) -> f64 {
    (word_count as f64 / 8.0).clamp(0.0, 1.0)
}

/// Per-word loudness (RMS energy, normalised 0–1 by the loudest word)
/// for a word-timestamped transcript over mono PCM samples. Words
/// outside the audio range score 0. This is the loudness data the
/// diarization confidence uses; it is a level measurement, not speaker
/// identification.
pub fn word_energies(
    samples: &[i16],
    sample_rate: u32,
    words: &[transcription::TimedText],
) -> Vec<f32> {
    if samples.is_empty() || sample_rate == 0 || words.is_empty() {
        return vec![0.0; words.len()];
    }
    let mut rms_values = Vec::with_capacity(words.len());
    let mut peak = 0.0f64;
    for word in words {
        let start =
            ((word.start.max(0.0) * sample_rate as f64).floor() as usize).min(samples.len());
        let end = ((word.end.max(0.0) * sample_rate as f64).ceil() as usize).min(samples.len());
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for sample in &samples[start..end] {
            let value = *sample as f64 / 32768.0;
            sum += value * value;
            count += 1;
        }
        let rms = if count == 0 {
            0.0
        } else {
            (sum / count as f64).sqrt()
        };
        peak = peak.max(rms);
        rms_values.push(rms);
    }
    if peak <= 0.0 {
        return vec![0.0; words.len()];
    }
    rms_values
        .into_iter()
        .map(|rms| (rms / peak) as f32)
        .collect()
}

/// Pause-based speaker-turn hints for a word-timestamped transcript.
///
/// A pause of at least `min_pause_secs` (0.3–5.0 s) between words starts
/// a new turn. Each turn receives a confidence score (0–1) from the
/// pause length, the loudness contrast with the neighbouring turn, and
/// the turn length; when per-word loudness is supplied in `energies`
/// (one normalised 0–1 value per word, see [`word_energies`]), the
/// contrast term is used, otherwise confidence uses pause length and
/// turn length only. A turn whose confidence is below
/// [`DIARIZATION_CONFIDENCE_THRESHOLD`] (0.5) is reported with
/// `speaker: null` instead of an invented label, and the speaker label
/// only alternates across turns confident enough to carry one. Words
/// are validated the same way as [`transcription::validate`]: sorted,
/// non-overlapping, at most 12000.
///
/// This is a heuristic, not diarization: the same speaker may be split
/// across turns, and different speakers may be merged into one turn.
/// Reliable speaker labels require a diarization model, which is not
/// bundled.
pub fn diarize_words_with_energy(
    words: &[transcription::TimedText],
    min_pause_secs: f64,
    energies: &[f32],
) -> Result<DiarizationResult, String> {
    if !min_pause_secs.is_finite() || !(0.3..=5.0).contains(&min_pause_secs) {
        return Err("Speaker-turn pause must be 0.3–5.0 seconds".into());
    }
    if !energies.is_empty() && energies.len() != words.len() {
        return Err("Per-word loudness must be empty or one value per word".into());
    }
    let duration = words.last().map(|word| word.end).unwrap_or(0.0);
    transcription::validate(
        &transcription::Transcript {
            timing: "word".into(),
            items: words.to_vec(),
        },
        duration,
    )?;
    // Split into turns, recording the pause before each turn after the
    // first and the word index range of each turn.
    let mut ranges: Vec<(usize, usize, Option<f64>)> = vec![];
    let mut start = 0usize;
    let mut pause_before: Option<f64> = None;
    for (index, pair) in words.windows(2).enumerate() {
        let (previous, next) = (&pair[0], &pair[1]);
        if next.start - previous.end >= min_pause_secs {
            ranges.push((start, index + 1, pause_before));
            start = index + 1;
            pause_before = Some(next.start - previous.end);
        }
    }
    ranges.push((start, words.len(), pause_before));
    let mean_energy = |range: (usize, usize, Option<f64>)| -> f64 {
        if energies.is_empty() || range.1 <= range.0 {
            return 0.0;
        }
        let slice = &energies[range.0..range.1];
        slice.iter().map(|value| *value as f64).sum::<f64>() / slice.len() as f64
    };
    let turn_energies: Vec<f64> = ranges.iter().map(|range| mean_energy(*range)).collect();
    let mut turns: Vec<SpeakerTurn> = vec![];
    let mut next_label = 1usize;
    for (index, (first, last, pause)) in ranges.iter().enumerate() {
        let word_count = last - first;
        let length = length_score(word_count);
        let confidence = if index == 0 {
            // The first turn has no pause before it: its boundary
            // evidence is the loudness contrast with the following
            // turn when loudness is available, and neutral (0.5) when
            // it is not.
            let boundary = if !energies.is_empty() && ranges.len() > 1 {
                energy_contrast_score((turn_energies[0] - turn_energies[1]).abs())
            } else {
                0.5
            };
            0.5 * boundary + 0.5 * length
        } else if !energies.is_empty() {
            let contrast = (turn_energies[index] - turn_energies[index - 1]).abs();
            0.45 * pause_score(pause.unwrap_or(0.0), min_pause_secs)
                + 0.35 * energy_contrast_score(contrast)
                + 0.20 * length
        } else {
            0.65 * pause_score(pause.unwrap_or(0.0), min_pause_secs) + 0.35 * length
        };
        let confidence = round_ms(confidence.clamp(0.0, 1.0));
        let speaker = if confidence >= DIARIZATION_CONFIDENCE_THRESHOLD {
            let label = format!("Speaker {next_label}");
            next_label = if next_label == 1 { 2 } else { 1 };
            Some(label)
        } else {
            None
        };
        turns.push(SpeakerTurn {
            speaker,
            start: round_ms(words[*first].start),
            end: round_ms(words[*last - 1].end),
            word_count,
            confidence,
        });
    }
    let overall = if turns.is_empty() {
        0.0
    } else {
        round_ms(turns.iter().map(|turn| turn.confidence).sum::<f64>() / turns.len() as f64)
    };
    let energy_note = if energies.is_empty() {
        " Loudness data was not available, so confidence uses pause length and turn length only."
    } else {
        " Confidence also uses the loudness contrast between neighbouring turns."
    };
    Ok(DiarizationResult {
        method: METHOD_HEURISTIC.into(),
        turns,
        confidence: overall,
        note: format!(
            "Pause-based speaker-turn hints (heuristic, not machine learning). A pause of at least {min_pause_secs} s starts a new turn. Each turn carries a confidence score from the pause length, the loudness contrast with the neighbouring turn, and the turn length; a turn below {DIARIZATION_CONFIDENCE_THRESHOLD} confidence is reported with no speaker label instead of an invented one, and the label only alternates across confident turns. The same speaker may be split across turns, and different speakers may be merged into one turn. Reliable speaker labels require a diarization model, which is not bundled.{energy_note}"
        ),
    })
}

/// [`diarize_words_with_energy`] without loudness data: confidence
/// uses pause length and turn length only.
pub fn diarize_words(
    words: &[transcription::TimedText],
    min_pause_secs: f64,
) -> Result<DiarizationResult, String> {
    diarize_words_with_energy(words, min_pause_secs, &[])
}

/// [`diarize_words`] for a clip in the open project. The words must be a
/// word-timestamped transcript of the clip (see `transcribe_clip`);
/// segment-only transcripts cannot produce speaker turns. When the
/// clip is at most 10 minutes long, its audio is extracted (the same
/// 16 kHz mono WAV extraction [`detect_beats_clip`] uses) and per-word
/// loudness feeds the turn confidence; when extraction fails, or the
/// clip is longer, the result falls back to pause-only confidence and
/// the note says so.
pub fn diarize_clip(
    project: &Project,
    clip_id: Uuid,
    words: &[transcription::TimedText],
    min_pause_secs: f64,
) -> Result<DiarizationResult, String> {
    let (asset, clip) = analysis::selected_audio(project, clip_id)?;
    let fps = project.settings.fps as f64;
    let duration = clip.duration as f64 / fps;
    transcription::validate(
        &transcription::Transcript {
            timing: "word".into(),
            items: words.to_vec(),
        },
        duration,
    )?;
    let start = clip.source_in as f64 / fps;
    let energies = if duration <= 600.0 {
        transcription::extract_wav(asset, start, duration)
            .ok()
            .and_then(|wav| parse_wav_pcm16(&wav).ok())
            .map(|(samples, sample_rate)| word_energies(&samples, sample_rate, words))
    } else {
        None
    };
    let mut result = match &energies {
        Some(values) => diarize_words_with_energy(words, min_pause_secs, values)?,
        None => diarize_words(words, min_pause_secs)?,
    };
    if energies.is_none() {
        result.note.push_str(
            " Clip audio could not be extracted for loudness measurement, so confidence uses pause length and turn length only.",
        );
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Music beat detection (heuristic)
// ---------------------------------------------------------------------------

/// Result of [`detect_beats`] / [`detect_beats_clip`]. `method` is
/// always `"heuristic"` in this build. `bpm` is `None` when no stable
/// pulse was found; `confidence` (0–1) is the mean of the share of
/// onset energy that falls on the estimated beat grid and the tempo
/// stability (see [`estimate_bpm_with_prominence`]); `tempo_stability`
/// is the autocorrelation prominence of the winning tempo, mapped
/// 0–1. When `confidence` is below 0.25, `bpm` is `None` and `beats`
/// is empty rather than an invented tempo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeatResult {
    pub method: String,
    pub bpm: Option<f64>,
    pub confidence: f64,
    pub tempo_stability: f64,
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

/// Loudness-weighted onset envelope: the [`onset_envelope`] flux with
/// each frame scaled by that frame's RMS relative to the loudest frame
/// (0–1). Onsets in loud passages count at full weight; onsets in
/// quiet passages — where background noise and speech sibilance
/// produce spurious flux — count for less. The weighting uses only
/// the loudness already measured for the envelope, so no new audio
/// analysis is needed.
pub fn energy_weighted_flux(samples: &[i16], sample_rate: u32) -> Vec<f32> {
    if samples.is_empty() || sample_rate == 0 {
        return vec![];
    }
    let mut rms_frames = Vec::new();
    let mut start = 0usize;
    while start < samples.len() {
        let end = (start + BEAT_WINDOW).min(samples.len());
        let mut sum = 0.0f64;
        for sample in &samples[start..end] {
            let value = *sample as f64 / 32768.0;
            sum += value * value;
        }
        rms_frames.push((sum / (end - start) as f64).sqrt() as f32);
        start += BEAT_HOP;
    }
    let peak = rms_frames.iter().copied().fold(0.0f32, f32::max);
    if peak <= 0.0 {
        return vec![0.0; rms_frames.len()];
    }
    let flux = onset_envelope(samples, sample_rate);
    flux.into_iter()
        .zip(rms_frames)
        .map(|(value, rms)| value * (rms / peak))
        .collect()
}

/// Estimate the tempo (60–180 BPM) from an onset envelope by
/// autocorrelation, with the prominence of the winning tempo (best lag
/// score divided by the mean lag score). The fastest tempo whose
/// autocorrelation score is within 90% of the best score wins, which
/// resolves the common half/double-tempo ambiguity towards the faster
/// reading. Returns `None` when the envelope is too short, carries no
/// onset energy, or the pulse does not stand out from the average lag
/// score (prominence below 1.5: speech, ambience, rubato music).
pub fn estimate_bpm_with_prominence(flux: &[f32], hop_seconds: f64) -> Option<(f64, f64)> {
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
    if mean <= 0.0 || best < 1.5 * mean {
        return None;
    }
    let fastest = scores
        .iter()
        .filter(|(_, score)| *score >= 0.9 * best)
        .map(|(bpm, _)| *bpm)
        .fold(0.0f64, f64::max);
    if fastest > 0.0 {
        Some((fastest, best / mean))
    } else {
        None
    }
}

/// Estimate the tempo (60–180 BPM) from an onset envelope by
/// autocorrelation. See [`estimate_bpm_with_prominence`].
pub fn estimate_bpm(flux: &[f32], hop_seconds: f64) -> Option<f64> {
    estimate_bpm_with_prominence(flux, hop_seconds).map(|(bpm, _)| bpm)
}

/// Tempo stability (0–1) from autocorrelation prominence: a
/// prominence of 1.5 (the minimum for a tempo to be reported at all)
/// scores 0.2, and a prominence of 3.5 or more scores 1.
fn tempo_stability_score(prominence: f64) -> f64 {
    ((prominence - 1.0) / 2.5).clamp(0.0, 1.0)
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
/// involved. The onset envelope is loudness-weighted (see
/// [`energy_weighted_flux`]) and the confidence is the mean of the
/// beat-grid energy share and the tempo stability. When the pulse is
/// too weak or unstable (`confidence < 0.25`), `bpm` is `None` and
/// `beats` is empty rather than an invented tempo.
pub fn detect_beats(samples: &[i16], sample_rate: u32) -> BeatResult {
    let hop_seconds = BEAT_HOP as f64 / sample_rate as f64;
    let flux = energy_weighted_flux(samples, sample_rate);
    let Some((bpm, prominence)) = estimate_bpm_with_prominence(&flux, hop_seconds) else {
        return BeatResult {
            method: METHOD_HEURISTIC.into(),
            bpm: None,
            confidence: 0.0,
            tempo_stability: 0.0,
            beats: vec![],
            note: "No stable pulse was detected: the audio has too little onset energy or no steady tempo (speech, ambience, or rubato music). No tempo was estimated rather than reporting an unreliable one."
                .into(),
        };
    };
    let beats = pick_beats(&flux, hop_seconds, 60.0 / bpm);
    let grid_energy = beat_grid_energy(&flux, &beats, hop_seconds);
    let stability = tempo_stability_score(prominence);
    let confidence = (grid_energy + stability) / 2.0;
    if confidence < 0.25 {
        return BeatResult {
            method: METHOD_HEURISTIC.into(),
            bpm: None,
            confidence: (confidence * 1000.0).round() / 1000.0,
            tempo_stability: (stability * 1000.0).round() / 1000.0,
            beats: vec![],
            note: "The detected pulse is too weak to trust (onset energy does not fall on a steady beat grid, or the tempo estimate is unstable). No tempo was reported rather than an unreliable one."
                .into(),
        };
    }
    BeatResult {
        method: METHOD_HEURISTIC.into(),
        bpm: Some((bpm * 10.0).round() / 10.0),
        confidence: (confidence * 1000.0).round() / 1000.0,
        tempo_stability: (stability * 1000.0).round() / 1000.0,
        beats,
        note: "Energy-based onset detection (heuristic, not machine learning). The onset envelope is weighted by frame loudness, the tempo is estimated from the onset envelope, and confidence is the mean of the beat-grid energy share and the tempo stability. The tempo may be off by a half or double factor on syncopated or sparse music."
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

/// One keyframe of a face-tracked reframe plan: at `t_seconds` into
/// the clip the crop is centred on (`focus_x`, `focus_y`) in
/// normalised frame coordinates. Shared with the export filtergraph,
/// which turns these keyframes into a time-varying crop expression
/// (see [`crate::filtergraph::ExportSettings::face_tracks`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReframeKeyframe {
    pub t_seconds: f64,
    pub focus_x: f64,
    pub focus_y: f64,
}

/// Result of [`reframe_crop`] / [`reframe_clip`] /
/// [`reframe_motion_clip`] / [`reframe_face_track_clip`]. `method` is
/// `"centre-crop"` for a geometric crop, `"motion-weighted"` for a
/// crop centred on the highest-motion region (see
/// [`reframe_motion_clip`]), or `"face-tracked"` for a crop whose
/// centre follows a detected face over time (see
/// [`reframe_face_track_clip`]). `motion_confidence` is the share of
/// frame-to-frame motion in the chosen region (0–1) for
/// motion-weighted plans and `None` otherwise. For face-tracked
/// plans, `keyframes` holds the smoothed face track (one focus point
/// per sampled frame) and `face_confidence` is the mean detection
/// score across the track (0–1); both are empty / `None` for other
/// methods. `ffmpeg_filter` is a ready-to-use FFmpeg `crop` filter
/// for the source (e.g. `"crop=606:1080:656:0"`); for face-tracked
/// plans it is the static crop at the first keyframe — the
/// time-varying crop lives in the keyframes and is applied on export.
/// Both new fields default on deserialization, so plans written
/// before this feature still load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReframePlan {
    pub method: String,
    pub target: String,
    pub source_width: u32,
    pub source_height: u32,
    pub crop: CropRect,
    pub ffmpeg_filter: String,
    pub motion_confidence: Option<f64>,
    #[serde(default)]
    pub keyframes: Vec<ReframeKeyframe>,
    #[serde(default)]
    pub face_confidence: Option<f64>,
    pub note: String,
}

/// What auto-reframe can and cannot do. `face_tracking_available` is
/// `true` when the bundled BlazeFace model is present and loads (see
/// [`crate::face`]); `motion_reframe_available` is `true`: a
/// motion-weighted reframe (highest-motion region, centre-crop
/// fallback) is available as well, and is the fallback when no face
/// is found consistently.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReframeCapability {
    pub face_tracking_available: bool,
    pub motion_reframe_available: bool,
    pub method: String,
    pub note: String,
}

pub fn reframe_capability() -> ReframeCapability {
    if face::model_available() {
        ReframeCapability {
            face_tracking_available: true,
            motion_reframe_available: true,
            method: METHOD_FACE_TRACKED.into(),
            note: "Face tracking is available: the bundled BlazeFace full-range model (Apache-2.0, converted in-house to ONNX, see assets/face-detection/README.md) detects faces under tract on the CPU. See reframe_face_track. When no face is found consistently across the clip, the plan falls back to the motion-weighted reframe and says so."
                .into(),
        }
    } else {
        ReframeCapability {
            face_tracking_available: false,
            motion_reframe_available: true,
            method: METHOD_CENTRE_CROP.into(),
            note: format!(
                "Face tracking is unavailable: the bundled BlazeFace model failed to load ({}). A motion-weighted reframe is available instead: it centres the crop on the highest-motion region at the playhead and falls back to a centre crop when the frames are effectively static; motion is a proxy for the subject, not subject detection. See reframe_motion_clip.",
                face::model_load_error()
                    .unwrap_or_else(|| "unknown load error".into())
            ),
        }
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
/// frame allows. No face is detected by this function itself: a focus
/// point is only a hint the caller supplies (for example from the
/// face tracker in [`reframe_face_track_clip`]). When the source
/// already matches the target aspect ratio, the crop is the full
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
        motion_confidence: None,
        keyframes: vec![],
        face_confidence: None,
        note: "Centre crop — no face is detected by this crop itself. Face tracking is available separately (see reframe_face_track): the crop here is centred on the frame (or on the supplied focus point)."
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

// ---------------------------------------------------------------------------
// Motion-weighted reframe (no face detection)
// ---------------------------------------------------------------------------

/// Columns of the motion grid ([`motion_grid`]).
pub const MOTION_COLUMNS: usize = 8;
/// Rows of the motion grid ([`motion_grid`]).
pub const MOTION_ROWS: usize = 4;
/// Time between the two frames compared for motion, in seconds.
pub const MOTION_FRAME_GAP_SECS: f64 = 0.5;
/// Mean absolute luma difference (0–255) below which two frames count
/// as effectively static and the motion-weighted reframe falls back to
/// the centre crop.
pub const MOTION_MIN_MEAN_DIFF: f64 = 1.0;

/// Per-cell mean absolute luma difference (0–255) between two RGB24
/// frames of `width` x `height`, over a [`MOTION_ROWS`] x
/// [`MOTION_COLUMNS`] grid in row-major order. Pure computation: no
/// I/O, no FFmpeg. Returns an empty vector when the inputs are not
/// two complete frames of the stated size.
pub fn motion_grid(previous: &[u8], current: &[u8], width: u32, height: u32) -> Vec<f32> {
    let expected = width as usize * height as usize * 3;
    if width == 0 || height == 0 || previous.len() < expected || current.len() < expected {
        return vec![];
    }
    let mut sums = vec![0.0f64; MOTION_COLUMNS * MOTION_ROWS];
    let mut counts = vec![0usize; MOTION_COLUMNS * MOTION_ROWS];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let offset = (y * width as usize + x) * 3;
            let (before, _, _) =
                scopes::rgb_to_ycbcr(previous[offset], previous[offset + 1], previous[offset + 2]);
            let (after, _, _) =
                scopes::rgb_to_ycbcr(current[offset], current[offset + 1], current[offset + 2]);
            let column = (x * MOTION_COLUMNS) / width as usize;
            let row = (y * MOTION_ROWS) / height as usize;
            let cell = row * MOTION_COLUMNS + column;
            sums[cell] += (after as f64 - before as f64).abs();
            counts[cell] += 1;
        }
    }
    sums.into_iter()
        .zip(counts)
        .map(|(sum, count)| {
            if count == 0 {
                0.0
            } else {
                (sum / count as f64) as f32
            }
        })
        .collect()
}

/// Focus point and confidence from two RGB24 frames: the centre of the
/// motion-grid cell with the strongest mean luma change, as normalised
/// frame coordinates (`focus_x`, `focus_y`, each 0–1), and the share
/// of total motion that cell holds (0–1). Returns `None` when the
/// frames are effectively static (mean difference below
/// [`MOTION_MIN_MEAN_DIFF`]) or the inputs are not complete frames.
/// Motion is a proxy for the subject, not subject detection.
pub fn motion_focus(
    previous: &[u8],
    current: &[u8],
    width: u32,
    height: u32,
) -> Option<(f64, f64, f64)> {
    let grid = motion_grid(previous, current, width, height);
    if grid.is_empty() {
        return None;
    }
    let total: f64 = grid.iter().map(|value| *value as f64).sum();
    let mean = total / grid.len() as f64;
    if !(mean >= MOTION_MIN_MEAN_DIFF) || total <= 0.0 {
        return None;
    }
    let (best, peak) = grid
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;
    let column = best % MOTION_COLUMNS;
    let row = best / MOTION_COLUMNS;
    Some((
        (column as f64 + 0.5) / MOTION_COLUMNS as f64,
        (row as f64 + 0.5) / MOTION_ROWS as f64,
        round_ms((*peak as f64 / total).clamp(0.0, 1.0)),
    ))
}

/// Extract one RGB24 frame from `asset` at `source_time` seconds,
/// downscaled to `sample_width` x `sample_height` (the sampled
/// resolution used for scopes). The frame is small; only the motion
/// between two such frames is measured, never the full-resolution
/// source.
fn extract_sample_frame(
    asset: &media::Asset,
    source_time: f64,
    sample_width: u32,
    sample_height: u32,
) -> Result<Vec<u8>, String> {
    let source_path = media::verified_source(asset)?;
    let max_bytes = sample_width as u64 * sample_height as u64 * 3;
    let file = media::run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &source_time.to_string(),
            "-i",
            source_path.to_str().ok_or("Invalid media path")?,
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={sample_width}:{sample_height}:force_original_aspect_ratio=decrease"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ],
        Duration::from_secs(30),
        max_bytes,
    )?;
    let mut pixels = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut pixels)
        .map_err(|e| e.to_string())?;
    if pixels.len() as u64 != max_bytes {
        return Err("Frame extraction returned incomplete pixel data".into());
    }
    Ok(pixels)
}

/// Motion-weighted reframe plan for a clip in the open project: two
/// frames [`MOTION_FRAME_GAP_SECS`] apart are compared at the
/// playhead (`at_seconds`, seconds into the clip) and the crop is
/// centred on the highest-motion region (method
/// `"motion-weighted"`). When the frames are effectively static, or
/// the clip is too short for a frame pair, the plan falls back to the
/// centre crop (method `"centre-crop"`) and the note says so. No face
/// is detected or tracked by this plan: motion is a proxy for the
/// subject, and a moving background can pull the crop away from a
/// still subject. Face tracking is available separately (see
/// [`reframe_face_track_clip`]).
pub fn reframe_motion_clip(
    project: &Project,
    clip_id: Uuid,
    target: &str,
    at_seconds: f64,
) -> Result<ReframePlan, String> {
    let (asset, clip) = analysis::selected_video(project, clip_id)?;
    let width = asset.width.ok_or("Selected clip has no video dimensions")?;
    let height = asset
        .height
        .ok_or("Selected clip has no video dimensions")?;
    let duration = clip.duration as f64 / project.settings.fps as f64;
    if !at_seconds.is_finite() || at_seconds < 0.0 || at_seconds > duration {
        return Err("Reframe time must be inside the selected clip".into());
    }
    let source_in = clip.source_in as f64 / project.settings.fps as f64;
    let first = at_seconds;
    let second = if at_seconds + MOTION_FRAME_GAP_SECS <= duration {
        at_seconds + MOTION_FRAME_GAP_SECS
    } else if at_seconds >= MOTION_FRAME_GAP_SECS {
        at_seconds - MOTION_FRAME_GAP_SECS
    } else {
        let mut plan = reframe_crop(width, height, target, None, None)?;
        plan.note = "Motion-weighted reframe fell back to a centre crop: the clip is too short for two frames 0.5 s apart, so no motion region could be identified. No face is detected or tracked by this plan; face tracking is available separately (see reframe_face_track)."
            .into();
        return Ok(plan);
    };
    let (sample_width, sample_height) = scopes::sample_dimensions(width, height, false);
    let before = extract_sample_frame(asset, source_in + first, sample_width, sample_height)?;
    let after = extract_sample_frame(asset, source_in + second, sample_width, sample_height)?;
    match motion_focus(&before, &after, sample_width, sample_height) {
        Some((focus_x, focus_y, confidence)) => {
            let mut plan = reframe_crop(width, height, target, Some(focus_x), Some(focus_y))?;
            plan.method = METHOD_MOTION_WEIGHTED.into();
            plan.motion_confidence = Some(confidence);
            plan.note = format!(
                "Motion-weighted reframe — no face is detected or tracked by this plan; face tracking is available separately (see reframe_face_track). The crop is centred on the highest-motion region between two frames {MOTION_FRAME_GAP_SECS} s apart (focus {focus_x:.2}, {focus_y:.2} of the frame, motion confidence {confidence:.2}). Motion is a proxy for the subject: a moving background can pull the crop away from a still subject."
            );
            Ok(plan)
        }
        None => {
            let mut plan = reframe_crop(width, height, target, None, None)?;
            plan.note = "Motion-weighted reframe fell back to a centre crop: the two frames compared were effectively static, so no motion region could be identified. No face is detected or tracked by this plan; face tracking is available separately (see reframe_face_track)."
                .into();
            Ok(plan)
        }
    }
}

// ---------------------------------------------------------------------------
// Face-tracked reframe (BlazeFace full-range under tract)
// ---------------------------------------------------------------------------

/// Maximum number of frames sampled across the clip for face
/// tracking. Eight samples bound the FFmpeg extraction and inference
/// work while still covering the clip.
pub const FACE_TRACK_MAX_SAMPLES: usize = 8;
/// Nominal spacing between sampled frames, in seconds. Short clips
/// yield fewer samples; long clips are spread evenly across the whole
/// duration instead of clustering at the playhead.
pub const FACE_TRACK_SAMPLE_GAP_SECS: f64 = 0.5;

/// Face-tracked reframe plan for a clip in the open project: frames
/// are sampled across the whole clip, the bundled BlazeFace model
/// (see [`crate::face`]) runs on each sample, and the one face that
/// appears consistently is tracked (method `"face-tracked"`). The
/// plan's `keyframes` hold the smoothed track — one focus point per
/// sampled frame, in normalised frame coordinates — and its `crop` /
/// `ffmpeg_filter` is the static crop at the first keyframe; the
/// time-varying crop is applied on export from the keyframes (see
/// [`crate::filtergraph::ExportSettings::face_tracks`]).
///
/// When no face is found consistently, the plan falls back to the
/// motion-weighted reframe at the clip start ([`reframe_motion_clip`])
/// and the note says no face was tracked — it never claims a face was
/// tracked when it was not. When the bundled model fails to load, an
/// error is returned instead of a silent fallback, so the caller can
/// report the real reason (see [`reframe_capability`]).
pub fn reframe_face_track_clip(
    project: &Project,
    clip_id: Uuid,
    target: &str,
) -> Result<ReframePlan, String> {
    let (asset, clip) = analysis::selected_video(project, clip_id)?;
    let width = asset.width.ok_or("Selected clip has no video dimensions")?;
    let height = asset
        .height
        .ok_or("Selected clip has no video dimensions")?;
    if !face::model_available() {
        return Err(format!(
            "Face tracking is unavailable: the bundled BlazeFace model failed to load ({})",
            face::model_load_error().unwrap_or_else(|| "unknown load error".into())
        ));
    }
    let duration = clip.duration as f64 / project.settings.fps as f64;
    if !(duration > 0.0) {
        return Err("Selected clip has no duration".into());
    }
    let sample_count = ((duration / FACE_TRACK_SAMPLE_GAP_SECS).floor() as usize + 1)
        .clamp(1, FACE_TRACK_MAX_SAMPLES);
    let source_in = clip.source_in as f64 / project.settings.fps as f64;
    let (sample_width, sample_height) = scopes::sample_dimensions(width, height, false);
    let mut samples: Vec<face::FaceSample> = Vec::with_capacity(sample_count);
    for index in 0..sample_count {
        let t_seconds = if sample_count == 1 {
            0.0
        } else {
            duration * index as f64 / (sample_count - 1) as f64
        };
        let frame =
            extract_sample_frame(asset, source_in + t_seconds, sample_width, sample_height)?;
        let faces = face::detect_faces(&frame, sample_width, sample_height)?;
        samples.push(face::FaceSample { t_seconds, faces });
    }
    let found_in = samples
        .iter()
        .filter(|sample| !sample.faces.is_empty())
        .count();
    match face::select_face_track(&samples) {
        Some(track) => {
            let first = track[0];
            let mut plan = reframe_crop(
                width,
                height,
                target,
                Some(first.focus_x),
                Some(first.focus_y),
            )?;
            plan.method = METHOD_FACE_TRACKED.into();
            plan.keyframes = track
                .iter()
                .map(|point| ReframeKeyframe {
                    t_seconds: round_ms(point.t_seconds),
                    focus_x: round_ms(point.focus_x),
                    focus_y: round_ms(point.focus_y),
                })
                .collect();
            plan.face_confidence = Some(round_ms(
                track.iter().map(|point| point.confidence).sum::<f64>() / track.len() as f64,
            ));
            plan.note = format!(
                "Face-tracked reframe: one face was detected in {found_in} of {sample_count} sampled frames and tracked across the clip (mean face confidence {:.2}). The crop centre follows the smoothed track in the keyframes; the FFmpeg filter below is the static crop at the first keyframe, and export applies the time-varying crop from the track.",
                plan.face_confidence.unwrap_or(0.0)
            );
            Ok(plan)
        }
        None => {
            let mut plan = reframe_motion_clip(project, clip_id, target, 0.0)?;
            plan.note = format!(
                "No face was detected consistently in {sample_count} sampled frames ({found_in} had any face), so this plan falls back to the motion-weighted reframe. {}",
                plan.note
            );
            Ok(plan)
        }
    }
}
