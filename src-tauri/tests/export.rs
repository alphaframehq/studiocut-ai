use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use studiocut_ai::{
    create_project,
    edl::{Effects, Lut},
    filtergraph::{compile, ExportSettings},
    finishing::{preset, validate_cube, Finishing, TransformKey},
    media::{ffmpeg_path, ffprobe_path, import_media},
    render::{parse_progress, ExportManager, Snapshot},
    timeline::{Clip, Track},
};
use uuid::Uuid;
fn settings() -> ExportSettings {
    ExportSettings {
        container: "mp4".into(),
        height: 1080,
        fps: 30,
        encoder: "libx264".into(),
        ..Default::default()
    }
}
fn fixture(root: &Path) -> studiocut_ai::Project {
    let source = root.join("source ' [clip].mp4");
    assert!(Command::new(ffmpeg_path())
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=30:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-threads",
            "1",
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
    let mut p = create_project("Export integration".into(), 1920, 1080, 30).unwrap();
    p.schema_version = 6;
    let asset = import_media(&root.join("cache"), &source).unwrap();
    for kind in ["video", "audio"] {
        let mut f = Finishing::default();
        if kind == "video" {
            f.brightness = 0.02;
            f.cube = Some(preset("warm").unwrap());
            f.transforms = vec![
                TransformKey {
                    frame: 0,
                    scale: 0.8,
                    x: 0.0,
                    y: 0.0,
                    opacity: 0.6,
                },
                TransformKey {
                    frame: 60,
                    scale: 1.0,
                    x: 0.1,
                    y: 0.0,
                    opacity: 1.0,
                },
            ];
        } else {
            f.role = "voice".into();
            f.denoise = true;
            f.level = true;
        }
        p.tracks.push(Track {
            id: Uuid::new_v4(),
            name: kind.into(),
            kind: kind.into(),
            muted: false,
            solo: false,
            locked: false,
            clips: vec![Clip {
                id: Uuid::new_v4(),
                asset_id: asset.id,
                link_id: None,
                start: 6,
                source_in: 3,
                duration: 24,
                caption: None,
                effects: Some(Effects {
                    finishing: Some(f),
                    ..Default::default()
                }),
            }],
        });
    }
    let mut music = p.tracks[1].clone();
    music.id = Uuid::new_v4();
    music.name = "BGM".into();
    music.clips[0].id = Uuid::new_v4();
    let f = music.clips[0]
        .effects
        .as_mut()
        .unwrap()
        .finishing
        .as_mut()
        .unwrap();
    f.role = "music".into();
    f.auto_duck = true;
    f.speed = 1.5;
    p.tracks.push(music);
    let mut overlay = p.tracks[0].clone();
    overlay.id = Uuid::new_v4();
    overlay.clips[0].id = Uuid::new_v4();
    overlay.clips[0].start = 0;
    overlay.clips[0].duration = 12;
    overlay.clips[0].effects = Some(Effects {
        lut: Some(Lut::Cool),
        ..Default::default()
    });
    p.tracks.push(overlay);
    p.tracks.push(Track {
        id: Uuid::new_v4(),
        name: "Captions".into(),
        kind: "caption".into(),
        muted: false,
        solo: false,
        locked: false,
        clips: vec![Clip {
            id: Uuid::new_v4(),
            asset_id: asset.id,
            link_id: None,
            start: 6,
            source_in: 3,
            duration: 24,
            effects: None,
            caption: Some(studiocut_ai::timeline::Caption {
                preset: "neon".into(),
                timing: "word".into(),
                words: vec![studiocut_ai::timeline::CaptionWord {
                    text: "Real {export} \\ test".into(),
                    start: 3,
                    end: 27,
                }],
            }),
        }],
    });
    p.assets.push(asset);
    p.validate().unwrap();
    p
}
fn wait(manager: &ExportManager, id: Uuid) -> Snapshot {
    let start = Instant::now();
    loop {
        let s = manager.status(Some(id), None).unwrap().unwrap();
        if ["completed", "failed", "cancelled"].contains(&s.state.as_str()) {
            return s;
        }
        assert!(
            start.elapsed() < Duration::from_secs(90),
            "export timed out: {s:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}
#[test]
fn real_export_progress_reconnect_cancellation_and_atomic_publication() {
    let root = tempfile::tempdir().unwrap();
    let p = fixture(root.path());
    let a = compile(&p, &settings()).unwrap();
    let b = compile(&p, &settings()).unwrap();
    assert_eq!(a, b);
    for filter in [
        "overlay=",
        "afftdn=",
        "acompressor=",
        "sidechaincompress=",
        "lut3d=",
        "ass=",
        "geq=",
        "atempo=1.50000000",
    ] {
        assert!(a.graph.contains(filter), "{filter}");
    }
    let manager = ExportManager::default();
    let output = root.path().join("finished.mp4");
    let initial = manager
        .start(p.clone(), output.clone(), settings())
        .unwrap();
    assert!(manager
        .start(p.clone(), root.path().join("other.mp4"), settings())
        .is_err());
    let reader1 = manager.clone();
    let reader2 = manager.clone();
    let id = initial.id;
    let client = thread::spawn(move || {
        let mut sequence = 0;
        for _ in 0..10 {
            let s = reader1.status(Some(id), Some(sequence)).unwrap().unwrap();
            assert!(s.sequence >= sequence);
            sequence = s.sequence;
            thread::sleep(Duration::from_millis(25));
        }
        sequence
    });
    let before = reader2.status(Some(id), None).unwrap().unwrap();
    assert_eq!(before.version, 1);
    let final_state = wait(&manager, id);
    assert_eq!(final_state.state, "completed", "{final_state:?}");
    assert_eq!(final_state.progress, 1.0);
    assert!(client.join().unwrap() >= initial.sequence);
    let replay = reader2
        .status(Some(id), Some(before.sequence))
        .unwrap()
        .unwrap();
    assert_eq!(replay.state, "completed");
    assert!(replay.sequence > before.sequence);
    assert!(output.is_file());
    assert_eq!(manager.cancel(id).unwrap().state, "completed");
    let bytes = fs::read(&output).unwrap();
    assert!(manager
        .start(p.clone(), output.clone(), settings())
        .is_err());
    assert_eq!(bytes, fs::read(&output).unwrap());
    let mut long = p.clone();
    long.tracks[0].clips[0].start = 3000;
    let cancel_path = root.path().join("cancelled.mp4");
    let job = manager
        .start(long, cancel_path.clone(), settings())
        .unwrap();
    let deadline = Instant::now();
    loop {
        let s = manager.status(Some(job.id), None).unwrap().unwrap();
        if s.state == "rendering" {
            break;
        }
        assert!(
            !["failed", "completed"].contains(&s.state.as_str()),
            "{s:?}"
        );
        assert!(deadline.elapsed() < Duration::from_secs(20));
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(100));
    manager.cancel(job.id).unwrap();
    manager.cancel(job.id).unwrap();
    assert_eq!(wait(&manager, job.id).state, "cancelled");
    assert!(!cancel_path.exists());
    assert!(!fs::read_dir(root.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".studiocut-export-")));
    assert_eq!(fs::read(&output).unwrap(), bytes);
}
#[test]
fn cube_settings_and_progress_validation() {
    for name in ["identity", "warm", "cool"] {
        assert_eq!(validate_cube(&preset(name).unwrap()).unwrap(), 17);
    }
    for cube in [
        "LUT_3D_SIZE 2\n0 0 0",
        "LUT_3D_SIZE 100",
        "LUT_3D_SIZE 2\nNaN 0 0",
        "LUT_1D_SIZE 2",
    ] {
        assert!(validate_cube(cube).is_err());
    }
    assert_eq!(parse_progress("out_time_us=500000", 1.0), Some(0.5));
    assert_eq!(parse_progress("out_time_us=N/A", 1.0), None);
    assert_eq!(parse_progress("out_time_us=NaN", 1.0), None);
    assert_eq!(parse_progress("out_time_us=-1", 1.0), None);
    let mut config = settings();
    config.height = 720;
    assert!(config.validate().is_err());
}

#[test]
fn mov_4k_frame_rates_solo_and_persisted_finishing() {
    let root = tempfile::tempdir().unwrap();
    let mut p = fixture(root.path());
    p.tracks.retain(|t| t.kind != "caption");
    for track in &mut p.tracks {
        track.clips[0].start = 0;
        track.clips[0].duration = 6;
        track.clips[0].effects = None;
    }
    p.tracks[0].solo = true;
    let graph = compile(&p, &settings()).unwrap();
    assert_eq!(
        graph.inputs.iter().filter(|v| v.as_str() == "-i").count(),
        3
    );
    p.tracks[0].muted = true;
    assert!(!compile(&p, &settings()).unwrap().graph.contains("overlay="));
    p.tracks[0].muted = false;
    p.tracks[0].clips[0].effects = Some(Effects {
        finishing: Some(Finishing {
            speed: 2.0,
            ..Default::default()
        }),
        ..Default::default()
    });
    let path = root.path().join("finishing.studiocut");
    studiocut_ai::save_project(&p, &path).unwrap();
    let reopened = studiocut_ai::load_project(&path).unwrap();
    assert_eq!(reopened, p);
    let mut history = studiocut_ai::timeline::History::default();
    let mut tracks = p.tracks.clone();
    tracks[0].clips[0].effects = None;
    let next = history.apply(&p, tracks, p.revision).unwrap();
    assert!(next.tracks[0].clips[0].effects.is_none());
    assert_eq!(history.travel(&next, true).unwrap().tracks, p.tracks);
    let manager = ExportManager::default();
    for (container, height, fps) in [("mov", 2160, 24), ("mp4", 1080, 60)] {
        let config = ExportSettings {
            container: container.into(),
            height,
            fps,
            encoder: "hevc_videotoolbox".into(),
            ..Default::default()
        };
        let output = root.path().join(format!("short-{fps}.{container}"));
        let job = manager
            .start(reopened.clone(), output.clone(), config)
            .unwrap();
        let done = wait(&manager, job.id);
        assert_eq!(done.state, "completed", "{done:?}");
        if cfg!(target_os = "linux") {
            assert_eq!(done.encoder.as_deref(), Some("libx264"));
            assert!(done.disclosure.contains("CPU fallback"));
        }
        assert!(fs::metadata(output).unwrap().len() > 1000);
    }
    let mut invalid = reopened.clone();
    invalid.tracks[0].clips[0]
        .effects
        .as_mut()
        .unwrap()
        .finishing
        .as_mut()
        .unwrap()
        .speed = 4.0;
    invalid.tracks[0].clips[0].duration = 60;
    assert!(invalid.validate().is_err());
    let mut locked = reopened.clone();
    locked.tracks[0].locked = true;
    assert!(history
        .apply(&locked, next.tracks, locked.revision)
        .is_err());
    let mut bad = reopened.clone();
    bad.assets[0].source_path = root
        .path()
        .join("missing.mp4")
        .to_string_lossy()
        .into_owned();
    let output = root.path().join("failed.mp4");
    let job = manager.start(bad, output.clone(), settings()).unwrap();
    assert_eq!(wait(&manager, job.id).state, "failed");
    assert!(!output.exists());
}

#[test]
fn realtime_channel_two_clients_delivery_replay_and_vanished_reader() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let root = tempfile::tempdir().unwrap();
    let mut project = fixture(root.path());
    project.tracks[0].clips[0].start = 60;
    let manager = ExportManager::default();
    assert!(manager.progress(None, None).unwrap().is_none());
    let job = manager
        .start(project, root.path().join("channel.mp4"), settings())
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let readers: Vec<_> = [false, true]
        .into_iter()
        .map(|vanish| {
            let client = manager.clone();
            let barrier = barrier.clone();
            let id = job.id;
            thread::spawn(move || {
                barrier.wait();
                let mut cursor = 1;
                let mut max_latency = 0;
                let mut saw_ffmpeg = false;
                let deadline = Instant::now();
                loop {
                    let batch = client.progress(Some(id), Some(cursor)).unwrap().unwrap();
                    assert_eq!(batch.version, 1);
                    assert_eq!(batch.message_type, "export.progress");
                    assert!(!batch.reset);
                    for event in batch.events {
                        assert_eq!(event.id, id);
                        assert_eq!(event.sequence, cursor + 1);
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;
                        let latency = now.saturating_sub(event.timestamp);
                        max_latency = max_latency.max(latency);
                        assert!(latency < 1000, "delivery exceeded one second: {latency} ms");
                        cursor = event.sequence;
                        saw_ffmpeg |= event.state == "rendering" && event.progress > 0.0;
                        if vanish && saw_ffmpeg {
                            return (cursor, max_latency, saw_ffmpeg);
                        }
                        if event.state == "completed" {
                            assert!(saw_ffmpeg);
                            return (cursor, max_latency, saw_ffmpeg);
                        }
                        assert!(
                            !["failed", "cancelled"].contains(&event.state.as_str()),
                            "{event:?}"
                        );
                    }
                    assert!(deadline.elapsed() < Duration::from_secs(90));
                    thread::sleep(Duration::from_millis(25));
                }
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = readers
        .into_iter()
        .map(|reader| reader.join().unwrap())
        .collect();
    let (completed_cursor, latency1, _) = results[0];
    let (disconnected_cursor, latency2, _) = results[1];
    assert!(disconnected_cursor < completed_cursor);
    let replay = manager
        .progress(Some(job.id), Some(disconnected_cursor))
        .unwrap()
        .unwrap();
    assert!(!replay.reset);
    assert!(!replay.events.is_empty());
    assert_eq!(replay.events[0].sequence, disconnected_cursor + 1);
    assert_eq!(replay.events.last().unwrap().state, "completed");
    for pair in replay.events.windows(2) {
        assert_eq!(pair[1].sequence, pair[0].sequence + 1);
    }
    let duplicate = manager
        .progress(Some(job.id), Some(disconnected_cursor))
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&replay).unwrap(),
        serde_json::to_value(duplicate).unwrap()
    );
    assert!(manager
        .progress(Some(job.id), Some(completed_cursor))
        .unwrap()
        .unwrap()
        .events
        .is_empty());
    assert!(manager
        .progress(Some(job.id), Some(completed_cursor + 1))
        .is_err());
    let reset = manager
        .progress(Some(Uuid::new_v4()), Some(completed_cursor))
        .unwrap()
        .unwrap();
    assert!(reset.reset);
    assert_eq!(reset.events[0].id, job.id);
    assert!(root.path().join("channel.mp4").is_file());
    println!("Real FFmpeg progress: two clients max latency {latency1}/{latency2} ms; {} missed events replayed after reader disconnected; export completed", replay.events.len());
}

fn vertical_settings(fill: &str) -> ExportSettings {
    ExportSettings {
        container: "mp4".into(),
        height: 1080,
        fps: 30,
        encoder: "libx264".into(),
        aspect: "vertical".into(),
        fill: fill.into(),
    }
}

fn probe_dimensions(path: &Path) -> (u32, u32) {
    let out = Command::new(ffprobe_path())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let (w, h) = text.trim().split_once(',').unwrap();
    (w.parse().unwrap(), h.parse().unwrap())
}

#[test]
fn vertical_and_square_exports_render_native_canvases() {
    let root = tempfile::tempdir().unwrap();
    let p = fixture(root.path());
    let cover = compile(&p, &vertical_settings("cover")).unwrap();
    assert!(
        cover.graph.contains("color=c=black:s=608x1080"),
        "vertical base canvas must be 608x1080"
    );
    assert!(
        cover.graph.contains("crop=608:1080"),
        "cover must center-crop"
    );
    let fit = compile(&p, &vertical_settings("fit")).unwrap();
    assert!(fit.graph.contains("color=c=black:s=608x1080"));
    assert!(fit.graph.contains("pad=608:1080"), "fit must pillarbox");
    assert!(compile(
        &p,
        &ExportSettings {
            aspect: "square".into(),
            fill: "fit".into(),
            ..vertical_settings("fit")
        }
    )
    .unwrap()
    .graph
    .contains("color=c=black:s=1080x1080"));
    let manager = ExportManager::default();
    let output = root.path().join("vertical.mp4");
    let job = manager
        .start(p.clone(), output.clone(), vertical_settings("cover"))
        .unwrap();
    let final_state = wait(&manager, job.id);
    assert_eq!(final_state.state, "completed", "{final_state:?}");
    assert_eq!(probe_dimensions(&output), (608, 1080));
}
