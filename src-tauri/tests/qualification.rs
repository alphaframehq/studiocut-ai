use std::{
    fs,
    path::Path,
    process::Command,
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};
use studiocut_ai::{
    create_project,
    edl::Effects,
    filtergraph::ExportSettings,
    finishing::Finishing,
    media::{ffmpeg_path, import_media},
    render::{bounded_command, ExportManager, Snapshot},
    timeline::{Caption, CaptionWord, Clip, Track},
    Project,
};
use uuid::Uuid;

fn run(command: Command) -> Vec<u8> {
    bounded_command(
        command,
        &AtomicBool::new(false),
        Duration::from_secs(90),
        32 * 1024 * 1024,
    )
    .unwrap()
}

fn ffmpeg(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut command = Command::new(ffmpeg_path());
    command
        .current_dir(root)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-n",
            "-filter_complex_threads",
            "1",
        ])
        .args(args);
    run(command)
}

fn track(asset_id: Uuid, kind: &str, start: u32, duration: u32, f: Finishing) -> Track {
    Track {
        id: Uuid::new_v4(),
        name: kind.into(),
        kind: kind.into(),
        muted: false,
        solo: false,
        locked: false,
        clips: vec![Clip {
            id: Uuid::new_v4(),
            asset_id,
            link_id: None,
            start,
            source_in: 0,
            duration,
            caption: None,
            effects: Some(Effects {
                finishing: Some(f),
                ..Default::default()
            }),
        }],
    }
}

fn export(project: &Project, root: &Path, name: &str, encoder: &str) -> Snapshot {
    let manager = ExportManager::default();
    let job = manager
        .start(
            project.clone(),
            root.join(name),
            ExportSettings {
                container: "mp4".into(),
                height: 1080,
                fps: 30,
                encoder: encoder.into(),
                ..Default::default()
            },
        )
        .unwrap();
    let deadline = Instant::now();
    loop {
        let state = manager.status(Some(job.id), None).unwrap().unwrap();
        if ["completed", "failed", "cancelled"].contains(&state.state.as_str()) {
            assert_eq!(state.state, "completed", "{state:?}");
            return state;
        }
        if deadline.elapsed() > Duration::from_secs(90) {
            manager.cancel(job.id).unwrap();
            panic!("qualification export timed out: {state:?}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn video_project(root: &Path) -> Project {
    ffmpeg(
        root,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30:duration=1",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-crf",
            "10",
            "source.mp4",
        ],
    );
    let asset = import_media(&root.join("cache"), &root.join("source.mp4")).unwrap();
    let mut project = create_project("Release qualification".into(), 1920, 1080, 30).unwrap();
    project.schema_version = 6;
    project
        .tracks
        .push(track(asset.id, "video", 0, 30, Finishing::default()));
    project.assets.push(asset);
    project
}

fn frame(root: &Path, file: &str, index: u32) -> Vec<u8> {
    ffmpeg(
        root,
        &[
            "-i",
            file,
            "-vf",
            &format!("select=eq(n\\,{index})"),
            "-frames:v",
            "1",
            "-threads",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "pipe:1",
        ],
    )
}

fn compare_frames(actual: &[u8], expected: &[u8], label: &str) {
    assert_eq!(actual.len(), 1920 * 1080 * 3);
    assert_eq!(actual.len(), expected.len());
    let mse = actual
        .iter()
        .zip(expected)
        .map(|(a, b)| (*a as f64 - *b as f64).powi(2))
        .sum::<f64>()
        / actual.len() as f64;
    let psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0_f64.powi(2) / mse).log10()
    };
    println!("{label}: RGB PSNR {psnr:.2} dB (minimum 38 dB)");
    assert!(psnr >= 38.0, "{label}: {psnr:.2} dB");
}

