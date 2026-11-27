//! Director v2 agent helpers: pure functions over a [`Project`] and an
//! [`Edl`](crate::edl::Edl) — diffing, conservative QA, caption/grade
//! suggestions from the plan text, and loudness measurement.
//!
//! Nothing here mutates a project or touches the network. `measure_loudness`
//! only runs ffmpeg's loudnorm filter and reports a gain suggestion.

use crate::{
    edl::{Edl, Operation},
    media, Project,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;

/// Index-based difference between two EDLs, as reported on a proposal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdlDiff {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

/// One operation QA removed, with its index in the proposed EDL and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QaDrop {
    pub index: usize,
    pub reason: String,
}

/// Grade suggestion derived from the plan's grade text. Suggestion only —
/// never applied without the user approving the proposal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeSuggestion {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub cube: Option<String>,
}

/// Loudness measurement for one clip. `gain_db` is a suggestion only —
/// audio is never modified.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loudness {
    pub integrated_lufs: f64,
    pub gain_db: f64,
    pub target_lufs: f64,
}

/// Target loudness for [`measure_loudness`], in LUFS.
pub const TARGET_LUFS: f64 = -14.0;

/// Index-based diff: for each index up to the longer EDL, an index past
/// the old end is added, past the new end is removed, and a differing
/// operation at the same index is changed (compared as JSON).
pub fn diff_edls(prev: &Edl, next: &Edl) -> EdlDiff {
    let mut diff = EdlDiff::default();
    for i in 0..prev.operations.len().max(next.operations.len()) {
        match (prev.operations.get(i), next.operations.get(i)) {
            (None, Some(_)) => diff.added += 1,
            (Some(_), None) => diff.removed += 1,
            (Some(old), Some(new)) => {
                let old = serde_json::to_value(old).unwrap_or(Value::Null);
                let new = serde_json::to_value(new).unwrap_or(Value::Null);
                if old != new {
                    diff.changed += 1;
                }
            }
            (None, None) => {}
        }
    }
    diff
}

/// Longest timeline frame: 6 hours at the project frame rate, matching
/// [`crate::timeline::validate`].
fn max_frame(project: &Project) -> u64 {
    21600u64 * project.settings.fps as u64
}

/// Asset length in project frames, or `None` for images / unknown length.
fn asset_frames(project: &Project, asset_id: Uuid) -> Option<u64> {
    project
        .assets
        .iter()
        .find(|a| a.id == asset_id)
        .and_then(|a| {
            a.duration
                .map(|d| (d * project.settings.fps as f64).floor() as u64)
        })
}

fn find_clip<'p>(
    project: &'p Project,
    clip_id: Uuid,
) -> Option<(usize, &'p crate::timeline::Clip)> {
    project
        .tracks
        .iter()
        .enumerate()
        .find_map(|(ti, t)| t.clips.iter().find(|c| c.id == clip_id).map(|c| (ti, c)))
}

/// Every track a clip operation touches: the clip's own track, plus its
/// linked partner's track when the pair is linked.
fn touched_tracks(project: &Project, clip_id: Uuid) -> Vec<usize> {
    let Some((ti, clip)) = find_clip(project, clip_id) else {
        return vec![];
    };
    let mut tracks = vec![ti];
    if let Some(link) = clip.link_id {
        for (tj, t) in project.tracks.iter().enumerate() {
            if tj != ti
                && t.clips
                    .iter()
                    .any(|c| c.link_id == Some(link) && c.id != clip_id)
            {
                tracks.push(tj);
            }
        }
    }
    tracks
}

fn id_taken(project: &Project, id: Uuid) -> bool {
    project
        .tracks
        .iter()
        .any(|t| t.id == id || t.clips.iter().any(|c| c.id == id || c.link_id == Some(id)))
        || project.assets.iter().any(|a| a.id == id)
}

