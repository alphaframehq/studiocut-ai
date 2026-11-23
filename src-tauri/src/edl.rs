use crate::{
    timeline::{Caption, Clip, History, Track},
    Project,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ZoomKey {
    pub frame: u32,
    pub scale: f64,
    pub x: f64,
    pub y: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GainKey {
    pub frame: u32,
    pub gain: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Lut {
    Identity,
    Warm,
    Cool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Effects {
    pub zoom: Vec<ZoomKey>,
    pub lut: Option<Lut>,
    pub duck: Vec<GainKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finishing: Option<crate::finishing::Finishing>,
}
impl Effects {
    pub fn validate(&self, kind: &str, max: u32) -> Result<(), String> {
        if self.zoom.len() > 64
            || self.duck.len() > 128
            || (kind != "video" && (!self.zoom.is_empty() || self.lut.is_some()))
            || (kind != "audio" && !self.duck.is_empty())
        {
            return Err("Effects incompatible with track or too many keyframes".into());
        }
        for (i, k) in self.zoom.iter().enumerate() {
            if k.frame > max
                || (i > 0 && k.frame <= self.zoom[i - 1].frame)
                || !k.scale.is_finite()
                || !(1.0..=4.0).contains(&k.scale)
                || !k.x.is_finite()
                || !(0.0..=1.0).contains(&k.x)
                || !k.y.is_finite()
                || !(0.0..=1.0).contains(&k.y)
            {
                return Err("Invalid zoom keyframe".into());
            }
        }
        for (i, k) in self.duck.iter().enumerate() {
            if k.frame > max
                || (i > 0 && k.frame <= self.duck[i - 1].frame)
                || !k.gain.is_finite()
                || !(0.0..=1.0).contains(&k.gain)
            {
                return Err("Invalid duck keyframe".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Edl {
    pub version: u32,
    pub project_id: Uuid,
    pub base_revision: u64,
    pub summary: String,
    pub operations: Vec<Operation>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Video,
    Audio,
    Caption,
}
impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Caption => "caption",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Operation {
    AddTrack {
        track_id: Uuid,
        name: String,
        kind: Kind,
    },
    Insert {
        track_id: Uuid,
        clip: Clip,
    },
    Broll {
        track_id: Uuid,
        clip: Clip,
    },
    Sfx {
        track_id: Uuid,
        clip: Clip,
    },
    Captions {
        track_id: Uuid,
        clip: Clip,
    },
    Move {
        clip_id: Uuid,
        start: u32,
    },
    Trim {
        clip_id: Uuid,
        start: u32,
        source_in: u32,
        duration: u32,
    },
    Split {
        clip_id: Uuid,
        frame: u32,
        right_ids: Vec<Uuid>,
        right_link_id: Option<Uuid>,
    },
    Delete {
        clip_id: Uuid,
    },
    Ripple {
        start: u32,
        end: u32,
    },
    JCut {
        clip_id: Uuid,
        frames: u32,
    },
    LCut {
        clip_id: Uuid,
        frames: u32,
    },
    Zoom {
        clip_id: Uuid,
        keys: Vec<ZoomKey>,
    },
    Lut {
        clip_id: Uuid,
        preset: Lut,
    },
    Duck {
        clip_id: Uuid,
        keys: Vec<GainKey>,
    },
    CaptionStyle {
        clip_id: Uuid,
        caption: Caption,
    },
}
pub fn parse(text: &str) -> Result<Edl, String> {
    if text.len() > 262144 {
        return Err("EDL exceeds 256 KB".into());
    }
    serde_json::from_str(text).map_err(|e| format!("Invalid EDL: {e}"))
}
fn fresh(project: &Project, id: Uuid) -> Result<(), String> {
    if id.get_version_num() != 4
        || project
            .tracks
            .iter()
            .any(|t| t.id == id || t.clips.iter().any(|c| c.id == id || c.link_id == Some(id)))
        || project.assets.iter().any(|a| a.id == id)
    {
        return Err("New ID must be a unique UUID v4".into());
    }
    Ok(())
}
fn targets(project: &Project, id: Uuid, linked: bool) -> Result<Vec<(usize, usize)>, String> {
    let clip = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.id == id)
        .ok_or("Clip ID not found")?;
    let mut output = Vec::new();
    for (ti, t) in project.tracks.iter().enumerate() {
        for (ci, c) in t.clips.iter().enumerate() {
            if c.id == id || (linked && clip.link_id.is_some() && c.link_id == clip.link_id) {
                if t.locked {
                    return Err("Operation affects a locked track".into());
                }
                output.push((ti, ci));
            }
        }
    }
    Ok(output)
}
fn add(a: u32, b: u32) -> Result<u32, String> {
    a.checked_add(b).ok_or("Frame overflow".into())
}
pub fn compile(project: &Project, edl: &Edl) -> Result<Project, String> {
    project.validate()?;
    if edl.version != 1 || edl.project_id != project.id || edl.base_revision != project.revision {
        return Err("Unsupported EDL, wrong project or stale revision".into());
    }
    if edl.summary.trim().is_empty()
        || edl.summary.encode_utf16().count() > 2000
        || edl.operations.len() > 64
    {
        return Err("EDL needs a summary and at most 64 operations".into());
    }
    let mut next = project.clone();
    next.schema_version = project.schema_version.max(5);
    for op in &edl.operations {
        match op {
            Operation::AddTrack {
                track_id,
                name,
                kind,
            } => {
                fresh(&next, *track_id)?;
                next.tracks.push(Track {
                    id: *track_id,
                    name: name.clone(),
                    kind: kind.name().into(),
                    muted: false,
                    solo: false,
                    locked: false,
                    clips: vec![],
                });
            }
            Operation::Insert { track_id, clip }
            | Operation::Broll { track_id, clip }
            | Operation::Sfx { track_id, clip }
            | Operation::Captions { track_id, clip } => {
                fresh(&next, clip.id)?;
                let track = next
                    .tracks
                    .iter_mut()
                    .find(|t| t.id == *track_id)
                    .ok_or("Track ID not found")?;
                if track.locked {
                    return Err("Track is locked".into());
                }
                if (matches!(op, Operation::Broll { .. }) && track.kind != "video")
                    || (matches!(op, Operation::Sfx { .. }) && track.kind != "audio")
                    || (matches!(op, Operation::Captions { .. }) && track.kind != "caption")
                {
                    return Err("Placement requires the matching track kind".into());
                }
                track.clips.push(clip.clone());
            }
            Operation::Move { clip_id, start } => {
                for (t, c) in targets(&next, *clip_id, true)? {
                    next.tracks[t].clips[c].start = *start;
                }
            }
            Operation::Trim {
                clip_id,
                start,
                source_in,
                duration,
            } => {
                for (t, c) in targets(&next, *clip_id, true)? {
                    let clip = &mut next.tracks[t].clips[c];
                    clip.start = *start;
                    clip.source_in = *source_in;
                    clip.duration = *duration;
                }
            }
            Operation::Delete { clip_id } => {
                for (t, c) in targets(&next, *clip_id, true)?.into_iter().rev() {
                    next.tracks[t].clips.remove(c);
                }
            }
            Operation::Split {
                clip_id,
                frame,
                right_ids,
                right_link_id,
            } => {
                let indices = targets(&next, *clip_id, true)?;
                if right_ids.len() != indices.len()
                    || (indices.len() == 2) != right_link_id.is_some()
                {
                    return Err(
                        "Split needs one right ID per linked member and a new link for a pair"
                            .into(),
                    );
                }
                let mut unique = std::collections::HashSet::new();
                for id in right_ids.iter().chain(right_link_id.iter()) {
                    fresh(&next, *id)?;
                    if !unique.insert(id) {
                        return Err("Duplicate split ID".into());
                    }
                }
                for ((t, c), id) in indices.into_iter().zip(right_ids) {
                    let clip = &mut next.tracks[t].clips[c];
                    if *frame <= clip.start || *frame >= add(clip.start, clip.duration)? {
                        return Err("Split must be inside the clip".into());
                    }
                    let offset = frame - clip.start;
                    let mut right = clip.clone();
                    right.id = *id;
                    right.link_id = *right_link_id;
                    right.start = *frame;
                    right.source_in = add(right.source_in, right.source_offset(offset)?)?;
                    right.duration -= offset;
                    clip.duration = offset;
                    next.tracks[t].clips.push(right);
                }
            }
            Operation::Ripple { start, end } => {
                next = crate::analysis::ripple(
                    &next,
                    &[crate::analysis::Cut {
                        start: *start,
                        end: *end,
                    }],
                    next.revision,
                )?;
                next.revision = project.revision;
            }
            Operation::JCut { clip_id, frames } | Operation::LCut { clip_id, frames } => {
                let indices = targets(&next, *clip_id, true)?;
                if *frames == 0 || indices.len() != 2 {
                    return Err("J/L cut requires a linked A/V pair and positive frames".into());
                }
                for (t, c) in indices {
                    let track = &mut next.tracks[t];
                    let clip = &mut track.clips[c];
                    clip.link_id = None;
                    if track.kind == "audio" {
                        if matches!(op, Operation::JCut { .. }) {
                            clip.start = clip
                                .start
                                .checked_sub(*frames)
                                .ok_or("J-cut starts before timeline")?;
                            clip.source_in = clip
                                .source_in
                                .checked_sub(clip.source_offset(*frames)?)
                                .ok_or("J-cut starts before source")?;
                        }
                        clip.duration = add(clip.duration, *frames)?;
                    }
                }
            }
            Operation::Zoom { clip_id, keys } => {
                let (t, c) = targets(&next, *clip_id, false)?[0];
                if next.tracks[t].kind != "video" {
                    return Err("Zoom requires video".into());
                }
                next.tracks[t].clips[c]
                    .effects
                    .get_or_insert_with(Effects::default)
                    .zoom = keys.clone();
            }
            Operation::Lut { clip_id, preset } => {
                let (t, c) = targets(&next, *clip_id, false)?[0];
                if next.tracks[t].kind != "video" {
                    return Err("LUT requires video".into());
                }
                next.tracks[t].clips[c]
                    .effects
                    .get_or_insert_with(Effects::default)
                    .lut = Some(preset.clone());
            }
            Operation::Duck { clip_id, keys } => {
                let (t, c) = targets(&next, *clip_id, false)?[0];
                if next.tracks[t].kind != "audio" {
                    return Err("Ducking requires audio".into());
                }
                next.tracks[t].clips[c]
                    .effects
                    .get_or_insert_with(Effects::default)
                    .duck = keys.clone();
            }
            Operation::CaptionStyle { clip_id, caption } => {
                let (t, c) = targets(&next, *clip_id, false)?[0];
                next.tracks[t].clips[c].caption = Some(caption.clone());
            }
        }
        for track in &mut next.tracks {
            track.clips.sort_by_key(|c| c.start);
        }
    }
    next.revision = project.revision.checked_add(1).ok_or("Revision overflow")?;
    next.validate()?;
    if serde_json::to_vec_pretty(&next)
        .map_err(|e| e.to_string())?
        .len() as u64
        > crate::MAX_PROJECT_BYTES
    {
        return Err("Project exceeds 1 MB".into());
    }
    Ok(next)
}
pub fn apply(project: &Project, edl: &Edl, history: &mut History) -> Result<Project, String> {
    if edl.operations.is_empty() {
        return Err("Proposal has no edits to apply".into());
    }
    let next = compile(project, edl)?;
    let mut upgraded = project.clone();
    upgraded.schema_version = project.schema_version.max(5);
    history.apply(&upgraded, next.tracks, project.revision)
}
