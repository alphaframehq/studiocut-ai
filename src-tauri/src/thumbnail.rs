//! Single-frame thumbnail grabs: one PNG frame from a video clip,
//! returned as a base64 data URL for the UI to show and download.
//! Pure frame grab — no grading, captions or overlays are applied.

use crate::{media, Project};
use base64::Engine;
use std::{io::Read, time::Duration};
use uuid::Uuid;

const MAX_THUMBNAIL_BYTES: u64 = 500_000;

/// Grab one PNG frame at `at_seconds` (seconds into the selected clip)
/// and return it as a `data:image/png;base64,...` URL.
pub fn extract_thumbnail(
    project: &Project,
    clip_id: Uuid,
    at_seconds: f64,
) -> Result<String, String> {
    project.validate()?;
    let clip = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.id == clip_id)
        .ok_or("Select a video clip first")?;
    let asset = project
        .assets
        .iter()
        .find(|a| a.id == clip.asset_id)
        .ok_or("Asset not found")?;
    if asset.video_stream.is_none() {
        return Err("Selected clip has no video".to_string());
    }
    let duration = clip.duration as f64 / project.settings.fps as f64;
    if !at_seconds.is_finite() || at_seconds < 0.0 || at_seconds > duration {
        return Err("Thumbnail time must be inside the selected clip".to_string());
    }
    let source_path = media::verified_source(asset)?;
    let source_time = clip.source_in as f64 / project.settings.fps as f64 + at_seconds;
    let file = media::run(
        "ffmpeg",
        &[
            "-nostdin",
            "-ss",
            &source_time.to_string(),
            "-i",
            source_path.to_str().ok_or("Invalid media path")?,
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "-",
        ],
        Duration::from_secs(30),
        MAX_THUMBNAIL_BYTES,
    )?;
    let mut bytes = Vec::new();
    file.take(MAX_THUMBNAIL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_THUMBNAIL_BYTES {
        return Err("Thumbnail exceeds the 500 KB limit".to_string());
    }
    if bytes.is_empty() {
        return Err("No video frame at that time".to_string());
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}
