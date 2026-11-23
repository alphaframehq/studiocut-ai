use std::{fs, path::Path, process::Command, time::Instant};
use studiocut_ai::media::{import_media, media_preview, open_index, MIGRATE_DOWN, MIGRATE_UP};

fn fixture(path: &Path, audio_only: bool) {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-v",
        "error",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=1",
    ]);
    if !audio_only {
        command.args([
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:size=160x90:rate=24:duration=1",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-shortest",
        ]);
    }
    assert!(command.arg(path).status().unwrap().success());
}

#[test]
fn real_import_metadata_thumbnail_waveform_and_deduplication() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cache");
    let path = directory.path().join("clip with spaces ü.mp4");
    fixture(&path, false);
    let original = fs::read(&path).unwrap();
    let asset = import_media(&root, &path).unwrap();
    assert_eq!(asset.kind, "video");
    assert_eq!((asset.width, asset.height), (Some(160), Some(90)));
    assert_eq!(asset.fps, Some(24.0));
    assert!((asset.duration.unwrap() - 1.0).abs() < 0.1);
    assert_eq!(asset.sample_rate, Some(48000));
    let preview = media_preview(&root, &asset).unwrap();
    assert!(preview.thumbnail.unwrap().starts_with(&[0xff, 0xd8]));
    let peaks = preview.peaks.unwrap();
    assert!(!peaks.is_empty() && peaks.len() <= 2000);
    assert!(peaks
        .iter()
        .all(|p| p.is_finite() && (0.0..=1.0).contains(p)));
    assert!(peaks.iter().any(|p| *p > 0.05));
    assert_eq!(import_media(&root, &path).unwrap(), asset);
    fs::remove_file(root.join(asset.id.to_string()).join("thumbnail.jpg")).unwrap();
    assert_eq!(import_media(&root, &path).unwrap(), asset);
    assert!(media_preview(&root, &asset).unwrap().thumbnail.is_some());
    assert_eq!(fs::read(&path).unwrap(), original);
    let mut project =
        studiocut_ai::create_project("Media roundtrip".into(), 1920, 1080, 30).unwrap();
    project.schema_version = 2;
    project.revision = 1;
    project.assets.push(asset.clone());
    let project_path = directory.path().join("media.studiocut");
    studiocut_ai::save_project(&project, &project_path).unwrap();
    assert_eq!(studiocut_ai::load_project(&project_path).unwrap(), project);
    let mut invalid = asset.clone();
    invalid.duration = Some(21600.0);
    assert!(invalid.validate().is_ok());
    invalid.duration = Some(21600.1);
    assert!(invalid.validate().is_err());
    invalid.duration = Some(0.0);
    assert!(invalid.validate().is_err());
    let db = open_index(&root).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM assets", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let audio = directory.path().join("sound.wav");
    fixture(&audio, true);
    let sound = import_media(&root, &audio).unwrap();
    assert_eq!(sound.kind, "audio");
    assert!(media_preview(&root, &sound).unwrap().thumbnail.is_none());
    let image = directory.path().join("still.png");
    assert!(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=80x60",
            "-frames:v",
            "1",
            "-threads",
            "1"
        ])
        .arg(&image)
        .status()
        .unwrap()
        .success());
    let still = import_media(&root, &image).unwrap();
    assert_eq!(still.kind, "image");
    assert!(still.duration.is_none());
    assert!(media_preview(&root, &still).unwrap().peaks.is_none());
    let silent = directory.path().join("silent.mp4");
    assert!(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=s=80x60:d=1",
            "-c:v",
            "mpeg4"
        ])
        .arg(&silent)
        .status()
        .unwrap()
        .success());
    let silent_asset = import_media(&root, &silent).unwrap();
    assert!(silent_asset.audio_stream.is_none());
    assert!(media_preview(&root, &silent_asset).unwrap().peaks.is_none());
    let broken = directory.path().join("broken.mp4");
    fs::write(&broken, b"not video").unwrap();
    assert!(import_media(&root, &broken).is_err());
    assert!(import_media(&root, &directory.path().join("missing")).is_err());
    fs::write(&broken, b"").unwrap();
    assert!(import_media(&root, &broken).is_err());
    assert_eq!(
        db.query_row("SELECT count(*) FROM assets", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn migration_roundtrip_and_index_query_plan() {
    let directory = tempfile::tempdir().unwrap();
    let mut db = open_index(directory.path()).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute_batch(MIGRATE_DOWN).unwrap();
    assert_eq!(
        tx.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))
            .unwrap(),
        0
    );
    tx.execute_batch(MIGRATE_UP).unwrap();
    tx.commit().unwrap();
    db.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10000)
        INSERT INTO assets(id,source_path,name,size_bytes,modified_ns,kind,duration,audio_stream,created_at)
        SELECT printf('asset-%d',x),printf('/media/%d.wav',x),'sound',100,'1','audio',1,0,1 FROM n;").unwrap();
    let query = "SELECT id FROM assets WHERE source_path='/media/9999.wav' AND size_bytes=100 AND modified_ns='1'";
    let indexed: String = db
        .query_row(&format!("EXPLAIN QUERY PLAN {query}"), [], |r| r.get(3))
        .unwrap();
    let start = Instant::now();
    for _ in 0..200 {
        db.query_row(query, [], |r| r.get::<_, String>(0)).unwrap();
    }
    let indexed_time = start.elapsed();
    let scan_query = query.replace("FROM assets", "FROM assets NOT INDEXED");
    let scan: String = db
        .query_row(&format!("EXPLAIN QUERY PLAN {scan_query}"), [], |r| {
            r.get(3)
        })
        .unwrap();
    let start = Instant::now();
    for _ in 0..200 {
        db.query_row(&scan_query, [], |r| r.get::<_, String>(0))
            .unwrap();
    }
    println!(
        "10,000 assets / 200 lookups: before {scan} {:?}; after {indexed} {indexed_time:?}",
        start.elapsed()
    );
    assert!(indexed.contains("assets_source_version"));
    assert!(scan.contains("SCAN"));
    assert!(db
        .execute("INSERT INTO assets SELECT * FROM assets LIMIT 1", [])
        .is_err());
    assert!(db
        .execute("UPDATE assets SET duration=-1 WHERE id='asset-1'", [])
        .is_err());
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