#[test]
fn grading_matches_independent_reference_export() {
    let root = tempfile::tempdir().unwrap();
    let mut project = video_project(root.path());
    let cube = "LUT_3D_SIZE 2\n0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n";
    project.tracks[0].clips[0]
        .effects
        .as_mut()
        .unwrap()
        .finishing = Some(Finishing {
        cube: Some(cube.into()),
        brightness: 0.04,
        contrast: 1.1,
        saturation: 0.8,
        balance: [0.05, -0.02, 0.03],
        ..Default::default()
    });
    export(&project, root.path(), "graded.mp4", "libx264");
    fs::write(root.path().join("reference.cube"), cube).unwrap();
    ffmpeg(root.path(), &[
        "-i", "source.mp4", "-vf",
        "scale=1920:1080,lut3d=reference.cube:interp=tetrahedral,eq=brightness=0.04:contrast=1.1:saturation=0.8,colorbalance=rs=0.05:gs=-0.02:bs=0.03,format=yuv420p",
        "-an", "-c:v", "libx264", "-threads", "2", "-preset", "veryfast", "-crf", "20", "reference.mp4",
    ]);
    for index in [3, 15, 27] {
        compare_frames(
            &frame(root.path(), "graded.mp4", index),
            &frame(root.path(), "reference.mp4", index),
            &format!("grading frame {index}"),
        );
    }
    ffmpeg(
        root.path(),
        &[
            "-i",
            "source.mp4",
            "-vf",
            "scale=1920:1080",
            "-an",
            "-c:v",
            "libx264",
            "-threads",
            "2",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "ungraded.mp4",
        ],
    );
    let actual = frame(root.path(), "graded.mp4", 15);
    let untreated = frame(root.path(), "ungraded.mp4", 15);
    let difference = actual
        .iter()
        .zip(untreated)
        .map(|(a, b)| (*a as f64 - b as f64).abs())
        .sum::<f64>()
        / actual.len() as f64;
    assert!(
        difference > 15.0,
        "grading must visibly change the reference: {difference}"
    );
}

fn reference_ass(preset: &str) -> String {
    let (face, bold, secondary, border, active) = match preset {
        "hormozi" => ("DejaVu Sans", -1, "&H0000FFFF", 1, "&H00FFFF&"),
        "minimal" => ("DejaVu Sans", 0, "&H00FFFFFF", 1, "&H00FFFF&"),
        "documentary" => ("DejaVu Serif", 0, "&H00FFFFFF", 3, "&H00FFFF&"),
        "neon" => ("DejaVu Sans", -1, "&H00FFFF00", 1, "&HFFFF00&"),
        _ => unreachable!(),
    };
    format!("[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: reference,{face},45,&H00FFFFFF,{secondary},&H00000000,&H80000000,{bold},0,0,0,100,100,0,0,{border},2,1,2,80,80,80,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.20,0:00:00.50,reference,,0,0,0,,{{\\c{active}}}FIRST {{\\c&HFFFFFF&}}SECOND\nDialogue: 0,0:00:00.50,0:00:00.80,reference,,0,0,0,,{{\\c&HFFFFFF&}}FIRST {{\\c{active}}}SECOND\n")
}

#[test]
fn caption_presets_match_independent_timed_reference_exports() {
    let root = tempfile::tempdir().unwrap();
    let mut project = video_project(root.path());
    let asset_id = project.assets[0].id;
    let mut caption = track(asset_id, "caption", 6, 18, Finishing::default());
    caption.clips[0].effects = None;
    caption.clips[0].caption = Some(Caption {
        preset: "minimal".into(),
        timing: "word".into(),
        words: vec![
            CaptionWord {
                text: "FIRST".into(),
                start: 0,
                end: 9,
            },
            CaptionWord {
                text: "SECOND".into(),
                start: 9,
                end: 18,
            },
        ],
    });
    project.tracks.push(caption);
    for preset in ["hormozi", "minimal", "documentary", "neon"] {
        project.tracks[1].clips[0].caption.as_mut().unwrap().preset = preset.into();
        let actual = format!("{preset}.mp4");
        let reference = format!("{preset}-reference.mp4");
        export(&project, root.path(), &actual, "libx264");
        fs::write(root.path().join("reference.ass"), reference_ass(preset)).unwrap();
        ffmpeg(
            root.path(),
            &[
                "-i",
                "source.mp4",
                "-vf",
                "scale=1920:1080,ass=reference.ass,format=yuv420p",
                "-an",
                "-c:v",
                "libx264",
                "-threads",
                "2",
                "-preset",
                "veryfast",
                "-crf",
                "20",
                &reference,
            ],
        );
        for index in [3, 6, 9, 15, 18, 24, 27] {
            let a = frame(root.path(), &actual, index);
            let b = frame(root.path(), &reference, index);
            let begin = 900 * 1920 * 3;
            compare_frames(&a, &b, &format!("{preset} frame {index}"));
            let mae = a[begin..]
                .iter()
                .zip(&b[begin..])
                .map(|(a, b)| (*a as f64 - *b as f64).abs())
                .sum::<f64>()
                / (a.len() - begin) as f64;
            // The bound is a rounding-noise floor, not a visual
            // threshold: a real caption defect (wrong text, position or
            // timing) shifts the region mean error into the tens, while
            // identity filter rounding (eq/colorbalance, overlay
            // blending) across FFmpeg builds measures about 1.0 — the
            // BtbN master build reads 1.04 here, the system build under
            // 1.0. A bound of 2.0 keeps the defect check strict while
            // accepting that build-to-build rounding.
            assert!(
                mae < 2.0,
                "caption region mismatch for {preset} at frame {index}: {mae}"
            );
        }
    }
}

