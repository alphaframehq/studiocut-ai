//! Integration tests for `studiocut_ai::ai_gaps`: WAV parsing, heuristic
//! beat detection on a synthetic click track, pause-based speaker turns,
//! and centre-crop reframe geometry. No external tools are needed: the
//! audio fixtures are built in memory.

use studiocut_ai::{ai_gaps, transcription::TimedText};

fn make_wav(samples: &[i16], channels: u16, sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = vec![];
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// 8 seconds of 16 kHz audio with a short burst every `60 / bpm`
/// seconds, starting at 0.
fn click_track(seconds: f64, sample_rate: u32, bpm: f64) -> Vec<i16> {
    let total = (seconds * sample_rate as f64) as usize;
    let mut samples = vec![0i16; total];
    let period = 60.0 / bpm;
    let mut at = 0.0;
    while at < seconds {
        let start = (at * sample_rate as f64) as usize;
        for i in 0..100 {
            if start + i < total {
                samples[start + i] = 20_000;
            }
        }
        at += period;
    }
    samples
}

fn word(text: &str, start: f64, end: f64) -> TimedText {
    TimedText {
        text: text.into(),
        start,
        end,
    }
}

#[test]
fn wav_round_trip_mono_and_stereo_downmix() {
    let (samples, rate) =
        ai_gaps::parse_wav_pcm16(&make_wav(&[0, 1000, -1000, 8000], 1, 16000)).unwrap();
    assert_eq!(rate, 16000);
    assert_eq!(samples, vec![0, 1000, -1000, 8000]);
    // Stereo downmixes by averaging channels: (1000 + -1000) / 2 = 0.
    let (mixed, _) =
        ai_gaps::parse_wav_pcm16(&make_wav(&[1000, -1000, 2000, -2000], 2, 16000)).unwrap();
    assert_eq!(mixed, vec![0, 0]);
}

#[test]
fn wav_rejects_non_wav_and_non_pcm() {
    assert!(ai_gaps::parse_wav_pcm16(b"not a wav file at all").is_err());
    assert!(ai_gaps::parse_wav_pcm16(&[]).is_err());
    let mut non_pcm = make_wav(&[1, 2, 3], 1, 16000);
    // fmt chunk, audio format field: bytes 20..22 hold the format tag.
    non_pcm[20] = 3;
    non_pcm[21] = 0;
    assert!(ai_gaps::parse_wav_pcm16(&non_pcm).is_err());
    let mut eight_bit = make_wav(&[1, 2, 3], 1, 16000);
    // fmt chunk, bits-per-sample field: bytes 34..36.
    eight_bit[34] = 8;
    eight_bit[35] = 0;
    assert!(ai_gaps::parse_wav_pcm16(&eight_bit).is_err());
}

#[test]
fn bpm_click_track_120_and_beats_on_grid() {
    let samples = click_track(8.0, 16000, 120.0);
    let result = ai_gaps::detect_beats(&samples, 16000);
    assert_eq!(result.method, "heuristic");
    let bpm = result.bpm.expect("120 BPM click track must yield a tempo");
    assert!((bpm - 120.0).abs() <= 2.0, "expected ~120 BPM, got {bpm}");
    assert!(
        result.confidence > 0.5,
        "clean click track must score high confidence, got {}",
        result.confidence
    );
    assert!(
        result.tempo_stability > 0.5,
        "clean click track must score high tempo stability, got {}",
        result.tempo_stability
    );
    // Clicks at 0, 0.5, …, 7.5 s: 16 beats, each near its grid position.
    assert_eq!(result.beats.len(), 16, "beats: {:?}", result.beats);
    for (index, beat) in result.beats.iter().enumerate() {
        let expected = index as f64 * 0.5;
        assert!(
            (beat - expected).abs() <= 0.08,
            "beat {index}: got {beat}, expected {expected}"
        );
    }
}

#[test]
fn bpm_silence_and_steady_signal_return_no_tempo() {
    let silent = ai_gaps::detect_beats(&vec![0i16; 16000 * 4], 16000);
    assert_eq!(silent.method, "heuristic");
    assert_eq!(silent.bpm, None);
    assert!(silent.beats.is_empty());
    assert_eq!(silent.confidence, 0.0);
    assert_eq!(silent.tempo_stability, 0.0);
    // Constant signal: steady energy, no onsets, no pulse.
    let steady = ai_gaps::detect_beats(&vec![10_000i16; 16000 * 4], 16000);
    assert_eq!(steady.bpm, None);
    assert!(steady.beats.is_empty());
    assert_eq!(steady.tempo_stability, 0.0);
}

/// Loudness weighting counts onsets in quiet passages for less: a
/// quiet burst produces a much smaller weighted flux than the same
/// burst at full loudness, and a smaller weighted flux than its own
/// unweighted flux.
#[test]
fn energy_weighted_flux_downweights_quiet_onsets() {
    let mut samples = vec![0i16; 16000 * 2];
    for i in 0..100 {
        samples[1600 + i] = 20_000;
        samples[17600 + i] = 2_000;
    }
    let flux = ai_gaps::onset_envelope(&samples, 16000);
    let weighted = ai_gaps::energy_weighted_flux(&samples, 16000);
    assert_eq!(flux.len(), weighted.len());
    let loud_max = weighted[0..10].iter().copied().fold(0.0f32, f32::max);
    let quiet_max = weighted[30..40].iter().copied().fold(0.0f32, f32::max);
    let quiet_unweighted = flux[30..40].iter().copied().fold(0.0f32, f32::max);
    assert!(loud_max > 0.0);
    assert!(
        quiet_max < loud_max * 0.2,
        "weighted quiet {quiet_max} vs loud {loud_max}"
    );
    assert!(
        quiet_max < quiet_unweighted * 0.5,
        "weighting must reduce the quiet onset: {quiet_max} vs {quiet_unweighted}"
    );
}

#[test]
fn diarize_words_splits_on_pauses_and_alternates_speakers() {
    let result = ai_gaps::diarize_words(
        &[
            word("hello", 0.0, 0.4),
            word("world", 0.5, 0.9),
            word("again", 2.1, 2.5),
            word("now", 2.6, 3.0),
        ],
        0.8,
    )
    .unwrap();
    assert_eq!(result.method, "heuristic");
    assert_eq!(result.turns.len(), 2);
    // Short turns, moderate pause, no loudness data: the first turn
    // (confidence 0.375) is below the 0.5 label threshold and carries
    // no speaker label; the second turn (confidence 0.575) is labelled,
    // taking the first label because the unlabelled turn did not
    // consume one.
    assert_eq!(result.turns[0].speaker, None);
    assert_eq!(result.turns[0].confidence, 0.375);
    assert_eq!(result.turns[0].start, 0.0);
    assert_eq!(result.turns[0].end, 0.9);
    assert_eq!(result.turns[0].word_count, 2);
    assert_eq!(result.turns[1].speaker.as_deref(), Some("Speaker 1"));
    assert_eq!(result.turns[1].confidence, 0.575);
    assert_eq!(result.turns[1].start, 2.1);
    assert_eq!(result.turns[1].end, 3.0);
    assert_eq!(result.turns[1].word_count, 2);
    assert_eq!(result.confidence, 0.475);
    assert!(result.note.contains("heuristic"));
    assert!(result.note.contains("0.5"));
}

#[test]
fn diarize_words_confident_turns_carry_alternating_labels() {
    // Eight words per turn and a pause twice the threshold: both turns
    // score above the 0.5 label threshold and alternate labels.
    let mut words = vec![];
    for i in 0..8 {
        words.push(word("word", i as f64 * 0.5, i as f64 * 0.5 + 0.4));
    }
    for i in 0..8 {
        words.push(word(
            "word",
            5.6 + i as f64 * 0.5,
            5.6 + i as f64 * 0.5 + 0.4,
        ));
    }
    let result = ai_gaps::diarize_words(&words, 0.8).unwrap();
    assert_eq!(result.turns.len(), 2);
    assert_eq!(result.turns[0].speaker.as_deref(), Some("Speaker 1"));
    assert_eq!(result.turns[0].confidence, 0.75);
    assert_eq!(result.turns[1].speaker.as_deref(), Some("Speaker 2"));
    assert_eq!(result.turns[1].confidence, 1.0);
    assert_eq!(result.confidence, 0.875);
}

#[test]
fn diarize_words_low_confidence_turns_have_no_speaker() {
    // Two-word turns separated by a pause only just above the
    // threshold: both confidences stay below 0.5, so neither turn
    // invents a speaker label.
    let result = ai_gaps::diarize_words(
        &[
            word("a", 0.0, 0.3),
            word("b", 0.4, 0.7),
            word("c", 1.6, 1.9),
            word("d", 2.0, 2.3),
        ],
        0.8,
    )
    .unwrap();
    assert_eq!(result.turns.len(), 2);
    assert!(result.turns.iter().all(|turn| turn.speaker.is_none()));
    assert!(result.turns.iter().all(|turn| turn.confidence < 0.5));
    assert!(result.confidence < 0.5);
}

#[test]
fn diarize_words_rejects_bad_pause_and_empty_input() {
    let words = [word("hello", 0.0, 0.4)];
    assert!(ai_gaps::diarize_words(&words, 0.1).is_err());
    assert!(ai_gaps::diarize_words(&words, 6.0).is_err());
    assert!(ai_gaps::diarize_words(&[], 0.8).is_err());
    // A gap shorter than the pause threshold stays in one turn. The
    // single short turn is below the label threshold, so it has no
    // speaker label.
    let one = ai_gaps::diarize_words(&[word("a", 0.0, 0.3), word("b", 0.5, 0.8)], 0.8).unwrap();
    assert_eq!(one.turns.len(), 1);
    assert_eq!(one.turns[0].speaker, None);
    assert_eq!(one.turns[0].confidence, 0.375);
}

#[test]
fn word_energies_normalise_by_the_loudest_word() {
    let mut samples = vec![10_000i16; 8000];
    samples.extend(vec![20_000i16; 8000]);
    let energies = ai_gaps::word_energies(
        &samples,
        16000,
        &[word("quiet", 0.0, 0.5), word("loud", 0.5, 1.0)],
    );
    assert_eq!(energies.len(), 2);
    assert!((energies[0] - 0.5).abs() < 1e-6, "got {}", energies[0]);
    assert!((energies[1] - 1.0).abs() < 1e-6, "got {}", energies[1]);
    // Silence scores zero throughout.
    assert_eq!(
        ai_gaps::word_energies(&vec![0i16; 16000], 16000, &[word("x", 0.0, 0.5)]),
        vec![0.0]
    );
}

#[test]
fn diarize_words_energy_contrast_raises_confidence() {
    let words = [
        word("hello", 0.0, 0.4),
        word("world", 0.5, 0.9),
        word("again", 2.1, 2.5),
        word("now", 2.6, 3.0),
    ];
    // Without loudness the first turn is unlabelled (see
    // diarize_words_splits_on_pauses_and_alternates_speakers).
    let pause_only = ai_gaps::diarize_words(&words, 0.8).unwrap();
    assert_eq!(pause_only.turns[0].speaker, None);
    // With a strong loudness contrast (quiet first turn, loud second
    // turn) both turns clear the threshold and alternate labels.
    let with_energy =
        ai_gaps::diarize_words_with_energy(&words, 0.8, &[0.1, 0.1, 0.9, 0.9]).unwrap();
    assert_eq!(with_energy.turns[0].speaker.as_deref(), Some("Speaker 1"));
    assert_eq!(with_energy.turns[0].confidence, 0.625);
    assert_eq!(with_energy.turns[1].speaker.as_deref(), Some("Speaker 2"));
    // 0.45 * 0.75 + 0.35 * 1.0 + 0.20 * 0.25 = 0.7375, asserted with
    // tolerance: the value sits on a binary rounding boundary.
    assert!((with_energy.turns[1].confidence - 0.7375).abs() < 0.002);
    assert!(with_energy.confidence > pause_only.confidence);
    // Loudness must be empty or one value per word.
    assert!(ai_gaps::diarize_words_with_energy(&words, 0.8, &[0.5]).is_err());
    assert!(ai_gaps::diarize_words_with_energy(&words, 0.8, &[0.5; 5]).is_err());
}

#[test]
fn reframe_9_16_centre_crop_is_even_and_centred() {
    let plan = ai_gaps::reframe_crop(1920, 1080, "9:16", None, None).unwrap();
    assert_eq!(plan.method, "centre-crop");
    assert_eq!(plan.motion_confidence, None);
    assert_eq!(plan.target, "9:16");
    assert_eq!(plan.source_width, 1920);
    assert_eq!(plan.source_height, 1080);
    // 1080 * 9/16 = 607.5 -> 607 -> even 606; x = (1920 - 606) / 2 = 657
    // -> even 656.
    assert_eq!(
        plan.crop,
        ai_gaps::CropRect {
            x: 656,
            y: 0,
            width: 606,
            height: 1080
        }
    );
    assert_eq!(plan.ffmpeg_filter, "crop=606:1080:656:0");
    assert!(plan.note.contains("no face is detected by this crop"));
    assert!(plan.keyframes.is_empty());
    assert_eq!(plan.face_confidence, None);
}

#[test]
fn reframe_1_1_and_focus_point() {
    let square = ai_gaps::reframe_crop(1920, 1080, "1:1", None, None).unwrap();
    assert_eq!(
        square.crop,
        ai_gaps::CropRect {
            x: 420,
            y: 0,
            width: 1080,
            height: 1080
        }
    );
    assert_eq!(square.ffmpeg_filter, "crop=1080:1080:420:0");
    // Focus near the right edge moves the crop window right, clamped to
    // the frame: centre 0.9 * 1920 = 1728, x = 1728 - 303 = 1425 ->
    // clamped to 1920 - 606 = 1314.
    let focused = ai_gaps::reframe_crop(1920, 1080, "9:16", Some(0.9), None).unwrap();
    assert_eq!(focused.crop.x, 1314);
    assert_eq!(focused.crop.width, 606);
    // A source that already matches the target needs no crop.
    let already = ai_gaps::reframe_crop(1080, 1920, "9:16", None, None).unwrap();
    assert_eq!(already.crop.width, 1080);
    assert_eq!(already.crop.height, 1920);
}

#[test]
fn reframe_rejects_unknown_target_and_bad_focus() {
    assert!(ai_gaps::reframe_crop(1920, 1080, "4:5", None, None).is_err());
    assert!(ai_gaps::reframe_crop(1920, 1080, "9:16", Some(1.5), None).is_err());
    assert!(ai_gaps::reframe_crop(1920, 1080, "9:16", None, Some(-0.1)).is_err());
    assert!(ai_gaps::reframe_crop(8, 1080, "9:16", None, None).is_err());
}

#[test]
fn capabilities_report_unavailable_methods_honestly() {
    let diarization = ai_gaps::diarization_capability();
    assert!(!diarization.whisper_diarization_available);
    assert_eq!(diarization.method, "heuristic");
    assert!(diarization.note.contains("no speaker segmentation"));
    let reframe = ai_gaps::reframe_capability();
    assert!(reframe.face_tracking_available);
    assert!(reframe.motion_reframe_available);
    assert_eq!(reframe.method, "face-tracked");
    assert!(reframe.note.contains("BlazeFace"));
    assert!(reframe.note.contains("motion-weighted"));
}

#[test]
fn reframe_plan_without_track_fields_still_parses() {
    // Plans written before face tracking carry no keyframes or face
    // confidence; serde defaults must keep them loadable.
    let plan: ai_gaps::ReframePlan = serde_json::from_str(
        r#"{"method":"centre-crop","target":"9:16","sourceWidth":1920,"sourceHeight":1080,"crop":{"x":656,"y":0,"width":606,"height":1080},"ffmpegFilter":"crop=606:1080:656:0","motionConfidence":null,"note":"Centre crop."}"#,
    )
    .unwrap();
    assert_eq!(plan.method, "centre-crop");
    assert!(plan.keyframes.is_empty());
    assert_eq!(plan.face_confidence, None);
    // A face-tracked plan round-trips with its keyframes intact.
    let tracked: ai_gaps::ReframePlan = serde_json::from_str(
        r#"{"method":"face-tracked","target":"9:16","sourceWidth":1920,"sourceHeight":1080,"crop":{"x":656,"y":0,"width":606,"height":1080},"ffmpegFilter":"crop=606:1080:656:0","motionConfidence":null,"keyframes":[{"tSeconds":0.0,"focusX":0.2,"focusY":0.5},{"tSeconds":1.0,"focusX":0.8,"focusY":0.5}],"faceConfidence":0.85,"note":"Face-tracked reframe."}"#,
    )
    .unwrap();
    assert_eq!(tracked.method, "face-tracked");
    assert_eq!(tracked.keyframes.len(), 2);
    assert_eq!(tracked.keyframes[1].focus_x, 0.8);
    assert_eq!(tracked.face_confidence, Some(0.85));
}

/// A 16x8 frame pair where only one motion-grid cell changes: the
/// grid is 8 columns by 4 rows, so each cell is 2x2 px.
fn motion_pair() -> (Vec<u8>, Vec<u8>) {
    let before = vec![0u8; 16 * 8 * 3];
    let mut after = before.clone();
    // Brighten the cell at column 2, row 1 (x 4..5, y 2..3).
    for y in 2..4 {
        for x in 4..6 {
            let offset = (y * 16 + x) * 3;
            after[offset] = 255;
            after[offset + 1] = 255;
            after[offset + 2] = 255;
        }
    }
    (before, after)
}

#[test]
fn motion_grid_finds_the_moving_cell() {
    let (before, after) = motion_pair();
    let grid = ai_gaps::motion_grid(&before, &after, 16, 8);
    assert_eq!(grid.len(), 8 * 4);
    // Only the cell at column 2, row 1 changed (mean luma change 255).
    for (index, value) in grid.iter().enumerate() {
        if index == 1 * 8 + 2 {
            assert_eq!(*value, 255.0);
        } else {
            assert_eq!(*value, 0.0, "cell {index} must be still");
        }
    }
    // Incomplete frames produce no grid.
    assert!(ai_gaps::motion_grid(&before[..10], &after, 16, 8).is_empty());
    assert!(ai_gaps::motion_grid(&before, &after, 0, 8).is_empty());
}

#[test]
fn motion_focus_returns_the_moving_cell_centre() {
    let (before, after) = motion_pair();
    let focus = ai_gaps::motion_focus(&before, &after, 16, 8)
        .expect("one moving cell must yield a focus point");
    // Cell centre: x (2 + 0.5) / 8, y (1 + 0.5) / 4; all motion is in
    // that cell, so confidence is 1.
    assert_eq!(focus, (0.3125, 0.375, 1.0));
}

#[test]
fn motion_focus_returns_none_for_static_frames() {
    let (before, after) = motion_pair();
    // Identical frames: no motion at all.
    assert_eq!(ai_gaps::motion_focus(&before, &before, 16, 8), None);
    // A single pixel changing by one luma step is far below the static
    // threshold (mean difference 1/128).
    let mut faint_after = before.clone();
    faint_after[0] = 1;
    faint_after[1] = 1;
    faint_after[2] = 1;
    assert_eq!(ai_gaps::motion_focus(&before, &faint_after, 16, 8), None);
    // Incomplete frames produce no focus.
    assert_eq!(ai_gaps::motion_focus(&before[..10], &after, 16, 8), None);
}
