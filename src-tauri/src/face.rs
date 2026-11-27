//! Face detection with BlazeFace full-range under `tract` (pure Rust,
//! CPU-only).
//!
//! The bundled model is the official MediaPipe BlazeFace full-range
//! `.tflite` converted in-house to ONNX with pinned `tf2onnx` 1.16.1
//! (see `src-tauri/assets/face-detection/README.md` for the source URL,
//! SHA-256 values, input/output specification, training-data provenance
//! and numerical-parity proof). It is Apache-2.0 licensed; BlazeFace
//! was trained on consented images, not on WIDER FACE.
//!
//! Detection pipeline, following the official MediaPipe graph
//! (`face_detection_full_range`, `num_layers: 1`, `strides: 4`,
//! `num_boxes: 2304`) and the `TensorsToDetectionsCalculator` decode
//! (default YXHW format, `x_scale = y_scale = w_scale = h_scale =
//! 192`, linear box size):
//!
//! 1. The source frame is letterboxed to 192x192 (aspect preserved,
//!    padded with mid-grey, which is 0.0 after `[-1, 1]`
//!    normalisation) and normalised to `[-1, 1]` as NHWC float32 —
//!    the layout the converted model expects.
//! 2. The model returns per-anchor regressors `[1, 2304, 16]` (box +
//!    6 keypoints) and score logits `[1, 2304, 1]`.
//! 3. Each anchor is one fixed-size box per cell of a 48x48 grid
//!    (stride 4, centre `(gx + 0.5) / 48`, `(gy + 0.5) / 48`, width =
//!    height = 1.0). The regressors adjust the anchor linearly; scores
//!    pass through a sigmoid.
//! 4. Detections below [`FACE_SCORE_THRESHOLD`] are dropped and the
//!    rest go through non-maximum suppression at IoU
//!    [`FACE_NMS_IOU_THRESHOLD`].
//!
//! Tracking ([`select_face_track`]) works on one largest-face
//! detection per sampled frame: detections are grouped into tracks by
//! centre proximity, the track seen in the most samples wins (ties go
//! to the larger mean face area), and its centres are smoothed with an
//! exponential moving average so the reframe crop does not jitter.
//! A face seen in only one sample never forms a track, unless the clip
//! yielded a single sample at all.

use std::sync::{Arc, OnceLock};
use tract::prelude::*;

/// Bundled BlazeFace full-range model (ONNX, converted in-house from
/// the official `.tflite`; see `assets/face-detection/README.md`).
const MODEL_BYTES: &[u8] = include_bytes!("../assets/face-detection/blaze_face_full_range.onnx");

/// Model input edge, in pixels.
pub const FACE_INPUT_SIZE: u32 = 192;
/// Anchors in the BlazeFace full-range grid (48 x 48).
pub const FACE_NUM_ANCHORS: usize = 2304;
/// Grid edge of the anchor layout.
pub const FACE_GRID: usize = 48;
/// Coordinate scale used by the MediaPipe decode (the model input
/// edge, in pixels).
pub const FACE_SCALE: f32 = 192.0;
/// Minimum detection score (after sigmoid) for a face to be reported.
/// The official full-range graph uses 0.6; 0.5 is used here because
/// tracking requires the face to appear consistently across samples,
/// which already guards against isolated false positives, and the
/// lower threshold keeps distant faces (the full-range model's purpose)
/// in the running.
pub const FACE_SCORE_THRESHOLD: f64 = 0.5;
/// Non-maximum-suppression IoU threshold (the MediaPipe suppression
/// threshold).
pub const FACE_NMS_IOU_THRESHOLD: f64 = 0.3;
/// Largest centre distance (in normalised frame coordinates) between
/// two sampled faces for them to count as the same track.
pub const FACE_TRACK_MAX_CENTRE_DISTANCE: f64 = 0.25;
/// Exponential-smoothing factor applied to track centres (0.5 keeps
/// half of the previous smoothed centre and half of the new sample).
pub const FACE_TRACK_SMOOTHING_ALPHA: f64 = 0.5;

