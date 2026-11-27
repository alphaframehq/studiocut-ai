//! Integration tests for the proxy workflow. The generation test needs
//! `ffmpeg`/`ffprobe` on PATH (as in `tests/media.rs`); it is
//! CI-verified only. Pure path/state logic is covered by unit tests in
//! `src/proxy.rs`, which need no FFmpeg.

use std::{fs, path::Path, process::Command};
use studiocut_ai::{
    media::{ffmpeg_path, import_media},
    proxy::{delete_proxy, generate_proxy, proxy_path_for, proxy_source, proxy_status, ProxyState},
};

fn video_fixture(path: &Path, size: &str) {
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
            &format!("color=c=blue:size={size}:rate=30:duration=1"),
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

fn audio_fixture(path: &Path) {
    assert!(Command::new(ffmpeg_path())
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
        ])
        .arg(path)
        .status()
        .unwrap()
        .success());
}

#[test]
fn proxy_roundtrip_keeps_the_original_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cache");
    let path = directory.path().join("proxy-clip.mp4");
    video_fixture(&path, "1280x720");
    let asset = import_media(&root, &path).unwrap();
    let original = fs::read(&path).unwrap();

    assert_eq!(proxy_status(&root, &asset).state, ProxyState::Missing);
    assert!(proxy_source(&root, &asset).is_err());

    let status = generate_proxy(&root, &asset).unwrap();
    assert_eq!(status.state, ProxyState::Ready);
    assert_eq!((status.width, status.height), (Some(960), Some(540)));
    assert!(status.size_bytes.is_some_and(|s| s > 0));
    let proxy_path = proxy_path_for(&root, asset.id);
    assert!(proxy_path.is_file());
    assert_ne!(proxy_path, path);
    // The original is byte-identical after proxy generation.
    assert_eq!(fs::read(&path).unwrap(), original);

    // A second call reuses the ready proxy instead of re-encoding.
    assert_eq!(generate_proxy(&root, &asset).unwrap(), status);
    assert_eq!(proxy_source(&root, &asset).unwrap(), proxy_path);

    let deleted = delete_proxy(&root, &asset).unwrap();
    assert_eq!(deleted.state, ProxyState::Missing);
    assert!(!proxy_path.exists());
    assert_eq!(proxy_status(&root, &asset).state, ProxyState::Missing);
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn proxy_is_refused_for_audio_only_assets() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cache");
    let path = directory.path().join("sound.wav");
    audio_fixture(&path);
    let asset = import_media(&root, &path).unwrap();
    assert_eq!(asset.kind, "audio");
    assert!(generate_proxy(&root, &asset).is_err());
    assert_eq!(proxy_status(&root, &asset).state, ProxyState::Missing);
}

#[test]
fn small_sources_are_not_upscaled_into_proxies() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cache");
    let path = directory.path().join("tiny.mp4");
    video_fixture(&path, "160x90");
    let asset = import_media(&root, &path).unwrap();
    let status = generate_proxy(&root, &asset).unwrap();
    assert_eq!(status.state, ProxyState::Ready);
    assert_eq!((status.width, status.height), (Some(160), Some(90)));
}
