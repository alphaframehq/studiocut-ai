//! Colour scopes for the frame at the playhead: waveform, histogram and
//! vectorscope, computed in Rust from a single frame extracted by FFmpeg.
//!
//! The frame is sampled at no more than 320 px wide before analysis, so
//! every scope is an approximation of the full-resolution frame. The
//! returned [`ScopeData::approximate`] flag is always `true` and the UI
//! must show the approximation notice; these scopes are a grading aid,
//! not a broadcast measurement.

use crate::{media, Project};
use serde::Serialize;
use std::{io::Read, time::Duration};
use uuid::Uuid;

/// Long edge of the sample frame, in pixels. Analysis runs on this
/// downscaled frame, never on the full-resolution source.
pub const SCOPE_SAMPLE_WIDTH: u32 = 320;
/// Waveform columns rendered by the UI.
pub const WAVEFORM_COLUMNS: usize = 128;
/// Vectorscope grid resolution (cells per axis).
pub const VECTORSCOPE_CELLS: usize = 64;
/// Histogram bins per colour channel.
pub const HISTOGRAM_CHANNEL_BINS: usize = 64;

/// Scope data for one frame. All arrays are plain counts so the UI can
/// render them on canvas without further processing.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeData {
    /// Width of the sampled frame the scopes were computed from.
    pub sample_width: u32,
    /// Height of the sampled frame the scopes were computed from.
    pub sample_height: u32,
    /// Always `true`: scopes are computed from a downscaled sample frame.
    pub approximate: bool,
    /// Per-column luma range: `[min, max]` luma (0-255) for each of
    /// [`WAVEFORM_COLUMNS`] columns, left to right.
    pub waveform: Vec<[u8; 2]>,
    /// Luma histogram: 256 bins, one count per luma value.
    pub histogram_luma: Vec<u32>,
    /// Red histogram: [`HISTOGRAM_CHANNEL_BINS`] bins over 0-255.
    pub histogram_red: Vec<u32>,
    /// Green histogram: [`HISTOGRAM_CHANNEL_BINS`] bins over 0-255.
    pub histogram_green: Vec<u32>,
    /// Blue histogram: [`HISTOGRAM_CHANNEL_BINS`] bins over 0-255.
    pub histogram_blue: Vec<u32>,
    /// Vectorscope density grid, row-major over Cb (horizontal) and Cr
    /// (vertical), [`VECTORSCOPE_CELLS`] x [`VECTORSCOPE_CELLS`] cells.
    pub vectorscope: Vec<u32>,
    /// Highest single-cell count in [`ScopeData::vectorscope`], for
    /// normalising the display.
    pub vectorscope_peak: u32,
}

/// Sample-frame dimensions for a source frame, capped so the long edge
/// is at most [`SCOPE_SAMPLE_WIDTH`] px. Small sources keep their native
/// size (they are never upscaled for analysis).
pub fn sample_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let long = width.max(height);
    if long <= SCOPE_SAMPLE_WIDTH {
        return (width, height);
    }
    let scale = SCOPE_SAMPLE_WIDTH as f64 / long as f64;
    (
        ((width as f64 * scale).round() as u32).max(1),
        ((height as f64 * scale).round() as u32).max(1),
    )
}

/// Convert one RGB pixel to YCbCr (BT.601, studio swing). Returns
/// `(luma, cb, cr)` with luma in 0-255 and chroma centred on 128.
pub fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (r, g, b) = (r as f64, g as f64, b as f64);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = 128.0 - 0.168736 * r - 0.331264 * g + 0.5 * b;
    let cr = 128.0 + 0.5 * r - 0.418688 * g - 0.081312 * b;
    (
        y.round().clamp(0.0, 255.0) as u8,
        cb.round().clamp(0.0, 255.0) as u8,
        cr.round().clamp(0.0, 255.0) as u8,
    )
}