/// One detected face, in normalised source-frame coordinates (0–1).
/// `x_min` / `y_min` / `x_max` / `y_max` describe the face bounding
/// box; `score` is the detection confidence after sigmoid (0–1).
/// Keypoints are decoded by the model but not needed for reframing,
/// so they are not returned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceBox {
    pub x_min: f64,
    pub y_min: f64,
    pub x_max: f64,
    pub y_max: f64,
    pub score: f64,
}

impl FaceBox {
    /// Centre of the box, in normalised frame coordinates.
    pub fn centre(&self) -> (f64, f64) {
        (
            (self.x_min + self.x_max) / 2.0,
            (self.y_min + self.y_max) / 2.0,
        )
    }

    /// Box area in normalised frame units.
    pub fn area(&self) -> f64 {
        ((self.x_max - self.x_min).max(0.0)) * ((self.y_max - self.y_min).max(0.0))
    }
}

/// One sampled frame's detections, at `t_seconds` into the clip.
#[derive(Clone, Debug)]
pub struct FaceSample {
    pub t_seconds: f64,
    pub faces: Vec<FaceBox>,
}

/// One smoothed point of the selected face track: the crop focus at
/// `t_seconds` into the clip, in normalised frame coordinates, with
/// the mean detection score of the track at that sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceTrackPoint {
    pub t_seconds: f64,
    pub focus_x: f64,
    pub focus_y: f64,
    pub confidence: f64,
}

/// The loaded model, shared across calls. Loading happens once, lazily,
/// on first use; a load failure is cached as an error string so the
/// capability report can state the reason instead of claiming
/// availability it does not have.
static MODEL: OnceLock<Result<Arc<Runnable>, String>> = OnceLock::new();

fn load_model() -> Result<&'static Arc<Runnable>, &'static String> {
    MODEL
        .get_or_init(|| {
            (|| -> Result<Arc<Runnable>, String> {
                let runnable = tract::onnx()
                    .map_err(|e| e.to_string())?
                    .load_buffer(MODEL_BYTES)
                    .map_err(|e| e.to_string())?
                    .into_model()
                    .map_err(|e| e.to_string())?
                    .into_runnable()
                    .map_err(|e| e.to_string())?;
                Ok(Arc::new(runnable))
            })()
        })
        .as_ref()
        .map_err(|e| e)
}

/// Whether the bundled face-detection model loads and is ready to run.
pub fn model_available() -> bool {
    load_model().is_ok()
}

/// The model-load failure reason, if loading failed. `None` when the
/// model loads (or has not been probed yet — probing happens on the
/// first call to [`model_available`] or [`detect_faces`]).
pub fn model_load_error() -> Option<String> {
    MODEL
        .get()
        .and_then(|result| result.as_ref().err().cloned())
}

/// Detect faces in one RGB24 frame (`width` x `height`, row-major,
/// 3 bytes per pixel). Returns boxes in normalised source-frame
/// coordinates, best score first. An empty vector means no face was
/// detected — never a hallucinated box. Returns an error when the
/// inputs are not one complete frame or the model fails to run.
pub fn detect_faces(rgb24: &[u8], width: u32, height: u32) -> Result<Vec<FaceBox>, String> {
    let expected = width as usize * height as usize * 3;
    if width == 0 || height == 0 || rgb24.len() < expected {
        return Err("Face detection needs one complete RGB24 frame".into());
    }
    let model = load_model().map_err(|e| e.clone())?;
    let input = preprocess(rgb24, width, height);
    let tensor = Tensor::from_slice(
        &[1, FACE_INPUT_SIZE as usize, FACE_INPUT_SIZE as usize, 3],
        &input,
    )
    .map_err(|e| e.to_string())?;
    let outputs = model.run(vec![tensor]).map_err(|e| e.to_string())?;
    if outputs.len() != 2 {
        return Err(format!(
            "Face model returned {} outputs, expected 2",
            outputs.len()
        ));
    }
    let regressors = outputs[0].as_slice::<f32>().map_err(|e| e.to_string())?;
    let scores = outputs[1].as_slice::<f32>().map_err(|e| e.to_string())?;
    if regressors.len() != FACE_NUM_ANCHORS * 16 || scores.len() != FACE_NUM_ANCHORS {
        return Err(format!(
            "Face model returned unexpected output sizes ({} regressors, {} scores)",
            regressors.len(),
            scores.len()
        ));
    }
    let letterboxed = decode(regressors, scores);
    let params = letterbox_params(width, height);
    Ok(letterboxed
        .into_iter()
        .map(|face| map_to_source(face, &params))
        .filter(|face| face.x_max > face.x_min && face.y_max > face.y_min)
        .collect())
}

