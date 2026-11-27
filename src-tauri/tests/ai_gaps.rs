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
    // Constant signal: steady energy, no onsets, no pulse.
    let steady = ai_gaps::detect_beats(&vec![10_000i16; 16000 * 4], 16000);
    assert_eq!(steady.bpm, None);
    assert!(steady.beats.is_empty());
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
    assert_eq!(result.turns[0].speaker, "Speaker 1");
    assert_eq!(result.turns[0].start, 0.0);
    assert_eq!(result.turns[0].end, 0.9);
    assert_eq!(result.turns[0].word_count, 2);
    assert_eq!(result.turns[1].speaker, "Speaker 2");
    assert_eq!(result.turns[1].start, 2.1);
    assert_eq!(result.turns[1].end, 3.0);
    assert_eq!(result.turns[1].word_count, 2);
    assert!(result.note.contains("heuristic"));
}

#[test]
fn diarize_words_rejects_bad_pause_and_empty_input() {
    let words = [word("hello", 0.0, 0.4)];
    assert!(ai_gaps::diarize_words(&words, 0.1).is_err());
    assert!(ai_gaps::diarize_words(&words, 6.0).is_err());
    assert!(ai_gaps::diarize_words(&[], 0.8).is_err());
    // A gap shorter than the pause threshold stays in one turn.
    let one = ai_gaps::diarize_words(&[word("a", 0.0, 0.3), word("b", 0.5, 0.8)], 0.8).unwrap();
    assert_eq!(one.turns.len(), 1);
    assert_eq!(one.turns[0].speaker, "Speaker 1");
}

#[test]
fn reframe_9_16_centre_crop_is_even_and_centred() {
    let plan = ai_gaps::reframe_crop(1920, 1080, "9:16", None, None).unwrap();
    assert_eq!(plan.method, "centre-crop");
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
    assert!(plan.note.contains("face tracking unavailable"));
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
    assert!(!reframe.face_tracking_available);
    assert_eq!(reframe.method, "centre-crop");
    assert!(reframe.note.contains("No face-detection model"));
}
