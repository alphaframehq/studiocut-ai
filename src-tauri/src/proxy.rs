//! Low-resolution proxy workflow: 540p H.264 proxies for project media,
//! stored in the per-asset media cache next to the thumbnail and waveform
//! peaks. Originals are never modified, moved or overwritten.
//!
//! Export uses the original media by default. An explicit, opt-in export
//! choice may render from a ready proxy instead (see [`export_source`]);
//! the choice defaults off, and a clip without a ready proxy falls back
//! to its original, so an export never silently uses low-resolution
//! media. Proxy use in preview is a separate step; see [`ProxyStatus`]
//! and the `proxy_playback_source` command notes in `main.rs`.

use crate::media::{self, Asset};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// Proxy height, in pixels. Width follows the source aspect ratio.
pub const PROXY_HEIGHT: u32 = 540;
/// File name of the proxy inside the per-asset cache directory.
pub const PROXY_FILE_NAME: &str = "proxy-540p.mp4";
/// File name of the proxy state sidecar inside the per-asset cache
/// directory.
pub const PROXY_STATE_FILE_NAME: &str = "proxy.json";

/// Lifecycle state of one asset's proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyState {
    /// No proxy has been generated, or the recorded proxy is stale.
    Missing,
    /// A proxy file exists and matches the recorded source fingerprint.
    Ready,
    /// Generation is in progress (recorded before FFmpeg starts).
    Generating,
    /// The last generation attempt failed; see [`ProxyStatus::error`].
    Failed,
}

/// Proxy state for one asset, as reported to the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    pub state: ProxyState,
    /// Absolute proxy path when [`ProxyState::Ready`], otherwise `None`.
    pub proxy_path: Option<String>,
    /// Proxy width in pixels, when ready.
    pub width: Option<u32>,
    /// Proxy height in pixels, when ready.
    pub height: Option<u32>,
    /// Proxy file size in bytes, when ready.
    pub size_bytes: Option<u64>,
    /// Encoder that produced the proxy file, when recorded.
    pub encoder: Option<String>,
    /// Failure detail when [`ProxyState::Failed`], otherwise `None`.
    pub error: Option<String>,
}

impl ProxyStatus {
    pub fn missing() -> Self {
        ProxyStatus {
            state: ProxyState::Missing,
            proxy_path: None,
            width: None,
            height: None,
            size_bytes: None,
            encoder: None,
            error: None,
        }
    }
}

/// Persisted proxy record. The source fingerprint makes a proxy stale
/// the moment the original file changes, so a changed original can
/// never silently serve an old proxy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRecord {
    pub state: ProxyState,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size_bytes: Option<u64>,
    /// Encoder that produced the proxy file, when recorded. Optional so
    /// records written before encoder tracking still parse.
    #[serde(default)]
    pub encoder: Option<String>,
    pub source_size_bytes: u64,
    pub source_modified_ns: String,
    pub error: Option<String>,
    pub updated_at_ms: u64,
}

/// Cache directory for one asset inside the media root.
pub fn asset_cache_dir(root: &Path, asset_id: Uuid) -> PathBuf {
    root.join(asset_id.to_string())
}

/// Absolute path of the proxy file for an asset. Pure path logic.
pub fn proxy_path_for(root: &Path, asset_id: Uuid) -> PathBuf {
    asset_cache_dir(root, asset_id).join(PROXY_FILE_NAME)
}

/// Absolute path of the proxy state sidecar for an asset. Pure path
/// logic.
pub fn proxy_sidecar_path(root: &Path, asset_id: Uuid) -> PathBuf {
    asset_cache_dir(root, asset_id).join(PROXY_STATE_FILE_NAME)
}

/// FFmpeg scale filter for a proxy, or `None` when the source is
/// already at or below proxy height (no upscaling). Pure logic.
pub fn proxy_scale_filter(width: u32, height: u32) -> Option<String> {
    if width == 0 || height == 0 || height <= PROXY_HEIGHT {
        None
    } else {
        Some(format!("scale=-2:{PROXY_HEIGHT}"))
    }
}