/// Luma histogram (256 bins) for RGB24 pixel data of `width` x `height`.
pub fn luma_histogram(pixels: &[u8], width: u32, height: u32) -> Vec<u32> {
    let mut histogram = [0u32; 256];
    let expected = width as usize * height as usize * 3;
    if pixels.len() < expected {
        return histogram.to_vec();
    }
    for chunk in pixels[..expected].chunks_exact(3) {
        let (y, _, _) = rgb_to_ycbcr(chunk[0], chunk[1], chunk[2]);
        histogram[y as usize] += 1;
    }
    histogram.to_vec()
}

/// Per-channel histograms with [`HISTOGRAM_CHANNEL_BINS`] bins each.
pub fn channel_histograms(
    pixels: &[u8],
    width: u32,
    height: u32,
) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let mut red = [0u32; HISTOGRAM_CHANNEL_BINS];
    let mut green = [0u32; HISTOGRAM_CHANNEL_BINS];
    let mut blue = [0u32; HISTOGRAM_CHANNEL_BINS];
    let expected = width as usize * height as usize * 3;
    if pixels.len() < expected {
        return (red.to_vec(), green.to_vec(), blue.to_vec());
    }
    for chunk in pixels[..expected].chunks_exact(3) {
        red[(chunk[0] as usize * HISTOGRAM_CHANNEL_BINS) / 256] += 1;
        green[(chunk[1] as usize * HISTOGRAM_CHANNEL_BINS) / 256] += 1;
        blue[(chunk[2] as usize * HISTOGRAM_CHANNEL_BINS) / 256] += 1;
    }
    (red.to_vec(), green.to_vec(), blue.to_vec())
}

/// Waveform: per-column luma `[min, max]` for [`WAVEFORM_COLUMNS`]
/// columns across the frame. Columns with no sampled pixels report
/// `[0, 0]`.
pub fn waveform_columns(pixels: &[u8], width: u32, height: u32) -> Vec<[u8; 2]> {
    let mut columns = vec![[u8::MAX, u8::MIN]; WAVEFORM_COLUMNS];
    let mut present = vec![false; WAVEFORM_COLUMNS];
    let expected = width as usize * height as usize * 3;
    if width == 0 || height == 0 || pixels.len() < expected {
        return vec![[0, 0]; WAVEFORM_COLUMNS];
    }
    for y in 0..height as usize {
        for x in 0..width as usize {
            let offset = (y * width as usize + x) * 3;
            let (luma, _, _) = rgb_to_ycbcr(pixels[offset], pixels[offset + 1], pixels[offset + 2]);
            let column = if width as usize <= 1 {
                0
            } else {
                (x * (WAVEFORM_COLUMNS - 1)) / (width as usize - 1)
            };
            columns[column][0] = columns[column][0].min(luma);
            columns[column][1] = columns[column][1].max(luma);
            present[column] = true;
        }
    }
    columns
        .into_iter()
        .zip(present)
        .map(|(range, seen)| if seen { range } else { [0, 0] })
        .collect()
}

/// Vectorscope density grid: each pixel contributes one count to the
/// cell holding its (Cb, Cr) chroma. Returns the flat row-major grid
/// and the highest single-cell count.
pub fn vectorscope_grid(pixels: &[u8], width: u32, height: u32) -> (Vec<u32>, u32) {
    let mut grid = vec![0u32; VECTORSCOPE_CELLS * VECTORSCOPE_CELLS];
    let mut peak = 0u32;
    let expected = width as usize * height as usize * 3;
    if pixels.len() < expected {
        return (grid, peak);
    }
    for chunk in pixels[..expected].chunks_exact(3) {
        let (_, cb, cr) = rgb_to_ycbcr(chunk[0], chunk[1], chunk[2]);
        let cell_x = (cb as usize * VECTORSCOPE_CELLS) / 256;
        let cell_y = (cr as usize * VECTORSCOPE_CELLS) / 256;
        let index = cell_y * VECTORSCOPE_CELLS + cell_x;
        grid[index] += 1;
        peak = peak.max(grid[index]);
    }
    (grid, peak)
}