/// Letterbox geometry for mapping between a source frame and the
/// 192x192 model input: the content occupies `content` pixels starting
/// at `offset` along each axis of the square input.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LetterboxParams {
    offset_x: u32,
    offset_y: u32,
    content_width: u32,
    content_height: u32,
}

fn letterbox_params(width: u32, height: u32) -> LetterboxParams {
    let scale = FACE_INPUT_SIZE as f64 / width.max(height) as f64;
    let content_width = ((width as f64 * scale).round() as u32).max(1);
    let content_height = ((height as f64 * scale).round() as u32).max(1);
    LetterboxParams {
        offset_x: (FACE_INPUT_SIZE - content_width) / 2,
        offset_y: (FACE_INPUT_SIZE - content_height) / 2,
        content_width,
        content_height,
    }
}

/// Resize one RGB24 frame to the 192x192 model input with
/// aspect-preserving letterboxing (bilinear sampling, mid-grey padding)
/// and normalise each channel to `[-1, 1]`. Output is NHWC float32 —
/// the layout the converted model expects.
fn preprocess(rgb24: &[u8], width: u32, height: u32) -> Vec<f32> {
    let params = letterbox_params(width, height);
    let mut input = vec![0.0f32; (FACE_INPUT_SIZE * FACE_INPUT_SIZE * 3) as usize];
    let sample = |x: f64, y: f64, channel: usize| -> f32 {
        // Bilinear sample of the source frame at continuous coordinates.
        let x = x.clamp(0.0, width as f64 - 1.0);
        let y = y.clamp(0.0, height as f64 - 1.0);
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(width as usize - 1);
        let y1 = (y0 + 1).min(height as usize - 1);
        let fx = x - x0 as f64;
        let fy = y - y0 as f64;
        let at = |xx: usize, yy: usize| -> f64 {
            rgb24[(yy * width as usize + xx) * 3 + channel] as f64
        };
        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        ((top * (1.0 - fy) + bottom * fy) / 127.5 - 1.0) as f32
    };
    for out_y in 0..FACE_INPUT_SIZE {
        for out_x in 0..FACE_INPUT_SIZE {
            if out_x < params.offset_x
                || out_x >= params.offset_x + params.content_width
                || out_y < params.offset_y
                || out_y >= params.offset_y + params.content_height
            {
                continue; // Padding stays 0.0 (mid-grey after normalisation).
            }
            // Centre of the output pixel, mapped back to source pixels.
            let src_x = (out_x - params.offset_x) as f64 + 0.5;
            let src_y = (out_y - params.offset_y) as f64 + 0.5;
            let scale_x = params.content_width as f64 / width as f64;
            let scale_y = params.content_height as f64 / height as f64;
            let x = src_x / scale_x - 0.5;
            let y = src_y / scale_y - 0.5;
            let base = ((out_y * FACE_INPUT_SIZE + out_x) * 3) as usize;
            for channel in 0..3 {
                input[base + channel] = sample(x, y, channel);
            }
        }
    }
    input
}

/// One anchor: centre and size in normalised input coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Anchor {
    x_center: f64,
    y_center: f64,
    width: f64,
    height: f64,
}

/// The 2304 fixed-size anchors: one per cell of the 48x48 grid
/// (stride 4 over the 192-pixel input).
fn anchors() -> Vec<Anchor> {
    let mut result = Vec::with_capacity(FACE_NUM_ANCHORS);
    for grid_y in 0..FACE_GRID {
        for grid_x in 0..FACE_GRID {
            result.push(Anchor {
                x_center: (grid_x as f64 + 0.5) / FACE_GRID as f64,
                y_center: (grid_y as f64 + 0.5) / FACE_GRID as f64,
                width: 1.0,
                height: 1.0,
            });
        }
    }
    result
}

