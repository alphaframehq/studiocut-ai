use std::{fs, process::Command};
use studiocut_ai::{
    create_project, load_project, media, save_project,
    timeline::{History, Track},
};

#[test]
fn typescript_edits_roundtrip_in_rust_and_real_audio_decode() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("linked clip.mp4");
    assert!(Command::new(media::ffmpeg_path())
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=6",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=30:duration=6",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest"
        ])
        .arg(&source)
        .status()
        .unwrap()
        .success());
    let original = fs::read(&source).unwrap();
    let asset = media::import_media(&dir.path().join("cache"), &source).unwrap();
    let mut project = create_project("Timeline parity".into(), 1920, 1080, 30).unwrap();
    project.schema_version = 2;
    project.assets.push(asset.clone());
    let input = dir.path().join("input.studiocut");
    let output = dir.path().join("edited.studiocut");
    save_project(&project, &input).unwrap();
    let result = Command::new("node")
        .arg("tests/timeline.mjs")
        .arg(&input)
        .arg(&output)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let edited = load_project(&output).unwrap();
    assert_eq!(edited.schema_version, 3);
    assert_eq!(edited.tracks.len(), 2);
    assert_eq!(edited.tracks[0].clips.len(), 2);
    assert_eq!(edited.tracks[0].clips[0].duration, 45);
    let reopened = dir.path().join("roundtrip.studiocut");
    save_project(&edited, &reopened).unwrap();
    assert_eq!(load_project(&reopened).unwrap(), edited);
    for range in [(0.0, 2.0), (2.0, 2.0), (4.0, 2.0)] {
        let bytes = media::audio_chunk(&asset, range.0, range.1).unwrap();
        assert_eq!(bytes.len(), 48000 * 2 * 2 * 4);
        assert!(bytes
            .chunks_exact(4)
            .any(|b| f32::from_le_bytes(b.try_into().unwrap()).abs() > 0.01));
    }
    for (start, duration) in [(f64::NAN, 1.0), (-1.0, 1.0), (0.0, 5.0), (5.0, 2.0)] {
        assert!(media::audio_chunk(&asset, start, duration).is_err());
    }
    assert_eq!(fs::read(&source).unwrap(), original);
    fs::write(&source, b"changed").unwrap();
    assert!(media::audio_chunk(&asset, 0.0, 1.0)
        .unwrap_err()
        .contains("changed"));
}

#[test]
fn rejects_invalid_graph_and_preserves_atomic_target() {
    let mut project = create_project("Validation".into(), 1920, 1080, 30).unwrap();
    let mut history = History::default();
    let track = Track {
        id: uuid::Uuid::new_v4(),
        name: "Video 1".into(),
        kind: "video".into(),
        muted: false,
        solo: false,
        locked: false,
        clips: vec![],
    };
    project = history.apply(&project, vec![track.clone()], 0).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.studiocut");
    save_project(&project, &path).unwrap();
    let mut invalid = project.clone();
    invalid.tracks.push(track.clone());
    assert!(save_project(&invalid, &path).is_err());
    assert_eq!(load_project(&path).unwrap(), project);
    invalid = project.clone();
    invalid.schema_version = 2;
    assert!(invalid.validate().is_err());
    invalid = project.clone();
    invalid.tracks[0].kind = "subtitle".into();
    assert!(invalid.validate().is_err());
    invalid = project.clone();
    invalid.tracks = (0..33)
        .map(|_| Track {
            id: uuid::Uuid::new_v4(),
            ..track.clone()
        })
        .collect();
    assert!(invalid.validate().is_err());
    let mut value = serde_json::to_value(&project).unwrap();
    value["tracks"][0]["unexpected"] = true.into();
    assert!(serde_json::from_value::<studiocut_ai::Project>(value).is_err());
    assert_eq!(history.travel(&project, true).unwrap().tracks.len(), 0);
}