/// Shape raw RGB24 pixels of `width` x `height` into [`ScopeData`].
/// Pure shaping: no I/O, no FFmpeg.
pub fn shape_scopes(pixels: &[u8], width: u32, height: u32) -> ScopeData {
    let (red, green, blue) = channel_histograms(pixels, width, height);
    let (vectorscope, vectorscope_peak) = vectorscope_grid(pixels, width, height);
    ScopeData {
        sample_width: width,
        sample_height: height,
        approximate: true,
        waveform: waveform_columns(pixels, width, height),
        histogram_luma: luma_histogram(pixels, width, height),
        histogram_red: red,
        histogram_green: green,
        histogram_blue: blue,
        vectorscope,
        vectorscope_peak,
    }
}

/// Analyse the frame of `clip_id` at `at_seconds` (seconds into the
/// clip) and return its scopes. The frame is extracted by FFmpeg at no
/// more than [`SCOPE_SAMPLE_WIDTH`] px on the long edge, so the result
/// is always approximate (see [`ScopeData::approximate`]).
pub fn analyze_scopes(
    project: &Project,
    clip_id: Uuid,
    at_seconds: f64,
) -> Result<ScopeData, String> {
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
        return Err("Selected clip has no video".into());
    }
    let duration = clip.duration as f64 / project.settings.fps as f64;
    if !at_seconds.is_finite() || at_seconds < 0.0 || at_seconds > duration {
        return Err("Scope time must be inside the selected clip".into());
    }
    let (width, height) = (asset.width.unwrap_or(0), asset.height.unwrap_or(0));
    let (sample_width, sample_height) = sample_dimensions(width, height);
    if sample_width == 0 || sample_height == 0 {
        return Err("Source video has no dimensions".into());
    }
    let source_path = media::verified_source(asset)?;
    let source_time = clip.source_in as f64 / project.settings.fps as f64 + at_seconds;
    let max_bytes = sample_width as u64 * sample_height as u64 * 3;
    let file = media::run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &source_time.to_string(),
            "-i",
            source_path.to_str().ok_or("Invalid media path")?,
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={sample_width}:{sample_height}:force_original_aspect_ratio=decrease"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ],
        Duration::from_secs(30),
        max_bytes,
    )?;
    let mut pixels = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut pixels)
        .map_err(|e| e.to_string())?;
    if pixels.len() as u64 != max_bytes {
        return Err("Frame extraction returned incomplete pixel data".into());
    }
    Ok(shape_scopes(&pixels, sample_width, sample_height))
}

/// Pure audio-mixer gain maths, shared with the `AudioMixer` UI (which
/// mirrors these functions in TypeScript). Gain is stored per track and
/// is preview-only in this phase: it is not applied to preview playback
/// or to export.
pub mod audio_gain {
    /// Gain range offered by the mixer sliders, in dB.
    pub const MIN_GAIN_DB: f64 = -60.0;
    /// Gain range offered by the mixer sliders, in dB.
    pub const MAX_GAIN_DB: f64 = 12.0;

    /// Clamp a gain value to the mixer slider range. Non-finite input
    /// becomes 0 dB.
    pub fn clamp_gain_db(gain_db: f64) -> f64 {
        if !gain_db.is_finite() {
            return 0.0;
        }
        gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
    }

    /// Convert a gain in dB to a linear multiplier. The conversion is
    /// unclamped; slider-range clamping is the caller's job and lives
    /// in [`clamp_gain_db`].
    pub fn gain_db_to_linear(gain_db: f64) -> f64 {
        10f64.powf(gain_db / 20.0)
    }

    /// Estimated integrated loudness after applying `gain_db` to a clip
    /// measured at `measured_lufs`. Display only; the source audio is
    /// never modified.
    pub fn effective_loudness_lufs(measured_lufs: f64, gain_db: f64) -> f64 {
        measured_lufs + clamp_gain_db(gain_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_pixels(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[r, g, b]);
        }
        pixels
    }

    #[test]
    fn sample_dimensions_caps_the_long_edge() {
        assert_eq!(sample_dimensions(1920, 1080), (320, 180));
        assert_eq!(sample_dimensions(1080, 1920), (180, 320));
        assert_eq!(sample_dimensions(160, 90), (160, 90));
        assert_eq!(sample_dimensions(0, 90), (0, 0));
    }