/// Expected proxy dimensions for a source. Width keeps the source
/// aspect ratio and is rounded to an even number for H.264.
pub fn proxy_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    if height <= PROXY_HEIGHT {
        return (width, height);
    }
    let scale = PROXY_HEIGHT as f64 / height as f64;
    let scaled_width = ((width as f64 * scale).round() as u32).max(2);
    (scaled_width - scaled_width % 2, PROXY_HEIGHT)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn source_fingerprint(asset: &Asset) -> Result<(u64, String), String> {
    let path = Path::new(&asset.source_path);
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("Select a nonempty local media file".into());
    }
    Ok((
        metadata.len(),
        metadata
            .modified()
            .map_err(|e| e.to_string())?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
            .to_string(),
    ))
}

fn write_record(root: &Path, asset_id: Uuid, record: &ProxyRecord) -> Result<(), String> {
    let cache = asset_cache_dir(root, asset_id);
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(&cache).map_err(|e| e.to_string())?;
    use std::io::Write;
    temporary.write_all(&bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist(proxy_sidecar_path(root, asset_id))
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn read_record(root: &Path, asset_id: Uuid) -> Option<ProxyRecord> {
    let bytes = fs::read(proxy_sidecar_path(root, asset_id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Read the stored proxy status for an asset. A record whose source
/// fingerprint no longer matches the original, or whose proxy file is
/// gone, reports [`ProxyState::Missing`]. Never touches the original.
pub fn proxy_status(root: &Path, asset: &Asset) -> ProxyStatus {
    let Some(record) = read_record(root, asset.id) else {
        return ProxyStatus::missing();
    };
    let (size, modified) = match source_fingerprint(asset) {
        Ok(fingerprint) => fingerprint,
        Err(_) => return ProxyStatus::missing(),
    };
    if record.source_size_bytes != size || record.source_modified_ns != modified {
        return ProxyStatus::missing();
    }
    if record.state == ProxyState::Ready {
        let path = proxy_path_for(root, asset.id);
        let meta = fs::metadata(&path).ok();
        let size_matches = meta.as_ref().is_some_and(|m| {
            m.is_file() && record.size_bytes.is_some_and(|s| s == m.len() && s > 0)
        });
        if !size_matches {
            return ProxyStatus::missing();
        }
        return ProxyStatus {
            state: ProxyState::Ready,
            proxy_path: Some(path.to_string_lossy().into_owned()),
            width: record.width,
            height: record.height,
            size_bytes: record.size_bytes,
            encoder: record.encoder.clone(),
            error: None,
        };
    }
    ProxyStatus {
        state: record.state,
        proxy_path: None,
        width: None,
        height: None,
        size_bytes: None,
        encoder: record.encoder.clone(),
        error: record.error,
    }
}

/// Generate (or regenerate) the 540p proxy for a video asset. The
/// proxy is encoded to a staging file and moved into place only on
/// success; a failed run leaves the previous proxy untouched and
/// records the failure. The original file is only read, never written.
///
/// `preference` is the saved encoder preference string (see
/// `preferences.ts` `getEncoderPreference()`): the encoder is resolved
/// per generation with `media::resolve_preview_encoder`, hardware first
/// and libx264 as the fallback, and the quality flags come from
/// `media::proxy_encoder_quality_args`, so libx264-only flags are never
/// passed to a hardware encoder. Probing runs on every generation, as
/// in export; the resolved encoder is recorded on the proxy record.
pub fn generate_proxy(root: &Path, asset: &Asset, preference: &str) -> Result<ProxyStatus, String> {
    if asset.video_stream.is_none() {
        return Err("Proxies are only generated for video assets".into());
    }
    let existing = proxy_status(root, asset);
    if existing.state == ProxyState::Ready {
        return Ok(existing);
    }
    let source = media::verified_source(asset)?;
    let source = source.to_str().ok_or("Invalid media path")?;
    let (source_size, source_modified) = source_fingerprint(asset)?;
    let (width, height) = proxy_dimensions(asset.width.unwrap_or(0), asset.height.unwrap_or(0));
    let selection = media::resolve_preview_encoder(preference);
    write_record(
        root,
        asset.id,
        &ProxyRecord {
            state: ProxyState::Generating,
            width: None,
            height: None,
            size_bytes: None,
            encoder: Some(selection.encoder.clone()),
            source_size_bytes: source_size,
            source_modified_ns: source_modified.clone(),
            error: None,
            updated_at_ms: now_ms(),
        },
    )?;
    let cache = asset_cache_dir(root, asset.id);
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let staging = tempfile::tempdir_in(root).map_err(|e| e.to_string())?;
    let staged = staging.path().join(PROXY_FILE_NAME);
    let staged = staged.to_str().ok_or("Invalid cache path")?.to_string();
    // Owned argument strings first: references into temporaries would
    // not survive the `let` binding below.
    let mut owned: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-protocol_whitelist".into(),
        "file".into(),
        "-i".into(),
        source.into(),
        "-map".into(),
        format!("0:{}", asset.video_stream.unwrap()),
    ];
    if let Some(filter) = proxy_scale_filter(asset.width.unwrap_or(0), asset.height.unwrap_or(0)) {
        owned.push("-vf".into());
        owned.push(filter);
    }
    if let Some(stream) = asset.audio_stream {
        owned.push("-map".into());
        owned.push(format!("0:{stream}"));
        owned.push("-c:a".into());
        owned.push("aac".into());
        owned.push("-b:a".into());
        owned.push("128k".into());
    }
    owned.push("-c:v".into());
    owned.push(selection.encoder.clone());
    for flag in media::proxy_encoder_quality_args(&selection.encoder) {
        owned.push(flag.into());
    }
    owned.push("-movflags".into());
    owned.push("+faststart".into());
    owned.push("-n".into());
    owned.push(staged.clone());
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    let result = media::run("ffmpeg", &args, Duration::from_secs(600), 1_048_576);
    match result {
        Ok(_) => {
            let size = fs::metadata(&staged).map_err(|e| e.to_string())?.len();
            if size == 0 {
                let _ = write_record(
                    root,
                    asset.id,
                    &ProxyRecord {
                        state: ProxyState::Failed,
                        width: None,
                        height: None,
                        size_bytes: None,
                        encoder: Some(selection.encoder.clone()),
                        source_size_bytes: source_size,
                        source_modified_ns: source_modified,
                        error: Some("Proxy encoder produced an empty file".into()),
                        updated_at_ms: now_ms(),
                    },
                );
                return Err("Proxy encoder produced an empty file".into());
            }
            let target = proxy_path_for(root, asset.id);
            if target.exists() {
                fs::remove_file(&target).map_err(|e| e.to_string())?;
            }
            fs::rename(&staged, &target).map_err(|e| e.to_string())?;
            let record = ProxyRecord {
                state: ProxyState::Ready,
                width: Some(width),
                height: Some(height),
                size_bytes: Some(size),
                encoder: Some(selection.encoder.clone()),
                source_size_bytes: source_size,
                source_modified_ns: source_modified,
                error: None,
                updated_at_ms: now_ms(),
            };
            write_record(root, asset.id, &record)?;
            Ok(proxy_status(root, asset))
        }
        Err(error) => {
            let detail: String = error.chars().take(500).collect();
            let _ = write_record(
                root,
                asset.id,
                &ProxyRecord {
                    state: ProxyState::Failed,
                    width: None,
                    height: None,
                    size_bytes: None,
                    encoder: Some(selection.encoder.clone()),
                    source_size_bytes: source_size,
                    source_modified_ns: source_modified,
                    error: Some(detail),
                    updated_at_ms: now_ms(),
                },
            );
            Err(error)
        }
    }
}

/// Delete an asset's proxy and its state record. The original media is
/// never touched.
pub fn delete_proxy(root: &Path, asset: &Asset) -> Result<ProxyStatus, String> {
    let path = proxy_path_for(root, asset.id);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    let sidecar = proxy_sidecar_path(root, asset.id);
    if sidecar.exists() {
        fs::remove_file(&sidecar).map_err(|e| e.to_string())?;
    }
    Ok(ProxyStatus::missing())
}

/// Resolve the proxy file for preview use. Returns an error unless the
/// proxy is ready and matches the original; callers fall back to the
/// original media in that case.
pub fn proxy_source(root: &Path, asset: &Asset) -> Result<PathBuf, String> {
    let status = proxy_status(root, asset);
    if status.state != ProxyState::Ready {
        return Err("No proxy is ready for this asset".into());
    }
    status
        .proxy_path
        .map(PathBuf::from)
        .ok_or_else(|| "No proxy is ready for this asset".to_string())
}

/// Which file an export should read for one asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportSource {
    /// The original media file.
    Original(PathBuf),
    /// A ready 540p proxy file for the asset.
    Proxy(PathBuf),
}

impl ExportSource {
    /// The file the export should read.
    pub fn path(&self) -> &Path {
        match self {
            ExportSource::Original(path) | ExportSource::Proxy(path) => path,
        }
    }

    /// Whether the export reads a proxy file for this asset.
    pub fn used_proxy(&self) -> bool {
        matches!(self, ExportSource::Proxy(_))
    }
}

/// Resolve the file an export should read for one asset.
///
/// With `use_proxy` off, the original media is always used. With it on,
/// a ready proxy is used when one exists and still matches the original;
/// otherwise the export falls back to the original media for that clip.
/// A proxy is only ever served when its recorded source fingerprint
/// still matches the original, so a changed original can never silently
/// serve a stale proxy. Errors only when the original itself is missing
/// or changed (see [`media::verified_source`]); the original file is
/// only read, never written.
pub fn export_source(root: &Path, asset: &Asset, use_proxy: bool) -> Result<ExportSource, String> {
    if use_proxy {
        if let Ok(path) = proxy_source(root, asset) {
            return Ok(ExportSource::Proxy(path));
        }
    }
    Ok(ExportSource::Original(media::verified_source(asset)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_paths_live_beside_the_asset_cache() {
        let root = Path::new("/tmp/media");
        let id = Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").unwrap();
        assert_eq!(
            proxy_path_for(root, id),
            Path::new("/tmp/media/123e4567-e89b-42d3-a456-426614174000/proxy-540p.mp4")
        );
        assert_eq!(
            proxy_sidecar_path(root, id),
            Path::new("/tmp/media/123e4567-e89b-42d3-a456-426614174000/proxy.json")
        );
        assert_ne!(
            proxy_path_for(root, id)
                .extension()
                .and_then(|s| s.to_str()),
            Some("studiocut")
        );
    }

    #[test]
    fn proxy_scale_filter_skips_small_sources() {
        assert_eq!(
            proxy_scale_filter(1920, 1080),
            Some("scale=-2:540".to_string())
        );
        assert_eq!(proxy_scale_filter(960, 540), None);
        assert_eq!(proxy_scale_filter(160, 90), None);
        assert_eq!(proxy_scale_filter(0, 1080), None);
    }

    #[test]
    fn proxy_dimensions_keep_aspect_and_even_width() {
        assert_eq!(proxy_dimensions(1920, 1080), (960, 540));
        assert_eq!(proxy_dimensions(1080, 1920), (304, 540));
        assert_eq!(proxy_dimensions(960, 540), (960, 540));
        assert_eq!(proxy_dimensions(160, 90), (160, 90));
        assert_eq!(proxy_dimensions(0, 0), (0, 0));
    }

    #[test]
    fn proxy_record_round_trips_through_json() {
        let record = ProxyRecord {
            state: ProxyState::Ready,
            width: Some(960),
            height: Some(540),
            size_bytes: Some(12345),
            encoder: Some("h264_nvenc".into()),
            source_size_bytes: 999,
            source_modified_ns: "123".into(),
            error: None,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<ProxyRecord>(&json).unwrap(), record);
        // Records written before encoder tracking omit the field and
        // still parse, with no encoder recorded.
        let legacy: ProxyRecord = serde_json::from_str(
            r#"{"state":"ready","width":960,"height":540,"sizeBytes":12345,"sourceSizeBytes":999,"sourceModifiedNs":"123","error":null,"updatedAtMs":1}"#,
        )
        .unwrap();
        assert_eq!(legacy.encoder, None);
    }

    /// Asset over a real file, with the fingerprint fields filled from
    /// the file itself. Returns the asset and its path.
    fn asset_with_file(directory: &Path, name: &str) -> (Asset, PathBuf) {
        let path = directory.join(name);
        fs::write(&path, b"original media bytes").unwrap();
        let meta = fs::metadata(&path).unwrap();
        let asset = Asset {
            id: Uuid::new_v4(),
            source_path: path.to_str().unwrap().into(),
            name: name.into(),
            size_bytes: meta.len(),
            modified_ns: meta
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .to_string(),
            kind: "video".into(),
            duration: Some(1.0),
            width: Some(160),
            height: Some(90),
            fps: Some(24.0),
            video_stream: Some(0),
            audio_stream: None,
            video_codec: None,
            audio_codec: None,
            sample_rate: None,
            channels: None,
            created_at: 0,
            bin: String::new(),
        };
        (asset, path)
    }

    /// Write the ready-proxy state the generator would record for an
    /// asset: a proxy file plus a sidecar whose fingerprint matches the
    /// original. Returns the proxy path.
    fn write_ready_proxy(root: &Path, asset: &Asset, size: u64) -> PathBuf {
        let path = proxy_path_for(root, asset.id);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![0u8; size as usize]).unwrap();
        write_record(
            root,
            asset.id,
            &ProxyRecord {
                state: ProxyState::Ready,
                width: Some(96),
                height: Some(54),
                size_bytes: Some(size),
                encoder: Some("libx264".into()),
                source_size_bytes: asset.size_bytes,
                source_modified_ns: asset.modified_ns.clone(),
                error: None,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        path
    }

    #[test]
    fn export_source_off_always_uses_the_original() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cache");
        let (asset, path) = asset_with_file(directory.path(), "clip.mp4");
        let proxy = write_ready_proxy(&root, &asset, 128);
        let resolved = export_source(&root, &asset, false).unwrap();
        assert_eq!(resolved, ExportSource::Original(path));
        assert!(!resolved.used_proxy());
        assert_ne!(resolved.path(), proxy.as_path());
    }

    #[test]
    fn export_source_on_uses_a_ready_proxy() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cache");
        let (asset, _) = asset_with_file(directory.path(), "clip.mp4");
        let proxy = write_ready_proxy(&root, &asset, 128);
        let resolved = export_source(&root, &asset, true).unwrap();
        assert_eq!(resolved, ExportSource::Proxy(proxy.clone()));
        assert!(resolved.used_proxy());
        assert_eq!(resolved.path(), proxy.as_path());
        assert_eq!(
            proxy_status(&root, &asset).encoder.as_deref(),
            Some("libx264")
        );
    }

    #[test]
    fn export_source_on_falls_back_to_the_original_without_a_proxy() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cache");
        let (asset, path) = asset_with_file(directory.path(), "clip.mp4");
        assert_eq!(
            export_source(&root, &asset, true).unwrap(),
            ExportSource::Original(path)
        );
    }

    #[test]
    fn export_source_on_falls_back_when_the_proxy_file_is_gone() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cache");
        let (asset, path) = asset_with_file(directory.path(), "clip.mp4");
        let proxy = write_ready_proxy(&root, &asset, 128);
        fs::remove_file(&proxy).unwrap();
        assert_eq!(
            export_source(&root, &asset, true).unwrap(),
            ExportSource::Original(path)
        );
    }

    #[test]
    fn export_source_errors_when_the_original_changed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cache");
        let (asset, path) = asset_with_file(directory.path(), "clip.mp4");
        write_ready_proxy(&root, &asset, 128);
        // The original changed after the proxy was recorded: the proxy
        // is stale and the export must fail rather than serve it.
        fs::write(&path, b"replaced original media bytes").unwrap();
        let error = export_source(&root, &asset, true).unwrap_err();
        assert!(error.contains("changed"), "{error}");
    }

    #[test]
    fn missing_proxy_reports_missing_without_a_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let asset = Asset {
            id: Uuid::new_v4(),
            source_path: directory.path().join("gone.mp4").to_str().unwrap().into(),
            name: "gone.mp4".into(),
            size_bytes: 1,
            modified_ns: "0".into(),
            kind: "video".into(),
            duration: Some(1.0),
            width: Some(160),
            height: Some(90),
            fps: Some(24.0),
            video_stream: Some(0),
            audio_stream: None,
            video_codec: None,
            audio_codec: None,
            sample_rate: None,
            channels: None,
            created_at: 0,
            bin: String::new(),
        };
        assert_eq!(
            proxy_status(directory.path(), &asset),
            ProxyStatus::missing()
        );
    }
}