fn samples(root: &Path, file: &str) -> Vec<f64> {
    let bytes = ffmpeg(
        root,
        &[
            "-i", file, "-vn", "-ac", "1", "-ar", "48000", "-f", "f32le", "pipe:1",
        ],
    );
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()) as f64)
        .collect()
}

fn tone_amplitude(samples: &[f64], start: f64, end: f64, hz: f64) -> f64 {
    let slice = &samples[(start * 48000.0) as usize..(end * 48000.0) as usize];
    let (sin, cos) = slice
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(sin, cos), (i, x)| {
            let angle = 2.0 * std::f64::consts::PI * hz * i as f64 / 48000.0;
            (sin + x * angle.sin(), cos + x * angle.cos())
        });
    2.0 * sin.hypot(cos) / slice.len() as f64
}

#[test]
fn mastering_matches_reference_and_ducks_music_under_voice() {
    let root = tempfile::tempdir().unwrap();
    ffmpeg(
        root.path(),
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.35*sin(2*PI*440*t)+0.002*sin(2*PI*7919*t):s=48000:d=1",
            "-c:a",
            "pcm_s16le",
            "voice.wav",
        ],
    );
    ffmpeg(
        root.path(),
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.08*sin(2*PI*1000*t):s=48000:d=2",
            "-c:a",
            "pcm_s16le",
            "music.wav",
        ],
    );
    let mut project = create_project("Mastering qualification".into(), 1920, 1080, 30).unwrap();
    project.schema_version = 6;
    for (file, start, duration, finishing) in [
        (
            "voice.wav",
            15,
            30,
            Finishing {
                role: "voice".into(),
                denoise: true,
                level: true,
                ..Default::default()
            },
        ),
        (
            "music.wav",
            0,
            60,
            Finishing {
                role: "music".into(),
                auto_duck: true,
                ..Default::default()
            },
        ),
    ] {
        let asset = import_media(&root.path().join("cache"), &root.path().join(file)).unwrap();
        project
            .tracks
            .push(track(asset.id, "audio", start, duration, finishing));
        project.assets.push(asset);
    }
    export(&project, root.path(), "mastered.mp4", "libx264");
    ffmpeg(root.path(), &[
        "-i", "voice.wav", "-af",
        "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,afftdn=nr=12:nf=-40,acompressor=threshold=0.125:ratio=3:attack=10:release=150:makeup=2,apad,atrim=duration=1,adelay=24000S:all=1,apad,atrim=duration=2",
        "-c:a", "pcm_f32le", "processed-voice.wav",
    ]);
    ffmpeg(root.path(), &[
        "-i", "processed-voice.wav", "-i", "music.wav", "-i", "processed-voice.wav", "-filter_complex",
        "[0:a]apad,atrim=duration=3[voice];[2:a]apad,atrim=duration=3[control];[1:a]aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,apad,atrim=duration=3[music];[music][control]sidechaincompress=threshold=0.03:ratio=8:attack=15:release=300[ducked];[voice][ducked]amix=inputs=2:normalize=0:duration=longest:dropout_transition=0,alimiter=limit=0.95:level=0:latency=1,atrim=duration=2[out]", 
        "-map", "[out]", "-c:a", "pcm_f32le", "-ar", "48000", "-ac", "2", "reference.wav",
    ]);
    ffmpeg(
        root.path(),
        &[
            "-i",
            "reference.wav",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
            "-ac",
            "2",
            "reference.m4a",
        ],
    );
    let reference_pcm = samples(root.path(), "reference.wav");
    assert_eq!(reference_pcm.len(), 96000);
    println!(
        "Reference music before/during/recovery: {}/{}/{}",
        tone_amplitude(&reference_pcm, 0.1, 0.4, 1000.0),
        tone_amplitude(&reference_pcm, 0.8, 1.3, 1000.0),
        tone_amplitude(&reference_pcm, 1.8, 1.95, 1000.0)
    );
    let actual = samples(root.path(), "mastered.mp4");
    let reference = samples(root.path(), "reference.m4a");
    assert_eq!(
        actual.len(),
        reference.len(),
        "mastered audio duration differs from reference: {}s versus {}s; compiled graph: {}",
        actual.len() as f64 / 48000.0,
        reference.len() as f64 / 48000.0,
        studiocut_ai::filtergraph::compile(
            &project,
            &ExportSettings {
                container: "mp4".into(),
                height: 1080,
                fps: 30,
                encoder: "libx264".into(),
                ..Default::default()
            }
        )
        .unwrap()
        .graph
    );
    assert!(actual.len() >= 96000);
    let rmse = (actual
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f64>()
        / actual.len() as f64)
        .sqrt();
    let before = tone_amplitude(&actual, 0.1, 0.4, 1000.0);
    let during = tone_amplitude(&actual, 0.8, 1.3, 1000.0);
    let recovery = tone_amplitude(&actual, 1.8, 1.95, 1000.0);
    let peak = actual.iter().map(|n| n.abs()).fold(0.0_f64, f64::max);
    println!("Mastering: reference RMSE {rmse:.6} (maximum 0.003); music amplitude before/during/recovery {before:.4}/{during:.4}/{recovery:.4}; peak {peak:.4}");
    let reference_before = tone_amplitude(&reference_pcm, 0.1, 0.4, 1000.0);
    let reference_during = tone_amplitude(&reference_pcm, 0.8, 1.3, 1000.0);
    let reference_recovery = tone_amplitude(&reference_pcm, 1.8, 1.95, 1000.0);
    assert!(reference_before > 0.04 && reference_during < reference_before * 0.6);
    assert!(reference_recovery > reference_during * 1.5);
    assert!(rmse < 0.003, "reference PCM mismatch: {rmse}");
    assert!(before > 0.04 && during < before * 0.6 && recovery > during * 1.5);
    assert!(tone_amplitude(&actual, 0.8, 1.3, 440.0) > 0.08);
    assert!(peak <= 1.0 && actual.iter().all(|n| n.is_finite()));
}