    #[test]
    fn rgb_to_ycbcr_known_values() {
        assert_eq!(rgb_to_ycbcr(255, 255, 255), (255, 128, 128));
        assert_eq!(rgb_to_ycbcr(0, 0, 0), (0, 128, 128));
        let (y, _, _) = rgb_to_ycbcr(255, 0, 0);
        assert_eq!(y, 76);
    }

    #[test]
    fn histograms_count_every_pixel() {
        let pixels = solid_pixels(4, 4, 255, 0, 0);
        let luma = luma_histogram(&pixels, 4, 4);
        assert_eq!(luma.iter().sum::<u32>(), 16);
        assert_eq!(luma[76], 16);
        let (red, green, blue) = channel_histograms(&pixels, 4, 4);
        assert_eq!(red.iter().sum::<u32>(), 16);
        assert_eq!(red[63], 16);
        assert_eq!(green[0], 16);
        assert_eq!(blue[0], 16);
    }

    #[test]
    fn waveform_columns_span_the_frame() {
        // Left half black, right half white: first columns report 0,
        // last columns report 255.
        let mut pixels = Vec::new();
        for _ in 0..4 {
            for x in 0..8 {
                let v = if x < 4 { 0 } else { 255 };
                pixels.extend_from_slice(&[v, v, v]);
            }
        }
        let columns = waveform_columns(&pixels, 8, 4);
        assert_eq!(columns.len(), WAVEFORM_COLUMNS);
        assert_eq!(columns[0], [0, 0]);
        assert_eq!(columns[WAVEFORM_COLUMNS - 1], [255, 255]);
        assert!(columns.iter().all(|[min, max]| min <= max));
    }

    #[test]
    fn vectorscope_grid_counts_and_peaks() {
        let pixels = solid_pixels(4, 4, 255, 255, 255);
        let (grid, peak) = vectorscope_grid(&pixels, 4, 4);
        assert_eq!(grid.len(), VECTORSCOPE_CELLS * VECTORSCOPE_CELLS);
        assert_eq!(grid.iter().sum::<u32>(), 16);
        assert_eq!(peak, 16);
    }

    #[test]
    fn shape_scopes_is_always_approximate() {
        let pixels = solid_pixels(2, 2, 10, 20, 30);
        let scopes = shape_scopes(&pixels, 2, 2);
        assert!(scopes.approximate);
        assert_eq!((scopes.sample_width, scopes.sample_height), (2, 2));
        assert_eq!(scopes.histogram_luma.iter().sum::<u32>(), 4);
        assert_eq!(scopes.vectorscope_peak, 4);
    }

    #[test]
    fn short_pixel_buffers_shape_to_empty_scopes() {
        let scopes = shape_scopes(&[1, 2, 3], 4, 4);
        assert_eq!(scopes.histogram_luma.iter().sum::<u32>(), 0);
        assert_eq!(scopes.vectorscope_peak, 0);
        assert!(scopes.waveform.iter().all(|c| *c == [0, 0]));
    }

    #[test]
    fn gain_maths_clamp_convert_and_apply() {
        assert_eq!(audio_gain::clamp_gain_db(f64::NAN), 0.0);
        assert_eq!(audio_gain::clamp_gain_db(99.0), 12.0);
        assert_eq!(audio_gain::clamp_gain_db(-99.0), -60.0);
        assert!((audio_gain::gain_db_to_linear(0.0) - 1.0).abs() < 1e-9);
        assert!((audio_gain::gain_db_to_linear(20.0) - 10.0).abs() < 1e-9);
        assert!((audio_gain::gain_db_to_linear(-20.0) - 0.1).abs() < 1e-9);
        assert_eq!(audio_gain::effective_loudness_lufs(-23.0, 9.0), -14.0);
        assert_eq!(audio_gain::effective_loudness_lufs(-23.0, 99.0), -11.0);
    }
}
