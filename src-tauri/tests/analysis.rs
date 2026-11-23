use std::{fs, process::Command};
use studiocut_ai::{
    analysis::{self, Cut, SilenceSettings},
    create_project, load_project, media, save_project,
    timeline::{self, Clip, History, Track},
    transcription::{self, TimedText, Transcript},
    Project,
};
use uuid::Uuid;

fn fixture(root: &std::path::Path) -> Project {
    let input = root.join("speech-and-silence.wav");
    assert!(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=if(between(t\\,1\\,2)\\,0\\,0.3*sin(2*PI*440*t)):s=48000:d=4",
            "-c:a",
            "pcm_s16le"
        ])
        .arg(&input)
        .status()
        .unwrap()
        .success());
    let asset = media::import_media(&root.join("cache"), &input).unwrap();
    let mut project = create_project("DSP roundtrip".into(), 1920, 1080, 30).unwrap();
    project.schema_version = 3;
    let clip = Clip {
        id: Uuid::new_v4(),
        asset_id: asset.id,
        link_id: None,
        start: 30,
        source_in: 0,
        duration: 120,
        caption: None,
        effects: None,
    };
    project.assets.push(asset);
    project.tracks.push(Track {
        id: Uuid::new_v4(),
        name: "Audio".into(),
        kind: "audio".into(),
        muted: false,
        solo: false,
        locked: false,
        clips: vec![clip],
    });
    project.validate().unwrap();
    project
}

#[test]
fn real_dsp_wav_captions_ripple_and_atomic_undo() {
    let root = tempfile::tempdir().unwrap();
    let mut project = fixture(root.path());
    let original = fs::read(&project.assets[0].source_path).unwrap();
    let id = project.tracks[0].clips[0].id;
    let cuts = analysis::detect(&project, id, SilenceSettings::default()).unwrap();
    assert_eq!(cuts, vec![Cut { start: 63, end: 87 }]);
    let wav = transcription::extract_wav(&project.assets[0], 0.5, 2.0).unwrap();
    let wav_path = root.path().join("extracted.wav");
    fs::write(&wav_path, wav).unwrap();
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(wav_path)
        .output()
        .unwrap();
    assert!(probe.status.success());
    let metadata: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(metadata["streams"][0]["sample_rate"], "16000");
    assert_eq!(metadata["streams"][0]["channels"], 1);
    assert_eq!(metadata["streams"][0]["codec_name"], "pcm_s16le");
    let transcript = Transcript {
        timing: "word".into(),
        items: vec![
            TimedText {
                text: "Before".into(),
                start: 0.1,
                end: 0.8,
            },
            TimedText {
                text: "After".into(),
                start: 2.1,
                end: 2.8,
            },
        ],
    };
    let caption = transcription::caption_track(&project, id, &transcript, "neon").unwrap();
    project.tracks.push(caption);
    project.schema_version = 4;
    project.validate().unwrap();
    let path = root.path().join("captions.studiocut");
    save_project(&project, &path).unwrap();
    assert_eq!(load_project(&path).unwrap(), project);
    let candidate = analysis::ripple(&project, &cuts, project.revision).unwrap();
    assert_eq!(candidate.tracks[0].clips.len(), 2);
    assert_eq!(candidate.tracks[0].clips[1].start, 63);
    assert_eq!(candidate.tracks[0].clips[1].source_in, 57);
    assert_eq!(candidate.tracks[1].clips.len(), 2);
    let mut history = History::default();
    let edited = history
        .apply(&project, candidate.tracks, project.revision)
        .unwrap();
    assert_eq!(
        history.travel(&edited, true).unwrap().tracks,
        project.tracks
    );
    assert!(history.travel(&project, true).is_err());
    assert_eq!(fs::read(&project.assets[0].source_path).unwrap(), original);
    assert!(analysis::ripple(
        &project,
        &[Cut { start: 50, end: 70 }, Cut { start: 60, end: 80 }],
        project.revision
    )
    .is_err());
    assert!(analysis::ripple(&project, &cuts, project.revision + 1).is_err());
    project.tracks[1].locked = true;
    assert!(analysis::ripple(&project, &cuts, project.revision)
        .unwrap_err()
        .contains("Unlock"));
}

#[test]
fn linked_ripple_matches_typescript_and_preserves_pairs() {
    let root = tempfile::tempdir().unwrap();
    let mut project = fixture(root.path());
    let asset = &mut project.assets[0];
    asset.kind = "video".into();
    asset.video_stream = Some(1);
    asset.width = Some(160);
    asset.height = Some(90);
    let mut video = project.tracks[0].clone();
    video.id = Uuid::new_v4();
    video.kind = "video".into();
    video.name = "Video".into();
    video.clips[0].id = Uuid::new_v4();
    let link = Some(Uuid::new_v4());
    video.clips[0].link_id = link;
    project.tracks[0].clips[0].link_id = link;
    project.tracks.push(video);
    let input = root.path().join("input.json");
    let output = root.path().join("output.json");
    fs::write(&input, serde_json::to_vec(&project).unwrap()).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/analysis.mjs");
    let result = Command::new("node")
        .arg(script)
        .arg(&input)
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let browser: Project = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    browser.validate().unwrap();
    let cuts = vec![
        Cut { start: 63, end: 87 },
        Cut {
            start: 110,
            end: 120,
        },
    ];
    let native = analysis::ripple(&project, &cuts, project.revision).unwrap();
    for (a, b) in native.tracks.iter().zip(&browser.tracks) {
        assert_eq!(a.clips.len(), b.clips.len());
        for (a, b) in a.clips.iter().zip(&b.clips) {
            assert_eq!(
                (a.start, a.source_in, a.duration),
                (b.start, b.source_in, b.duration)
            );
        }
    }
    for i in 0..native.tracks[0].clips.len() {
        assert_eq!(
            native.tracks[0].clips[i].link_id,
            native.tracks[1].clips[i].link_id
        );
    }
    let whole =
        analysis::ripple(&project, &[Cut { start: 0, end: 150 }], project.revision).unwrap();
    assert!(whole.tracks.iter().all(|t| t.clips.is_empty()));
    assert!(timeline::replace_tracks(&project, browser.tracks, project.revision).is_ok());
}
