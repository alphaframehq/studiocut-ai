//! Low-resolution proxy workflow: 540p H.264 proxies for project media,
//! stored in the per-asset media cache next to the thumbnail and waveform
//! peaks. Originals are never modified, moved or overwritten.
//!
//! Export always uses the original media. Proxy use in preview and any
//! proxy-for-export choice is a separate wiring step; see
//! [`ProxyStatus`] and the `proxy_playback_source` command notes in
//! `main.rs`.

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
/// Proxy video encoder settings: fast, small, good enough for editing.
pub const PROXY_CRF: u32 = 23;
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
            error: None,
        };
    }
    ProxyStatus {
        state: record.state,
        proxy_path: None,
        width: None,
        height: None,
        size_bytes: None,
        error: record.error,
    }
}

/// Generate (or regenerate) the 540p proxy for a video asset. The
/// proxy is encoded to a staging file and moved into place only on
/// success; a failed run leaves the previous proxy untouched and
/// records the failure. The original file is only read, never written.
pub fn generate_proxy(root: &Path, asset: &Asset) -> Result<ProxyStatus, String> {
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
    write_record(
        root,
        asset.id,
        &ProxyRecord {
            state: ProxyState::Generating,
            width: None,
            height: None,
            size_bytes: None,
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
    owned.push("libx264".into());
    owned.push("-preset".into());
    owned.push("veryfast".into());
    owned.push("-crf".into());
    owned.push(PROXY_CRF.to_string());
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
            source_size_bytes: 999,
            source_modified_ns: "123".into(),
            error: None,
            updated_at_ms: 1,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(serde_json::from_str::<ProxyRecord>(&json).unwrap(), record);
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