fn sigmoid(value: f32) -> f64 {
    1.0 / (1.0 + (-value as f64).exp())
}

/// Decode raw regressors and score logits into face boxes in
/// normalised 192x192-input coordinates, after the score threshold and
/// non-maximum suppression. Follows the MediaPipe
/// `TensorsToDetectionsCalculator` decode (default YXHW format):
/// the regressor holds `[y_centre, x_centre, height, width, ...]` per
/// anchor, each divided by [`FACE_SCALE`] and multiplied by the anchor
/// size, then offset by the anchor centre; box size is linear, not
/// exponential (verified against the exponential variant on a real
/// portrait — see `assets/face-detection/README.md`).
fn decode(regressors: &[f32], scores: &[f32]) -> Vec<FaceBox> {
    let anchors = anchors();
    let mut candidates: Vec<FaceBox> = Vec::new();
    for (index, anchor) in anchors.iter().enumerate() {
        let score = sigmoid(scores[index]);
        if score < FACE_SCORE_THRESHOLD {
            continue;
        }
        let raw = &regressors[index * 16..index * 16 + 16];
        // YXHW order: raw[0] is the y-centre offset, raw[1] the
        // x-centre offset, raw[2] the height, raw[3] the width.
        let centre_x = anchor.x_center + raw[1] as f64 / FACE_SCALE as f64 * anchor.width;
        let centre_y = anchor.y_center + raw[0] as f64 / FACE_SCALE as f64 * anchor.height;
        let width = raw[3] as f64 / FACE_SCALE as f64 * anchor.width;
        let height = raw[2] as f64 / FACE_SCALE as f64 * anchor.height;
        if !(width > 0.0) || !(height > 0.0) {
            continue;
        }
        candidates.push(FaceBox {
            x_min: (centre_x - width / 2.0).clamp(0.0, 1.0),
            y_min: (centre_y - height / 2.0).clamp(0.0, 1.0),
            x_max: (centre_x + width / 2.0).clamp(0.0, 1.0),
            y_max: (centre_y + height / 2.0).clamp(0.0, 1.0),
            score,
        });
    }
    non_maximum_suppression(candidates)
}

/// Intersection-over-union of two boxes, in normalised coordinates.
fn intersection_over_union(a: &FaceBox, b: &FaceBox) -> f64 {
    let x_overlap = (a.x_max.min(b.x_max) - a.x_min.max(b.x_min)).max(0.0);
    let y_overlap = (a.y_max.min(b.y_max) - a.y_min.max(b.y_min)).max(0.0);
    let intersection = x_overlap * y_overlap;
    let union = a.area() + b.area() - intersection;
    if union <= 0.0 {
        0.0
    } else {
        intersection / union
    }
}

/// Non-maximum suppression: best score first, drop any later box that
/// overlaps a kept box by more than [`FACE_NMS_IOU_THRESHOLD`].
fn non_maximum_suppression(mut candidates: Vec<FaceBox>) -> Vec<FaceBox> {
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<FaceBox> = Vec::new();
    for candidate in candidates {
        if kept
            .iter()
            .all(|kept| intersection_over_union(&candidate, kept) <= FACE_NMS_IOU_THRESHOLD)
        {
            kept.push(candidate);
        }
    }
    kept
}

/// Map a box from normalised 192x192-input coordinates back to
/// normalised source-frame coordinates, inverting the letterbox.
fn map_to_source(face: FaceBox, params: &LetterboxParams) -> FaceBox {
    let map = |value: f64, offset: u32, content: u32| -> f64 {
        ((value * FACE_INPUT_SIZE as f64 - offset as f64) / content as f64).clamp(0.0, 1.0)
    };
    FaceBox {
        x_min: map(face.x_min, params.offset_x, params.content_width),
        y_min: map(face.y_min, params.offset_y, params.content_height),
        x_max: map(face.x_max, params.offset_x, params.content_width),
        y_max: map(face.y_max, params.offset_y, params.content_height),
        score: face.score,
    }
}