#[test]
fn requested_encoders_render_or_disclose_cpu_fallback() {
    let root = tempfile::tempdir().unwrap();
    let project = video_project(root.path());
    for encoder in ["h264_nvenc", "hevc_videotoolbox", "h264_amf", "h264_qsv"] {
        let done = export(&project, root.path(), &format!("{encoder}.mp4"), encoder);
        let selected = done.encoder.as_deref().unwrap();
        assert!(selected == encoder || selected == "libx264");
        if selected == "libx264" {
            assert!(done.disclosure.contains("CPU fallback"));
        }
        println!(
            "Requested {encoder}: selected {selected}; {}",
            done.disclosure
        );
    }
}

#[test]
fn delayed_speed_adjusted_voice_and_music_preserve_audio_content() {
    let root = tempfile::tempdir().unwrap();
    for (file, hz) in [("voice.m4a", 440), ("music.m4a", 1000)] {
        ffmpeg(
            root.path(),
            &[
                "-f",
                "lavfi",
                "-i",
                &format!("sine=frequency={hz}:sample_rate=48000:duration=2"),
                "-c:a",
                "aac",
                file,
            ],
        );
    }
    for speed in [0.5, 1.0, 1.5, 2.0] {
        let mut project = create_project("Delayed mastering".into(), 1920, 1080, 30).unwrap();
        project.schema_version = 6;
        for (file, role, clip_speed) in [("voice.m4a", "voice", 1.0), ("music.m4a", "music", speed)]
        {
            let asset = import_media(&root.path().join("cache"), &root.path().join(file)).unwrap();
            let mut audio = track(
                asset.id,
                "audio",
                6,
                24,
                Finishing {
                    speed: clip_speed,
                    role: role.into(),
                    auto_duck: role == "music",
                    denoise: true,
                    level: true,
                    ..Default::default()
                },
            );
            audio.clips[0].source_in = 3;
            project.tracks.push(audio);
            project.assets.push(asset);
        }
        let file = format!("delayed-{speed}.mp4");
        export(&project, root.path(), &file, "libx264");
        let audio = samples(root.path(), &file);
        assert!((48000..=49024).contains(&audio.len()));
        let leading_peak = audio[..7200]
            .iter()
            .map(|n| n.abs())
            .fold(0.0_f64, f64::max);
        let voice = tone_amplitude(&audio, 0.4, 0.8, 440.0);
        let music = tone_amplitude(&audio, 0.4, 0.8, 1000.0);
        let tail_voice = tone_amplitude(&audio, 0.85, 0.95, 440.0);
        println!("Delayed speed {speed}: samples {}, leading peak {leading_peak:.6}, voice {voice:.4}, music {music:.4}, tail voice {tail_voice:.4}", audio.len());
        assert!(leading_peak < 0.001);
        assert!(voice > 0.05 && music > 0.001 && tail_voice > 0.05);
        assert!(audio.iter().all(|n| n.is_finite() && n.abs() <= 1.0));
    }
}
