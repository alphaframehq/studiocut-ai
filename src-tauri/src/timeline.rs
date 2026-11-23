use crate::Project;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub const HISTORY_LIMIT: usize = 50;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Clip {
    pub id: Uuid,
    pub asset_id: Uuid,
    pub link_id: Option<Uuid>,
    pub start: u32,
    pub source_in: u32,
    pub duration: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<Caption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<crate::edl::Effects>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Caption {
    pub preset: String,
    pub timing: String,
    pub words: Vec<CaptionWord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionWord {
    pub text: String,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Track {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub muted: bool,
    pub solo: bool,
    pub locked: bool,
    pub clips: Vec<Clip>,
}

impl Clip {
    pub fn speed(&self) -> f64 {
        self.effects
            .as_ref()
            .and_then(|e| e.finishing.as_ref())
            .map_or(1.0, |f| f.speed)
    }
    pub fn source_offset(&self, frames: u32) -> Result<u32, String> {
        let value = frames as f64 * self.speed();
        if value.fract().abs() > 0.0000001 || value > u32::MAX as f64 {
            return Err("Edit must land on a whole source frame at this speed".into());
        }
        Ok(value.round() as u32)
    }
}
pub fn validate(project: &Project) -> Result<(), String> {
    if project.tracks.len() > 32 || (project.schema_version < 3 && !project.tracks.is_empty()) {
        return Err("Unsupported timeline tracks".into());
    }
    let mut tracks = HashSet::new();
    let mut clips = HashSet::new();
    let mut links: HashMap<Uuid, Vec<(&str, &Clip)>> = HashMap::new();
    let maximum = 21600u64 * project.settings.fps as u64;
    for track in &project.tracks {
        if track.id.get_version_num() != 4
            || !tracks.insert(track.id)
            || track.name.trim().is_empty()
            || track.name.encode_utf16().count() > 80
            || !["video", "audio", "caption"].contains(&track.kind.as_str())
            || (track.kind == "caption" && project.schema_version < 4)
        {
            return Err("Invalid timeline track".into());
        }
        let mut end = 0u64;
        for clip in &track.clips {
            let asset = project
                .assets
                .iter()
                .find(|a| a.id == clip.asset_id)
                .ok_or("Clip asset not found")?;
            let clip_end = clip.start as u64 + clip.duration as u64;
            let speed = clip
                .effects
                .as_ref()
                .and_then(|e| e.finishing.as_ref())
                .map_or(1.0, |f| f.speed);
            let source_end = clip.source_in as u64 + (clip.duration as f64 * speed).ceil() as u64;
            if let Some(f) = clip.effects.as_ref().and_then(|e| e.finishing.as_ref()) {
                if project.schema_version < 6 {
                    return Err("Finishing requires project v6".into());
                }
                f.validate(&track.kind, maximum as u32)?;
            }
            if clip.id.get_version_num() != 4
                || !clips.insert(clip.id)
                || clip.duration == 0
                || (clip.start as u64) < end
                || clip_end > maximum
                || source_end > maximum
                || (asset.kind != "image"
                    && source_end
                        > (asset.duration.unwrap_or(0.0) * project.settings.fps as f64).floor()
                            as u64)
                || (track.kind == "audio" && asset.audio_stream.is_none())
                || (track.kind == "video"
                    && (asset.video_stream.is_none() || asset.kind == "audio"))
                || clip.link_id.is_some_and(|id| id.get_version_num() != 4)
            {
                return Err("Invalid clip: check source bounds, overlaps, and track type".into());
            }
            if let Some(effects) = &clip.effects {
                if project.schema_version < 5 {
                    return Err("Effects require project v5".into());
                }
                effects.validate(
                    &track.kind,
                    if asset.kind == "image" {
                        maximum as u32
                    } else {
                        (asset.duration.unwrap_or(0.0) * project.settings.fps as f64).floor() as u32
                    },
                )?;
            }
            match (&clip.caption, track.kind.as_str()) {
                (Some(c), "caption") if project.schema_version >= 4 => {
                    if clip.link_id.is_some()
                        || !["hormozi", "minimal", "documentary", "neon"]
                            .contains(&c.preset.as_str())
                        || !["word", "segment"].contains(&c.timing.as_str())
                        || c.words.is_empty()
                        || c.words.len() > 200
                    {
                        return Err("Invalid caption clip".into());
                    }
                    let mut previous = 0;
                    for word in &c.words {
                        if word.text.trim().is_empty()
                            || word.text.encode_utf16().count() > 500
                            || word.start < previous
                            || word.end <= word.start
                            || word.end as u64
                                > (asset.duration.unwrap_or(0.0) * project.settings.fps as f64)
                                    .floor() as u64
                        {
                            return Err("Invalid caption timestamps".into());
                        }
                        previous = word.end;
                    }
                }
                (None, "video" | "audio") => (),
                _ => return Err("Caption data must belong to a caption track".into()),
            }
            end = clip_end;
            if let Some(link) = clip.link_id {
                links.entry(link).or_default().push((&track.kind, clip));
            }
        }
    }
    if clips.len() > 2000 {
        return Err("Maximum 2,000 clips".into());
    }
    for pair in links.values() {
        if pair.len() != 2 {
            return Err("Linked clips must be a synchronized video/audio pair".into());
        }
        let ((ak, a), (bk, b)) = (pair[0], pair[1]);
        if ak == bk
            || a.asset_id != b.asset_id
            || a.start != b.start
            || a.source_in != b.source_in
            || a.duration != b.duration
            || a.speed() != b.speed()
        {
            return Err("Linked clips must be a synchronized video/audio pair".into());
        }
    }
    Ok(())
}

pub fn replace_tracks(
    project: &Project,
    tracks: Vec<Track>,
    expected_revision: u64,
) -> Result<Project, String> {
    if expected_revision != project.revision {
        return Err("Project changed; retry the edit".into());
    }
    let mut next = project.clone();
    next.schema_version = if project.schema_version >= 6
        || tracks.iter().any(|t| {
            t.clips
                .iter()
                .any(|c| c.effects.as_ref().is_some_and(|e| e.finishing.is_some()))
        }) {
        6
    } else if project.schema_version >= 5
        || tracks
            .iter()
            .any(|t| t.clips.iter().any(|c| c.effects.is_some()))
    {
        5
    } else if project.schema_version >= 4 || tracks.iter().any(|t| t.kind == "caption") {
        4
    } else {
        3
    };
    next.revision = next.revision.checked_add(1).ok_or("Revision overflow")?;
    next.tracks = tracks;
    next.validate()?;
    if serde_json::to_vec_pretty(&next)
        .map_err(|e| e.to_string())?
        .len() as u64
        > crate::MAX_PROJECT_BYTES
    {
        return Err("Project file exceeds 1 MB".into());
    }
    Ok(next)
}

#[derive(Default)]
pub struct History {
    past: Vec<Vec<Track>>,
    future: Vec<Vec<Track>>,
}

impl History {
    pub fn apply(
        &mut self,
        project: &Project,
        tracks: Vec<Track>,
        revision: u64,
    ) -> Result<Project, String> {
        for locked in project.tracks.iter().filter(|t| t.locked) {
            if !tracks
                .iter()
                .any(|t| t.id == locked.id && t.clips == locked.clips)
            {
                return Err("Unlock the track before changing its clips".into());
            }
        }
        let next = replace_tracks(project, tracks, revision)?;
        self.past.push(project.tracks.clone());
        if self.past.len() > HISTORY_LIMIT {
            self.past.remove(0);
        }
        self.future.clear();
        Ok(next)
    }
    pub fn travel(&mut self, project: &Project, undo: bool) -> Result<Project, String> {
        let (from, to) = if undo {
            (&mut self.past, &mut self.future)
        } else {
            (&mut self.future, &mut self.past)
        };
        let tracks = from.last().ok_or("No history available")?.clone();
        let next = replace_tracks(project, tracks, project.revision)?;
        from.pop();
        to.push(project.tracks.clone());
        if to.len() > HISTORY_LIMIT {
            to.remove(0);
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_is_bounded_and_branching_clears_redo() {
        let mut project = crate::create_project("History".into(), 1920, 1080, 30).unwrap();
        let mut history = History::default();
        for _ in 0..60 {
            project = history.apply(&project, vec![], project.revision).unwrap();
        }
        assert_eq!(history.past.len(), HISTORY_LIMIT);
        for _ in 0..HISTORY_LIMIT {
            project = history.travel(&project, true).unwrap();
        }
        assert!(history.travel(&project, true).is_err());
        project = history.travel(&project, false).unwrap();
        project = history.apply(&project, vec![], project.revision).unwrap();
        assert!(history.travel(&project, false).is_err());
        assert!(history.apply(&project, vec![], 0).is_err());
        assert_eq!(history.past.len(), 2);
    }
}