/// Select the one dominant face track across sampled frames and
/// smooth it. Returns `None` when no face appears consistently
/// enough to track: a track must span at least two samples, unless
/// the clip yielded a single sample at all, in which case that
/// sample's largest face is the track. Faces that appear and
/// disappear do not win over a face present across samples — the
/// track with the most samples wins, ties broken by larger mean face
/// area.
pub fn select_face_track(samples: &[FaceSample]) -> Option<Vec<FaceTrackPoint>> {
    if samples.is_empty() {
        return None;
    }
    // One largest face per sample; samples without a face contribute
    // nothing to any track.
    let per_sample: Vec<(f64, FaceBox)> = samples
        .iter()
        .filter_map(|sample| {
            sample
                .faces
                .iter()
                .max_by(|a, b| {
                    a.area()
                        .partial_cmp(&b.area())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|face| (sample.t_seconds, *face))
        })
        .collect();
    if per_sample.is_empty() {
        return None;
    }
    // Greedy tracks: a face joins the track whose last centre is
    // closest, when that centre is near enough; otherwise it starts
    // a new track.
    let mut tracks: Vec<Vec<(f64, FaceBox)>> = Vec::new();
    for (t_seconds, face) in per_sample {
        let (centre_x, centre_y) = face.centre();
        let mut best: Option<usize> = None;
        let mut best_distance = f64::INFINITY;
        for (index, track) in tracks.iter().enumerate() {
            let (_, last) = track.last().unwrap();
            let (last_x, last_y) = last.centre();
            let distance = ((centre_x - last_x).powi(2) + (centre_y - last_y).powi(2)).sqrt();
            if distance < best_distance {
                best_distance = distance;
                best = Some(index);
            }
        }
        match best {
            Some(index) if best_distance <= FACE_TRACK_MAX_CENTRE_DISTANCE => {
                tracks[index].push((t_seconds, face));
            }
            _ => tracks.push(vec![(t_seconds, face)]),
        }
    }
    let best = tracks.iter().max_by(|a, b| {
        let by_length = a.len().cmp(&b.len());
        if by_length != std::cmp::Ordering::Equal {
            return by_length;
        }
        let mean_area = |track: &Vec<(f64, FaceBox)>| -> f64 {
            track.iter().map(|(_, face)| face.area()).sum::<f64>() / track.len() as f64
        };
        mean_area(a)
            .partial_cmp(&mean_area(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;
    if best.len() < 2 && samples.len() > 1 {
        return None;
    }
    Some(smooth_track(best))
}

/// Exponential smoothing of a track's centres: the first point is
/// kept as sampled, each later point keeps half of the previous
/// smoothed centre and half of the new sample, removing frame-to-frame
/// jitter without lagging the track.
fn smooth_track(track: &[(f64, FaceBox)]) -> Vec<FaceTrackPoint> {
    let mut smoothed: Vec<FaceTrackPoint> = Vec::with_capacity(track.len());
    for (t_seconds, face) in track {
        let (centre_x, centre_y) = face.centre();
        let (focus_x, focus_y) = match smoothed.last() {
            Some(previous) => (
                FACE_TRACK_SMOOTHING_ALPHA * centre_x
                    + (1.0 - FACE_TRACK_SMOOTHING_ALPHA) * previous.focus_x,
                FACE_TRACK_SMOOTHING_ALPHA * centre_y
                    + (1.0 - FACE_TRACK_SMOOTHING_ALPHA) * previous.focus_y,
            ),
            None => (centre_x, centre_y),
        };
        smoothed.push(FaceTrackPoint {
            t_seconds: *t_seconds,
            focus_x,
            focus_y,
            confidence: face.score,
        });
    }
    smoothed
}

/// Focus point of a smoothed track at `t_seconds`, linearly
/// interpolated between the neighbouring keyframes and clamped to the
/// track ends. Used by the preview to follow the face at the playhead.
pub fn interpolate_focus(track: &[FaceTrackPoint], t_seconds: f64) -> Option<(f64, f64)> {
    let first = track.first()?;
    if t_seconds <= first.t_seconds {
        return Some((first.focus_x, first.focus_y));
    }
    let last = track.last().unwrap();
    if t_seconds >= last.t_seconds {
        return Some((last.focus_x, last.focus_y));
    }
    for pair in track.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t_seconds >= a.t_seconds && t_seconds <= b.t_seconds {
            let span = b.t_seconds - a.t_seconds;
            let fraction = if span > 0.0 {
                (t_seconds - a.t_seconds) / span
            } else {
                0.0
            };
            return Some((
                a.focus_x + (b.focus_x - a.focus_x) * fraction,
                a.focus_y + (b.focus_y - a.focus_y) * fraction,
            ));
        }
    }
    Some((last.focus_x, last.focus_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x_min: f64, y_min: f64, x_max: f64, y_max: f64, score: f64) -> FaceBox {
        FaceBox {
            x_min,
            y_min,
            x_max,
            y_max,
            score,
        }
    }

    #[test]
    fn anchors_cover_a_48_by_48_grid() {
        let anchors = anchors();
        assert_eq!(anchors.len(), FACE_NUM_ANCHORS);
        assert_eq!(
            anchors[0],
            Anchor {
                x_center: 0.5 / 48.0,
                y_center: 0.5 / 48.0,
                width: 1.0,
                height: 1.0,
            }
        );
        assert_eq!(
            anchors[FACE_NUM_ANCHORS - 1],
            Anchor {
                x_center: 47.5 / 48.0,
                y_center: 47.5 / 48.0,
                width: 1.0,
                height: 1.0,
            }
        );
    }

    #[test]
    fn decode_maps_regressors_to_anchor_boxes() {
        // Anchor (24, 24): centre (24.5/48, 24.5/48). A regressor of
        // [0, 0, 96, 96] (YXHW) gives a 0.5 x 0.5 box on that centre.
        let mut regressors = vec![0.0f32; FACE_NUM_ANCHORS * 16];
        let mut scores = vec![-10.0f32; FACE_NUM_ANCHORS];
        let index = 24 * 48 + 24;
        regressors[index * 16 + 2] = 96.0;
        regressors[index * 16 + 3] = 96.0;
        scores[index] = 2.0; // sigmoid(2.0) ~= 0.881
        let decoded = decode(&regressors, &scores);
        assert_eq!(decoded.len(), 1);
        let centre = 24.5 / 48.0;
        assert!((decoded[0].x_min - (centre - 0.25)).abs() < 1e-6);
        assert!((decoded[0].y_min - (centre - 0.25)).abs() < 1e-6);
        assert!((decoded[0].x_max - (centre + 0.25)).abs() < 1e-6);
        assert!((decoded[0].y_max - (centre + 0.25)).abs() < 1e-6);
        assert!((decoded[0].score - 0.880_797_08).abs() < 1e-6);
    }

    #[test]
    fn decode_drops_scores_below_threshold() {
        let regressors = vec![0.0f32; FACE_NUM_ANCHORS * 16];
        let scores = vec![-5.0f32; FACE_NUM_ANCHORS]; // sigmoid ~= 0.007
        assert!(decode(&regressors, &scores).is_empty());
    }

    #[test]
    fn nms_keeps_the_highest_score() {
        let kept = non_maximum_suppression(vec![
            face(0.2, 0.2, 0.6, 0.6, 0.9),
            face(0.25, 0.25, 0.65, 0.65, 0.7),
            face(0.8, 0.8, 0.95, 0.95, 0.6),
        ]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].score, 0.9);
        assert_eq!(kept[1].score, 0.6);
    }

    #[test]
    fn preprocess_of_a_square_frame_is_identity_scaled() {
        // A 192x192 frame maps 1:1 into the model input.
        let mut frame = vec![0u8; 192 * 192 * 3];
        frame[0] = 255; // top-left pixel, red channel
        frame[1] = 0;
        frame[2] = 0;
        let input = preprocess(&frame, 192, 192);
        assert_eq!(input.len(), 192 * 192 * 3);
        assert!((input[0] - 1.0).abs() < 1e-6); // 255 / 127.5 - 1
        assert!((input[1] - -1.0).abs() < 1e-6); // 0 / 127.5 - 1
        assert!((input[2] - -1.0).abs() < 1e-6);
    }

    #[test]
    fn letterbox_params_keep_aspect() {
        let params = letterbox_params(1920, 1080);
        assert_eq!(params.content_width, 192);
        assert_eq!(params.content_height, 108);
        assert_eq!(params.offset_x, 0);
        assert_eq!(params.offset_y, 42);
    }

    #[test]
    fn bundled_model_loads_and_blank_frame_has_no_face() {
        assert!(
            model_available(),
            "{}",
            model_load_error().unwrap_or_default()
        );
        let frame = vec![128u8; 192 * 192 * 3]; // Mid-grey: normalised ~0.
        let faces = detect_faces(&frame, 192, 192).unwrap();
        assert!(
            faces.is_empty(),
            "blank frame must not produce a face, got {faces:?}"
        );
    }

    #[test]
    fn detect_rejects_an_incomplete_frame() {
        assert!(detect_faces(&[0u8; 100], 192, 192).is_err());
        assert!(detect_faces(&[], 0, 0).is_err());
    }

    #[test]
    fn track_selects_the_consistent_face() {
        let samples = vec![
            FaceSample {
                t_seconds: 0.0,
                faces: vec![
                    face(0.1, 0.2, 0.3, 0.5, 0.9), // dominant face
                    face(0.7, 0.2, 0.8, 0.4, 0.8), // passer-by, one sample
                ],
            },
            FaceSample {
                t_seconds: 1.0,
                faces: vec![face(0.12, 0.22, 0.32, 0.52, 0.85)],
            },
            FaceSample {
                t_seconds: 2.0,
                faces: vec![face(0.14, 0.24, 0.34, 0.54, 0.88)],
            },
        ];
        let track = select_face_track(&samples).unwrap();
        assert_eq!(track.len(), 3);
        // First point is the sampled centre; later points are smoothed.
        assert!((track[0].focus_x - 0.2).abs() < 1e-9);
        assert!((track[0].focus_y - 0.35).abs() < 1e-9);
        assert!((track[1].focus_x - 0.21).abs() < 1e-9);
        assert!((track[2].focus_x - 0.225).abs() < 1e-9);
        assert!((track[0].confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn track_rejects_a_face_seen_once() {
        let samples = vec![
            FaceSample {
                t_seconds: 0.0,
                faces: vec![face(0.1, 0.2, 0.3, 0.5, 0.9)],
            },
            FaceSample {
                t_seconds: 1.0,
                faces: vec![],
            },
            FaceSample {
                t_seconds: 2.0,
                faces: vec![],
            },
        ];
        assert!(select_face_track(&samples).is_none());
    }

    #[test]
    fn track_accepts_a_single_sample_clip() {
        let samples = vec![FaceSample {
            t_seconds: 0.0,
            faces: vec![face(0.1, 0.2, 0.3, 0.5, 0.9)],
        }];
        let track = select_face_track(&samples).unwrap();
        assert_eq!(track.len(), 1);
        assert!((track[0].focus_x - 0.2).abs() < 1e-9);
    }

    #[test]
    fn interpolate_focus_blends_between_keyframes() {
        let track = vec![
            FaceTrackPoint {
                t_seconds: 0.0,
                focus_x: 0.2,
                focus_y: 0.3,
                confidence: 0.9,
            },
            FaceTrackPoint {
                t_seconds: 2.0,
                focus_x: 0.6,
                focus_y: 0.5,
                confidence: 0.9,
            },
        ];
        assert_eq!(interpolate_focus(&track, -1.0), Some((0.2, 0.3)));
        assert_eq!(interpolate_focus(&track, 1.0), Some((0.4, 0.4)));
        assert_eq!(interpolate_focus(&track, 5.0), Some((0.6, 0.5)));
        assert_eq!(interpolate_focus(&[], 1.0), None);
    }
}
