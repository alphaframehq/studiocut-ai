//! Integration tests for colour scopes. The FFmpeg-dependent test
//! needs `ffmpeg`/`ffprobe` on PATH (as in `tests/media.rs`); it is
//! CI-verified only and is skipped by nothing — it fails loudly without
//! FFmpeg, by design, so a missing binary is never silent.

use std::{fs, path::Path, process::Command};
use studiocut_ai::{
    media::{ffmpeg_path, import_media},
    scopes::{analyze_scopes, shape_scopes, VECTORSCOPE_CELLS, WAVEFORM_COLUMNS},
    timeline::{Clip, Track},
    Project, Settings,
};
use uuid::Uuid;

fn fixture(path: &Path) {
    assert!(Command::new(ffmpeg_path())
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:size=1280x720:rate=30:duration=1",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(path)
        .status()
        .unwrap()
        .success());
}

fn project_with_clip(asset: &studiocut_ai::media::Asset, clip_id: Uuid) -> Project {
    Project {
        schema_version: 5,
        id: Uuid::new_v4(),
        name: "Scopes".into(),
        revision: 0,
        created_at: 0,
        settings: Settings {
            width: 1920,
            height: 1080,
            fps: 30,
            audio_sample_rate: 48000,
        },
        assets: vec![asset.clone()],
        tracks: vec![Track {
            id: Uuid::new_v4(),
            name: "V1".into(),
            kind: "video".into(),
            muted: false,
            solo: false,
            locked: false,
            clips: vec![Clip {
                id: clip_id,
                asset_id: asset.id,
                link_id: None,
                start: 0,
                source_in: 0,
                // 24 frames of a ~1 s source: safely inside the asset
                // even if ffprobe reports the duration slightly short.
                duration: 24,
                caption: None,
                effects: None,
            }],
        }],
        markers: vec![],
    }
}

#[test]
fn scopes_shape_is_pure_and_counts_every_pixel() {
    // Solid red 4x4 frame: luma histogram and vectorscope each count 16.
    let mut pixels = Vec::new();
    for _ in 0..16 {
        pixels.extend_from_slice(&[255u8, 0, 0]);
    }
    let scopes = shape_scopes(&pixels, 4, 4, false);
    assert!(scopes.approximate);
    assert!(!scopes.full_resolution);
    assert_eq!(scopes.waveform.len(), WAVEFORM_COLUMNS);
    assert_eq!(scopes.histogram_luma.iter().sum::<u32>(), 16);
    assert_eq!(
        scopes.vectorscope.len(),
        VECTORSCOPE_CELLS * VECTORSCOPE_CELLS
    );
    assert_eq!(scopes.vectorscope.iter().sum::<u32>(), 16);
    assert_eq!(scopes.vectorscope_peak, 16);
}

#[test]
fn analyze_scopes_from_a_real_frame_is_approximate() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cache");
    let path = directory.path().join("scope-clip.mp4");
    fixture(&path);
    let asset = import_media(&root, &path).unwrap();
    assert_eq!((asset.width, asset.height), (Some(1280), Some(720)));
    let clip_id = Uuid::new_v4();
    let project = project_with_clip(&asset, clip_id);
    let scopes = analyze_scopes(&project, clip_id, 0.5, false).unwrap();
    // Approximation is labelled, never hidden.
    assert!(scopes.approximate);
    assert!(!scopes.full_resolution);
    assert_eq!((scopes.sample_width, scopes.sample_height), (320, 180));
    assert_eq!(scopes.waveform.len(), WAVEFORM_COLUMNS);
    assert_eq!(scopes.histogram_luma.iter().sum::<u32>(), 320 * 180);
    assert_eq!(scopes.vectorscope.iter().sum::<u32>(), 320 * 180);
    assert!(scopes.vectorscope_peak > 0);
    assert!(scopes.waveform.iter().all(|[min, max]| min <= max));
    // Out-of-range times and unknown clips are rejected.
    assert!(analyze_scopes(&project, clip_id, 2.0, false).is_err());
    assert!(analyze_scopes(&project, Uuid::new_v4(), 0.5, false).is_err());
    let _ = fs::remove_file(&path);
}

#[test]
fn analyze_scopes_full_resolution_uses_the_native_frame() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cache");
    let path = directory.path().join("scope-clip-full.mp4");
    fixture(&path);
    let asset = import_media(&root, &path).unwrap();
    assert_eq!((asset.width, asset.height), (Some(1280), Some(720)));
    let clip_id = Uuid::new_v4();
    let project = project_with_clip(&asset, clip_id);
    let scopes = analyze_scopes(&project, clip_id, 0.5, true).unwrap();
    // Full-resolution sampling is labelled as not approximate.
    assert!(!scopes.approximate);
    assert!(scopes.full_resolution);
    assert_eq!((scopes.sample_width, scopes.sample_height), (1280, 720));
    assert_eq!(scopes.waveform.len(), WAVEFORM_COLUMNS);
    assert_eq!(scopes.histogram_luma.iter().sum::<u32>(), 1280 * 720);
    assert_eq!(scopes.vectorscope.iter().sum::<u32>(), 1280 * 720);
    assert!(scopes.vectorscope_peak > 0);
    assert!(scopes.waveform.iter().all(|[min, max]| min <= max));
    let _ = fs::remove_file(&path);
}