/// Source-frame span a clip operation would occupy, honouring the clip's
/// finishing speed. `None` when the span cannot be computed.
fn source_span(clip: &crate::timeline::Clip, source_in: u32, duration: u32) -> Option<u64> {
    let speed = clip.speed();
    if !speed.is_finite() || speed <= 0.0 {
        return None;
    }
    Some(source_in as u64 + (duration as f64 * speed).ceil() as u64)
}

/// Conservative QA: drop only operations that would certainly fail
/// [`crate::edl::compile`] — unknown clip/track, a locked track, frames
/// out of range for the referenced clip, or an exact duplicate of an
/// earlier operation. Kept operations stay in their original order.
/// Never panics and never drops a valid operation.
pub fn qa_validate(project: &Project, edl: &Edl) -> (Vec<Operation>, Vec<QaDrop>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut seen: Vec<Value> = Vec::new();
    let max = max_frame(project);
    let timeline_end = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| c.start as u64 + c.duration as u64)
        .max()
        .unwrap_or(0);

    for (index, op) in edl.operations.iter().enumerate() {
        let mut drop = |reason: &str| {
            dropped.push(QaDrop {
                index,
                reason: reason.to_string(),
            });
        };
        // Exact duplicate of an earlier operation in this EDL.
        let value = serde_json::to_value(op).unwrap_or(Value::Null);
        if seen.contains(&value) {
            drop("Duplicate operation");
            continue;
        }
        seen.push(value);

        let reason: Option<String> = match op {
            Operation::AddTrack { track_id, .. } => {
                if track_id.get_version_num() != 4 {
                    Some("New track ID must be a UUID v4".into())
                } else if id_taken(project, *track_id) {
                    Some("Track ID is already used in this project".into())
                } else {
                    None
                }
            }
            Operation::Insert { track_id, clip }
            | Operation::Broll { track_id, clip }
            | Operation::Sfx { track_id, clip }
            | Operation::Captions { track_id, clip } => {
                let track = project.tracks.iter().find(|t| t.id == *track_id);
                match track {
                    None => Some("Track ID not found".into()),
                    Some(track) if track.locked => Some("Track is locked".into()),
                    Some(track) => {
                        let kind_ok = match op {
                            Operation::Broll { .. } => track.kind == "video",
                            Operation::Sfx { .. } => track.kind == "audio",
                            Operation::Captions { .. } => track.kind == "caption",
                            _ => true,
                        };
                        if !kind_ok {
                            Some("Clip kind does not match the track kind".into())
                        } else if id_taken(project, clip.id)
                            || clip.link_id.is_some_and(|id| id_taken(project, id))
                        {
                            Some("Clip ID is already used in this project".into())
                        } else if clip.duration == 0 {
                            Some("Clip duration must be greater than zero".into())
                        } else if clip.start as u64 + clip.duration as u64 > max {
                            Some("Clip runs past the end of the project".into())
                        } else {
                            match project.assets.iter().find(|a| a.id == clip.asset_id) {
                                None => Some("Clip asset not found".into()),
                                Some(asset) => {
                                    if track.kind == "audio" && asset.audio_stream.is_none() {
                                        Some("Audio track needs an asset with audio".into())
                                    } else if track.kind == "video"
                                        && (asset.video_stream.is_none() || asset.kind == "audio")
                                    {
                                        Some("Video track needs an asset with video".into())
                                    } else if asset.kind != "image" {
                                        match asset_frames(project, clip.asset_id) {
                                            Some(frames)
                                                if source_span(clip, clip.source_in, clip.duration)
                                                    .is_some_and(|end| end > frames) =>
                                            {
                                                Some(
                                                    "Clip source range runs past the end of the media"
                                                        .into(),
                                                )
                                            }
                                            _ => None,
                                        }
                                    } else {
                                        None
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Operation::Move { clip_id, start } => check_clip(project, *clip_id).or_else(|| {
                let (_, clip) = find_clip(project, *clip_id)?;
                if *start as u64 + clip.duration as u64 > max {
                    Some("Move would run past the end of the project".into())
                } else {
                    None
                }
            }),
            Operation::Trim {
                clip_id,
                start,
                source_in,
                duration,
            } => check_clip(project, *clip_id).or_else(|| {
                let (_, clip) = find_clip(project, *clip_id)?;
                if *duration == 0 {
                    Some("Trim duration must be greater than zero".into())
                } else if *start as u64 + *duration as u64 > max {
                    Some("Trim would run past the end of the project".into())
                } else if clip.asset_id != Uuid::nil()
                    && project
                        .assets
                        .iter()
                        .find(|a| a.id == clip.asset_id)
                        .is_some_and(|a| a.kind != "image")
                    && asset_frames(project, clip.asset_id).is_some_and(|frames| {
                        source_span(clip, *source_in, *duration).is_some_and(|end| end > frames)
                    })
                {
                    Some("Trim source range runs past the end of the media".into())
                } else {
                    None
                }
            }),
            Operation::Split {
                clip_id,
                frame,
                right_ids,
                right_link_id,
            } => check_clip(project, *clip_id).or_else(|| {
                let (_, clip) = find_clip(project, *clip_id)?;
                let linked = clip.link_id.is_some();
                if *frame <= clip.start || *frame >= clip.start + clip.duration {
                    Some("Split frame must be inside the clip".into())
                } else if right_ids.len() != if linked { 2 } else { 1 } {
                    Some("Split needs one right ID per linked member".into())
                } else if right_link_id.is_some() != linked {
                    Some("Split of a linked pair needs a new link ID".into())
                } else if right_ids
                    .iter()
                    .chain(right_link_id.iter())
                    .any(|id| id.get_version_num() != 4 || id_taken(project, *id))
                {
                    Some("Split right IDs must be fresh UUID v4 values".into())
                } else {
                    None
                }
            }),
            Operation::Delete { clip_id } => check_clip(project, *clip_id),
            Operation::Ripple { start, end } => {
                if start >= end {
                    Some("Ripple range must have an end after its start".into())
                } else if *end as u64 > timeline_end {
                    Some("Ripple range runs past the end of the timeline".into())
                } else if project.tracks.iter().any(|t| {
                    t.locked
                        && t.clips
                            .iter()
                            .any(|c| c.start as u64 + c.duration as u64 > *start as u64)
                }) {
                    Some("Ripple affects a locked track".into())
                } else {
                    None
                }
            }
            Operation::JCut { clip_id, frames } | Operation::LCut { clip_id, frames } => {
                check_clip(project, *clip_id).or_else(|| {
                    if *frames == 0 {
                        Some("J/L cut needs a positive frame count".into())
                    } else if touched_tracks(project, *clip_id).len() != 2 {
                        Some("J/L cut requires a linked video/audio pair".into())
                    } else {
                        None
                    }
                })
            }
            Operation::Zoom { clip_id, keys } => check_clip(project, *clip_id).or_else(|| {
                let (ti, clip) = find_clip(project, *clip_id)?;
                if project.tracks[ti].kind != "video" {
                    Some("Zoom requires a video track".into())
                } else {
                    check_keys(keys, clip, project, true)
                }
            }),
            Operation::Lut { clip_id, .. } => check_clip(project, *clip_id).or_else(|| {
                let (ti, _) = find_clip(project, *clip_id)?;
                if project.tracks[ti].kind != "video" {
                    Some("LUT requires a video track".into())
                } else {
                    None
                }
            }),
            Operation::Duck { clip_id, keys } => check_clip(project, *clip_id).or_else(|| {
                let (ti, clip) = find_clip(project, *clip_id)?;
                if project.tracks[ti].kind != "audio" {
                    Some("Ducking requires an audio track".into())
                } else {
                    check_keys(keys, clip, project, false)
                }
            }),
            Operation::CaptionStyle { clip_id, .. } => {
                check_clip(project, *clip_id).or_else(|| {
                    let (ti, _) = find_clip(project, *clip_id)?;
                    if project.tracks[ti].kind != "caption" {
                        Some("Caption style requires a caption track".into())
                    } else {
                        None
                    }
                })
            }
        };
        match reason {
            Some(reason) => drop(&reason),
            None => kept.push(op.clone()),
        }
    }
    (kept, dropped)
}

/// Shared check for operations that target one clip: the clip must exist
/// and none of the tracks it touches may be locked.
fn check_clip(project: &Project, clip_id: Uuid) -> Option<String> {
    if find_clip(project, clip_id).is_none() {
        return Some("Clip ID not found".into());
    }
    if touched_tracks(project, clip_id)
        .iter()
        .any(|&ti| project.tracks[ti].locked)
    {
        return Some("Operation affects a locked track".into());
    }
    None
}

/// Keyframe bounds for Zoom (scale 1–4, centre 0–1) and Duck (gain 0–1)
/// keys: frames must be ordered and inside the referenced clip's media.
fn check_keys(
    keys: &[impl Keyframe],
    clip: &crate::timeline::Clip,
    project: &Project,
    zoom: bool,
) -> Option<String> {
    let limit = match asset_frames(project, clip.asset_id) {
        Some(frames) => frames,
        None => max_frame(project),
    };
    let mut previous: Option<u32> = None;
    for key in keys {
        if key.frame() as u64 > limit
            || previous.is_some_and(|p| key.frame() <= p)
            || !key.value_ok(zoom)
        {
            return Some(if zoom {
                "Invalid zoom keyframe".into()
            } else {
                "Invalid duck keyframe".into()
            });
        }
        previous = Some(key.frame());
    }
    None
}

trait Keyframe {
    fn frame(&self) -> u32;
    fn value_ok(&self, zoom: bool) -> bool;
}

impl Keyframe for crate::edl::ZoomKey {
    fn frame(&self) -> u32 {
        self.frame
    }
    fn value_ok(&self, _zoom: bool) -> bool {
        self.scale.is_finite()
            && (1.0..=4.0).contains(&self.scale)
            && self.x.is_finite()
            && (0.0..=1.0).contains(&self.x)
            && self.y.is_finite()
            && (0.0..=1.0).contains(&self.y)
    }
}

impl Keyframe for crate::edl::GainKey {
    fn frame(&self) -> u32 {
        self.frame
    }
    fn value_ok(&self, _zoom: bool) -> bool {
        self.gain.is_finite() && (0.0..=1.0).contains(&self.gain)
    }
}

/// Caption preset from the plan's captions text: keyword map, defaulting
/// to Hormozi-style captions.
pub fn pick_caption_preset(plan_captions: &str) -> &'static str {
    let lower = plan_captions.to_lowercase();
    if lower.contains("bold") || lower.contains("punch") || lower.contains("hormozi") {
        "hormozi"
    } else if lower.contains("clean") || lower.contains("minimal") {
        "minimal"
    } else if lower.contains("documentary") || lower.contains("lower third") {
        "documentary"
    } else if lower.contains("neon") || lower.contains("glow") || lower.contains("electric") {
        "neon"
    } else {
        "hormozi"
    }
}

/// Grade suggestion from the plan's grade text: keyword map over a
/// neutral base, clamped to brightness −1..1, contrast 0..2, saturation
/// 0..3.
pub fn grade_suggestion(plan_grade: &str) -> GradeSuggestion {
    let lower = plan_grade.to_lowercase();
    let mut suggestion = GradeSuggestion {
        brightness: 0.0,
        contrast: 1.0,
        saturation: 1.0,
        cube: None,
    };
    if lower.contains("warm") {
        suggestion.cube = Some("warm".into());
    }
    if lower.contains("cool") {
        suggestion.cube = Some("cool".into());
    }
    if lower.contains("more contrast") || lower.contains("punchier") {
        suggestion.contrast = 1.15;
    }
    if lower.contains("less contrast") || lower.contains("flat") {
        suggestion.contrast = 0.9;
    }
    if lower.contains("desaturat") || lower.contains("muted") {
        suggestion.saturation = 0.8;
    } else if lower.contains("saturat") {
        suggestion.saturation = 1.2;
    }
    if lower.contains("bright") {
        suggestion.brightness = 0.1;
    }
    if lower.contains("dark") || lower.contains("moody") {
        suggestion.brightness = -0.1;
    }
    suggestion.brightness = suggestion.brightness.clamp(-1.0, 1.0);
    suggestion.contrast = suggestion.contrast.clamp(0.0, 2.0);
    suggestion.saturation = suggestion.saturation.clamp(0.0, 3.0);
    suggestion
}

/// Pull the integrated loudness out of loudnorm output, in either form:
/// the JSON block (`"input_i" : "-23.45"`) or the text line
/// (`Input Integrated: -23.4 LUFS`).
pub fn parse_loudnorm(stderr: &str) -> Option<f64> {
    // JSON block form.
    if let Some(at) = stderr.find("\"input_i\"") {
        let rest = &stderr[at + "\"input_i\"".len()..];
        if let Some(colon) = rest.find(':') {
            let value = rest[colon + 1..]
                .trim_start()
                .trim_start_matches('"')
                .split(['"', ',', '}', '\n', ' '])
                .next()
                .unwrap_or("");
            if let Ok(parsed) = value.parse::<f64>() {
                return Some(parsed);
            }
        }
    }
    // Text line form.
    for line in stderr.lines() {
        if let Some((_, rest)) = line.split_once("Input Integrated:") {
            let first = rest.trim().split_whitespace().next().unwrap_or("");
            if let Ok(parsed) = first.parse::<f64>() {
                return Some(parsed);
            }
        }
    }
    None
}

/// Measure one clip's integrated loudness with ffmpeg loudnorm and
/// suggest the gain needed to reach [`TARGET_LUFS`], clamped to ±12 dB
/// and rounded to 0.1 dB. Suggestion only — audio is never modified.
pub fn measure_loudness(project: &Project, clip_id: Uuid) -> Result<Loudness, String> {
    let clip = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.id == clip_id)
        .ok_or("Clip not found")?;
    let asset = project
        .assets
        .iter()
        .find(|a| a.id == clip.asset_id)
        .ok_or("Asset not found")?;
    if asset.audio_stream.is_none() {
        return Err("Clip has no audio stream".into());
    }
    let source = media::verified_source(asset)?;
    let source = source.to_str().ok_or("Invalid media path")?;
    let source_in_s = clip.source_in as f64 / project.settings.fps as f64;
    let duration_s = clip.duration as f64 / project.settings.fps as f64;
    let (_stdout, stderr) = media::run_text(
        "ffmpeg",
        &[
            "-ss",
            &source_in_s.to_string(),
            "-t",
            &duration_s.to_string(),
            "-i",
            source,
            "-vn",
            "-af",
            "loudnorm=print_format=json",
            "-f",
            "null",
            "-",
        ],
        Duration::from_secs(60),
        1_048_576,
    )?;
    let integrated = parse_loudnorm(&stderr).ok_or("Could not read loudness from ffmpeg output")?;
    let gain = ((TARGET_LUFS - integrated).clamp(-12.0, 12.0) * 10.0).round() / 10.0;
    Ok(Loudness {
        integrated_lufs: integrated,
        gain_db: gain,
        target_lufs: TARGET_LUFS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        edl::ZoomKey,
        timeline::{Clip, Track},
    };

    fn asset(id: Uuid, kind: &str, duration: Option<f64>) -> media::Asset {
        media::Asset {
            id,
            source_path: "/tmp/test-media.mp4".into(),
            name: "test".into(),
            size_bytes: 1024,
            modified_ns: "0".into(),
            kind: kind.into(),
            duration,
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            video_stream: if kind == "audio" { None } else { Some(0) },
            audio_stream: if kind == "image" { None } else { Some(1) },
            video_codec: None,
            audio_codec: None,
            sample_rate: Some(48000),
            channels: Some(2),
            created_at: 0,
            bin: String::new(),
        }
    }

    fn clip(id: Uuid, asset_id: Uuid, start: u32, source_in: u32, duration: u32) -> Clip {
        Clip {
            id,
            asset_id,
            link_id: None,
            start,
            source_in,
            duration,
            caption: None,
            effects: None,
        }
    }

    fn project() -> (Project, Uuid, Uuid, Uuid) {
        let asset_id = Uuid::new_v4();
        let clip_id = Uuid::new_v4();
        let track_id = Uuid::new_v4();
        let project = Project {
            schema_version: 5,
            id: Uuid::new_v4(),
            name: "Test".into(),
            revision: 0,
            created_at: 0,
            settings: crate::Settings {
                width: 1920,
                height: 1080,
                fps: 30,
                audio_sample_rate: 48000,
            },
            assets: vec![asset(asset_id, "video", Some(60.0))],
            tracks: vec![Track {
                id: track_id,
                name: "V1".into(),
                kind: "video".into(),
                muted: false,
                solo: false,
                locked: false,
                clips: vec![clip(clip_id, asset_id, 0, 0, 900)],
            }],
            markers: vec![],
        };
        (project, asset_id, clip_id, track_id)
    }

    fn edl(project_id: Uuid, operations: Vec<Operation>) -> Edl {
        Edl {
            version: 1,
            project_id,
            base_revision: 0,
            summary: "test".into(),
            operations,
        }
    }

    #[test]
    fn diff_edls_counts_added_removed_changed() {
        let id = Uuid::new_v4();
        let clip = Uuid::new_v4();
        let prev = edl(
            id,
            vec![
                Operation::Move {
                    clip_id: clip,
                    start: 0,
                },
                Operation::Delete { clip_id: clip },
            ],
        );
        let next = edl(
            id,
            vec![
                Operation::Move {
                    clip_id: clip,
                    start: 30,
                },
                Operation::Delete { clip_id: clip },
                Operation::Delete { clip_id: clip },
            ],
        );
        let diff = diff_edls(&prev, &next);
        assert_eq!(diff.changed, 1);
        assert_eq!(diff.added, 1);
        assert_eq!(diff.removed, 0);

        let shorter = edl(id, vec![Operation::Delete { clip_id: clip }]);
        let diff = diff_edls(&prev, &shorter);
        assert_eq!((diff.added, diff.removed, diff.changed), (0, 1, 1));
        assert_eq!(diff_edls(&prev, &prev), EdlDiff::default());
    }

    #[test]
    fn qa_validate_keeps_valid_and_drops_bad_ops() {
        let (project, asset_id, clip_id, track_id) = project();
        let missing = Uuid::new_v4();
        let fresh_clip = Uuid::new_v4();
        let ops = vec![
            Operation::Move { clip_id, start: 30 },
            Operation::Move {
                clip_id: missing,
                start: 0,
            },
            Operation::Trim {
                clip_id,
                start: 0,
                source_in: 0,
                duration: 1800, // 60 s at 30 fps: exactly the asset length, valid
            },
            Operation::Trim {
                clip_id,
                start: 0,
                source_in: 1700,
                duration: 300, // source 1700..2000 > 1800 asset frames
            },
            Operation::Split {
                clip_id,
                frame: 900, // at the clip end, outside
                right_ids: vec![Uuid::new_v4()],
                right_link_id: None,
            },
            Operation::Move { clip_id, start: 30 }, // exact duplicate of op 0
            Operation::Insert {
                track_id,
                clip: clip(fresh_clip, asset_id, 0, 0, 30),
            },
            Operation::Insert {
                track_id: Uuid::new_v4(),
                clip: clip(Uuid::new_v4(), asset_id, 0, 0, 30),
            },
        ];
        let (kept, dropped) = qa_validate(&project, &edl(project.id, ops));
        assert_eq!(kept.len(), 3);
        assert_eq!(
            dropped.iter().map(|d| d.index).collect::<Vec<_>>(),
            vec![1, 3, 4, 5, 7]
        );
        assert!(dropped.iter().all(|d| !d.reason.is_empty()));
    }

    #[test]
    fn qa_validate_drops_ops_on_locked_tracks() {
        let (mut project, _, clip_id, _) = project();
        project.tracks[0].locked = true;
        let ops = vec![
            Operation::Move { clip_id, start: 30 },
            Operation::Delete { clip_id },
        ];
        let (kept, dropped) = qa_validate(&project, &edl(project.id, ops));
        assert!(kept.is_empty());
        assert_eq!(dropped.len(), 2);
        assert!(dropped
            .iter()
            .all(|d| d.reason.to_lowercase().contains("locked")));
    }

    #[test]
    fn pick_caption_preset_keyword_map() {
        assert_eq!(pick_caption_preset("Bold punchy Hormozi words"), "hormozi");
        assert_eq!(pick_caption_preset("clean minimal lower thirds"), "minimal");
        assert_eq!(
            pick_caption_preset("documentary style captions"),
            "documentary"
        );
        assert_eq!(pick_caption_preset("lower third captions"), "documentary");
        assert_eq!(pick_caption_preset("neon glow captions"), "neon");
        assert_eq!(pick_caption_preset("something else entirely"), "hormozi");
        assert_eq!(pick_caption_preset(""), "hormozi");
    }

    #[test]
    fn grade_suggestion_keywords_and_clamps() {
        let warm = grade_suggestion("warm grade, more contrast, saturated, bright");
        assert_eq!(warm.cube.as_deref(), Some("warm"));
        assert_eq!(warm.contrast, 1.15);
        assert_eq!(warm.saturation, 1.2);
        assert_eq!(warm.brightness, 0.1);

        let cool = grade_suggestion("cool moody muted flat grade");
        assert_eq!(cool.cube.as_deref(), Some("cool"));
        assert_eq!(cool.contrast, 0.9);
        assert_eq!(cool.saturation, 0.8);
        assert_eq!(cool.brightness, -0.1);

        let plain = grade_suggestion("keep it natural");
        assert_eq!(
            plain,
            GradeSuggestion {
                brightness: 0.0,
                contrast: 1.0,
                saturation: 1.0,
                cube: None,
            }
        );
        // Every output stays inside the clamps.
        for text in ["warm cool bright dark", "punchier desaturated"] {
            let suggestion = grade_suggestion(text);
            assert!((-1.0..=1.0).contains(&suggestion.brightness));
            assert!((0.0..=2.0).contains(&suggestion.contrast));
            assert!((0.0..=3.0).contains(&suggestion.saturation));
        }
        // "desaturated" must not match the plain "saturat" branch.
        assert_eq!(grade_suggestion("desaturated look").saturation, 0.8);
    }

    #[test]
    fn parse_loudnorm_reads_json_block_and_text_line() {
        let stderr = r#"[Parsed_loudnorm_0 @ 0x7f8c1c004000] {
            "input_i" : "-23.45",
            "input_tp" : "-2.11",
            "input_lra" : "7.30",
            "input_thresh" : "-34.18",
            "output_i" : "-14.02",
            "output_tp" : "-1.00",
            "output_lra" : "5.40",
            "output_thresh" : "-24.18",
            "normalization_type" : "dynamic",
            "target_offset" : "-0.13"
        }"#;
        assert_eq!(parse_loudnorm(stderr), Some(-23.45));
        assert_eq!(
            parse_loudnorm("Input Integrated: -23.4 LUFS\nInput True Peak: -2.1 dBTP"),
            Some(-23.4)
        );
        assert_eq!(parse_loudnorm("no loudness here"), None);
        assert_eq!(parse_loudnorm(""), None);
    }

    #[test]
    fn zoom_keys_are_validated_by_qa() {
        let (project, _, clip_id, _) = project();
        let bad_scale = Operation::Zoom {
            clip_id,
            keys: vec![ZoomKey {
                frame: 10,
                scale: 9.0,
                x: 0.5,
                y: 0.5,
            }],
        };
        let good = Operation::Zoom {
            clip_id,
            keys: vec![ZoomKey {
                frame: 10,
                scale: 2.0,
                x: 0.5,
                y: 0.5,
            }],
        };
        let (kept, dropped) = qa_validate(&project, &edl(project.id, vec![bad_scale, good]));
        assert_eq!(kept.len(), 1);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].index, 0);
    }
}
